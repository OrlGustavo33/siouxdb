//! O banco com transações.

use crate::fila::{FilaEscritor, Vez};
use crate::wal::{
    self, CabecalhoWal, Montador, TAMANHO_CABECALHO_WAL, TAMANHO_CONFIRMACAO, TAMANHO_QUADRO,
    TIPO_CONFIRMACAO, TIPO_PAGINA,
};
use crate::{ErroBanco, Resultado};
use siouxdb_armazenamento::{
    CacheSieve, Meta, NumeroPagina, PAGINAS_RESERVADAS, Pagina, Paginador, SenhaNova,
    TamanhoPagina, TipoPagina,
};
use siouxdb_metricas::{Histograma, Leitura as LeituraMetricas};
use siouxdb_vfs::{Arquivo, ModoAbertura, Vfs};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// No modo [`Sincronia::Normal`], páginas acumuladas antes de um `fsync` fora da vez.
const PAGINAS_POR_SINCRONIA: u64 = 256;

/// Quando a confirmação vira durável.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sincronia {
    /// Cada confirmação só retorna depois do `fsync` do WAL. Confirmações que chegam juntas
    /// dividem o mesmo `fsync` (confirmação em grupo). Nada confirmado se perde numa queda.
    #[default]
    Total,
    /// O `fsync` fica para o checkpoint. Mais rápido; numa queda podem sumir as últimas
    /// confirmações, mas o banco sempre volta num estado inteiro de alguma confirmação.
    Normal,
}

/// Configuração do banco.
#[derive(Debug, Clone)]
pub struct Config {
    /// Durabilidade das confirmações.
    pub sincronia: Sincronia,
    /// Checkpoint automático quando o WAL passar desta quantidade de páginas (0 = nunca).
    pub checkpoint_apos_paginas: u64,
    /// Tempo máximo esperando a vez de escrever.
    pub espera_maxima_escritor: Duration,
    /// Páginas do arquivo principal guardadas em memória. **0 = automático**: 1/8 da
    /// memória da máquina, entre 16 MiB e 512 MiB.
    pub cache_paginas: usize,
    /// Páginas do WAL guardadas em memória. 0 = automático (1/4 do cache principal).
    pub cache_wal: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            sincronia: Sincronia::Total,
            checkpoint_apos_paginas: 1000,
            espera_maxima_escritor: Duration::from_secs(5),
            cache_paginas: 0,
            cache_wal: 0,
        }
    }
}

/// Números do banco, para acompanhamento.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EstatisticasBanco {
    /// LSN da última confirmação gravada no WAL.
    pub lsn_escrito: u64,
    /// LSN até onde o WAL já foi para o disco (`fsync`).
    pub lsn_duravel: u64,
    /// LSN até onde o arquivo principal já recebeu as páginas.
    pub lsn_checkpoint: u64,
    /// Confirmações feitas nesta abertura.
    pub confirmacoes: u64,
    /// `fsync`s do WAL feitos nesta abertura (menos que confirmações = confirmação em grupo).
    pub sincronizacoes: u64,
    /// Checkpoints feitos nesta abertura.
    pub checkpoints: u64,
    /// Páginas no WAL desde o último reinício dele.
    pub paginas_no_wal: u64,
    /// Bytes do WAL em uso.
    pub bytes_wal: u64,
    /// Leituras (fotos) abertas agora.
    pub leituras_abertas: usize,
    /// Transações esperando a vez de escrever.
    pub fila_escritor: usize,
}

struct EstadoWal {
    cabecalho: CabecalhoWal,
    /// Para cada página, as versões no WAL: (LSN, posição do quadro), em ordem de LSN.
    indice: HashMap<NumeroPagina, Vec<(u64, u64)>>,
    /// Meta de cada confirmação no WAL, por LSN.
    metas: BTreeMap<u64, Meta>,
    /// Meta do arquivo principal (até o último checkpoint).
    base: Meta,
    fim: u64,
    crc: u32,
    paginas: u64,
    /// Páginas gravadas desde a última tentativa de checkpoint (gatilho do automático).
    desde_checkpoint: u64,
    /// Fotos abertas: LSN → quantas.
    leitores: BTreeMap<u64, usize>,
    falhou: bool,
}

impl EstadoWal {
    fn meta_em(&self, lsn: u64) -> Meta {
        self.metas
            .range(..=lsn)
            .next_back()
            .map(|(_, m)| *m)
            .unwrap_or(self.base)
    }
}

struct Duravel {
    lsn: u64,
    sincronizando: bool,
    falhou: bool,
}

/// Banco com transações sobre um arquivo (`banco.sdb`) e o WAL ao lado (`banco.sdb-wal`).
///
/// - **Um escritor por vez**, em fila justa (por ordem de chegada, com tempo máximo).
/// - **Leitores nunca esperam**: cada [`Leitura`] vê uma foto fixa do banco.
/// - **Confirmação em grupo**: várias confirmações dividem um `fsync`.
/// - **Checkpoint** copia as páginas do WAL para o arquivo principal, sem atrapalhar leitores.
pub struct Banco<A: Arquivo> {
    paginador: Paginador<A>,
    wal: A,
    estado: Mutex<EstadoWal>,
    duravel: Mutex<Duravel>,
    sinal_duravel: Condvar,
    lsn_escrito: AtomicU64,
    lsn_duravel: AtomicU64,
    lsn_checkpoint: AtomicU64,
    fila: FilaEscritor,
    config: Config,
    /// Cache de páginas do WAL por (sequência do WAL, posição): depois de um reinício as
    /// posições se repetem, a sequência não.
    cache_wal: Mutex<CacheSieve<(u64, u64), Arc<Pagina>>>,
    confirmacoes: AtomicU64,
    sincronizacoes: AtomicU64,
    checkpoints: AtomicU64,
    leituras_pagina: AtomicU64,
    quadros_gravados: AtomicU64,
    /// Páginas gravadas no WAL desde o último `fsync` (modo Normal).
    nao_sincronizadas: AtomicU64,
    acertos_wal: AtomicU64,
    faltas_wal: AtomicU64,
    latencia: Histograma,
}

/// Memória total da máquina em bytes (Linux; nos outros sistemas supõe 1 GiB).
fn memoria_total() -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            let linha = s.lines().find(|l| l.starts_with("MemTotal:"))?;
            linha.split_whitespace().nth(1)?.parse::<u64>().ok()
        })
        .map_or(1 << 30, |kib| kib * 1024)
}

/// Tamanho do cache no modo automático: 1/8 da memória, entre 16 MiB e 512 MiB.
pub fn cache_automatico_bytes() -> u64 {
    (memoria_total() / 8).clamp(16 << 20, 512 << 20)
}

fn caminho_wal(caminho: &Path) -> PathBuf {
    let mut s = caminho.as_os_str().to_owned();
    s.push("-wal");
    PathBuf::from(s)
}

fn sal_novo() -> Resultado<u64> {
    let mut b = [0u8; 8];
    siouxdb_cripto::bytes_aleatorios(&mut b)
        .map_err(siouxdb_armazenamento::ErroArmazenamento::from)?;
    Ok(u64::from_le_bytes(b))
}

impl<A: Arquivo> Banco<A> {
    /// Cria um banco novo (cifrado se `senha` vier).
    pub fn criar<V: Vfs<Arquivo = A>>(
        vfs: &V,
        caminho: &Path,
        tamanho: TamanhoPagina,
        senha: Option<SenhaNova<'_>>,
        config: Config,
    ) -> Resultado<Self> {
        let paginador =
            Paginador::criar_com(vfs, caminho, tamanho, config.cache_paginas.max(1), senha)?;
        Self::montar(vfs, caminho, paginador, config)
    }

    /// Abre um banco existente e recupera o que estiver confirmado no WAL.
    pub fn abrir<V: Vfs<Arquivo = A>>(
        vfs: &V,
        caminho: &Path,
        senha: Option<&[u8]>,
        config: Config,
    ) -> Resultado<Self> {
        let paginador = Paginador::abrir_com(vfs, caminho, config.cache_paginas.max(1), senha)?;
        Self::montar(vfs, caminho, paginador, config)
    }

    fn montar<V: Vfs<Arquivo = A>>(
        vfs: &V,
        caminho: &Path,
        paginador: Paginador<A>,
        config: Config,
    ) -> Resultado<Self> {
        let wal = vfs.abrir(&caminho_wal(caminho), ModoAbertura::CriarSeNaoExistir)?;
        let base = paginador.meta();
        let id = paginador.id_arquivo();
        let tam = paginador.tamanho_pagina().bytes() as u32;

        let mut b = [0u8; TAMANHO_CABECALHO_WAL as usize];
        let lidos = wal.ler_em(0, &mut b)?;
        let existente = (lidos == b.len())
            .then(|| CabecalhoWal::decodificar(&b))
            .flatten()
            .filter(|c| c.id_arquivo == id && c.tamanho_pagina == tam);

        let mut estado = EstadoWal {
            cabecalho: CabecalhoWal {
                tamanho_pagina: tam,
                id_arquivo: id,
                sal: 0,
                sequencia: 0,
            },
            indice: HashMap::new(),
            metas: BTreeMap::new(),
            base,
            fim: TAMANHO_CABECALHO_WAL,
            crc: 0,
            paginas: 0,
            desde_checkpoint: 0,
            leitores: BTreeMap::new(),
            falhou: false,
        };
        let mut escrito = base.ultimo_lsn;
        match existente {
            Some(cab) => {
                let v = wal::varrer(&wal, &paginador, cab.sal)?;
                estado.cabecalho = cab;
                for q in v.paginas {
                    // O que já foi para o arquivo principal num checkpoint não precisa do WAL.
                    if q.lsn > base.ultimo_lsn {
                        estado
                            .indice
                            .entry(q.numero)
                            .or_default()
                            .push((q.lsn, q.posicao));
                        estado.paginas += 1;
                    }
                }
                for m in v.confirmacoes {
                    if m.ultimo_lsn > base.ultimo_lsn {
                        escrito = escrito.max(m.ultimo_lsn);
                        estado.metas.insert(m.ultimo_lsn, m);
                    }
                }
                estado.fim = v.fim;
                estado.crc = v.crc;
                // Lixo depois da última confirmação (transação cortada pela queda) sai.
                wal.truncar(v.fim)?;
                // O que foi lido do disco passa a ser durável de verdade.
                wal.sincronizar()?;
            }
            None => {
                // WAL novo, vazio, de outro banco ou ilegível: começa do zero.
                estado.cabecalho.sal = sal_novo()?;
                wal.gravar_em(0, &estado.cabecalho.codificar())?;
                wal.truncar(TAMANHO_CABECALHO_WAL)?;
                wal.sincronizar()?;
            }
        }
        // Cache automático: só agora se sabe o tamanho da página.
        let cache_paginas = match config.cache_paginas {
            0 => (cache_automatico_bytes() / tam as u64) as usize,
            n => n,
        };
        paginador.definir_capacidade_cache(cache_paginas);
        let cache_wal = match config.cache_wal {
            0 => (cache_paginas / 4).max(16),
            n => n,
        };
        Ok(Banco {
            cache_wal: Mutex::new(CacheSieve::novo(cache_wal)),
            paginador,
            wal,
            estado: Mutex::new(estado),
            duravel: Mutex::new(Duravel {
                lsn: escrito,
                sincronizando: false,
                falhou: false,
            }),
            sinal_duravel: Condvar::new(),
            lsn_escrito: AtomicU64::new(escrito),
            lsn_duravel: AtomicU64::new(escrito),
            lsn_checkpoint: AtomicU64::new(base.ultimo_lsn),
            fila: FilaEscritor::default(),
            config,
            confirmacoes: AtomicU64::new(0),
            sincronizacoes: AtomicU64::new(0),
            checkpoints: AtomicU64::new(0),
            leituras_pagina: AtomicU64::new(0),
            quadros_gravados: AtomicU64::new(0),
            nao_sincronizadas: AtomicU64::new(0),
            acertos_wal: AtomicU64::new(0),
            faltas_wal: AtomicU64::new(0),
            latencia: Histograma::novo(),
        })
    }

    fn travar(&self) -> MutexGuard<'_, EstadoWal> {
        self.estado.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn travar_duravel(&self) -> MutexGuard<'_, Duravel> {
        self.duravel.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// O paginador do arquivo principal (tamanho de página, cifra, estatísticas).
    pub fn paginador(&self) -> &Paginador<A> {
        &self.paginador
    }

    /// Configuração em uso.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// LSN que uma leitura nova enxerga.
    fn lsn_visivel(&self) -> u64 {
        match self.config.sincronia {
            Sincronia::Total => self.lsn_duravel.load(Ordering::SeqCst),
            Sincronia::Normal => self.lsn_escrito.load(Ordering::SeqCst),
        }
    }

    /// Abre uma leitura: uma foto do banco que não muda enquanto ela existir. Não espera
    /// ninguém e não bloqueia escritores.
    pub fn ler(&self) -> Resultado<Leitura<'_, A>> {
        let mut e = self.travar();
        if e.falhou {
            return Err(ErroBanco::Falhou);
        }
        let lsn = self.lsn_visivel();
        *e.leitores.entry(lsn).or_insert(0) += 1;
        let meta = e.meta_em(lsn);
        Ok(Leitura {
            banco: self,
            lsn,
            meta,
        })
    }

    /// Começa uma transação de escrita. Espera a vez (fila justa) até
    /// [`Config::espera_maxima_escritor`].
    pub fn escrever(&self) -> Resultado<Transacao<'_, A>> {
        self.escrever_esperando(self.config.espera_maxima_escritor)
    }

    /// Como [`Banco::escrever`], com outro tempo máximo de espera.
    pub fn escrever_esperando(&self, espera: Duration) -> Resultado<Transacao<'_, A>> {
        let vez = self.fila.adquirir(espera)?;
        let e = self.travar();
        if e.falhou {
            return Err(ErroBanco::Falhou);
        }
        let base_lsn = self.lsn_escrito.load(Ordering::SeqCst);
        let meta = e.meta_em(base_lsn);
        drop(e);
        Ok(Transacao {
            banco: self,
            vez: Some(vez),
            base_lsn,
            meta,
            meta_inicial: meta,
            sujas: BTreeMap::new(),
        })
    }

    /// Lê a página `numero` como estava na confirmação `lsn`.
    pub(crate) fn pagina_em(
        &self,
        lsn: u64,
        meta: &Meta,
        numero: NumeroPagina,
    ) -> Resultado<Arc<Pagina>> {
        if numero < PAGINAS_RESERVADAS || numero >= meta.total_paginas {
            return Err(ErroBanco::PaginaInexistente(numero));
        }
        let inicio = Instant::now();
        self.leituras_pagina.fetch_add(1, Ordering::Relaxed);
        let r = loop {
            let (posicao, sequencia) = {
                let e = self.travar();
                let pos = e
                    .indice
                    .get(&numero)
                    .and_then(|v| v.iter().rev().find(|(l, _)| *l <= lsn).map(|(_, p)| *p));
                (pos, e.cabecalho.sequencia)
            };
            let Some(pos) = posicao else {
                break self.paginador.ler(numero).map_err(ErroBanco::from);
            };
            let r = self.pagina_do_wal(numero, sequencia, pos);
            // Se o WAL reiniciou durante a leitura, o quadro pode ser outro: tenta de novo
            // (agora a página está no arquivo principal). Só reinicia quando todas as fotos
            // abertas já enxergam o checkpoint, então a nova tentativa é correta.
            if self.travar().cabecalho.sequencia == sequencia {
                break r;
            }
        };
        self.latencia.registrar_duracao(inicio.elapsed());
        r
    }

    fn pagina_do_wal(
        &self,
        numero: NumeroPagina,
        sequencia: u64,
        pos: u64,
    ) -> Resultado<Arc<Pagina>> {
        if let Some(p) = self
            .cache_wal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .obter(&(sequencia, pos))
        {
            self.acertos_wal.fetch_add(1, Ordering::Relaxed);
            return Ok(p);
        }
        self.faltas_wal.fetch_add(1, Ordering::Relaxed);
        let bytes = self.ler_quadro(pos)?;
        let p = Arc::new(self.paginador.do_disco(numero, bytes)?);
        self.cache_wal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .inserir((sequencia, pos), Arc::clone(&p));
        Ok(p)
    }

    fn ler_quadro(&self, pos: u64) -> Resultado<Box<[u8]>> {
        let mut bytes = vec![0u8; self.paginador.tamanho_pagina().bytes()].into_boxed_slice();
        self.wal
            .ler_exato_em(pos + TAMANHO_QUADRO as u64, &mut bytes)?;
        Ok(bytes)
    }

    fn soltar_leitura(&self, lsn: u64) {
        let mut e = self.travar();
        if let Some(n) = e.leitores.get_mut(&lsn) {
            *n -= 1;
            if *n == 0 {
                e.leitores.remove(&lsn);
            }
        }
    }

    /// Espera o WAL estar no disco até `lsn`. Quem chega primeiro faz o `fsync` por todos
    /// os que já gravaram; os outros só esperam (confirmação em grupo).
    fn esperar_duravel(&self, lsn: u64) -> Resultado<()> {
        let mut d = self.travar_duravel();
        loop {
            if d.falhou {
                return Err(ErroBanco::Falhou);
            }
            if d.lsn >= lsn {
                return Ok(());
            }
            if d.sincronizando {
                d = self
                    .sinal_duravel
                    .wait(d)
                    .unwrap_or_else(|p| p.into_inner());
                continue;
            }
            d.sincronizando = true;
            let alvo = self.lsn_escrito.load(Ordering::SeqCst);
            drop(d);
            let r = self.wal.sincronizar();
            self.sincronizacoes.fetch_add(1, Ordering::Relaxed);
            d = self.travar_duravel();
            d.sincronizando = false;
            match r {
                Ok(()) => {
                    d.lsn = d.lsn.max(alvo);
                    self.lsn_duravel.fetch_max(alvo, Ordering::SeqCst);
                }
                Err(_) => {
                    d.falhou = true;
                    self.travar().falhou = true;
                }
            }
            self.sinal_duravel.notify_all();
        }
    }

    /// Copia para o arquivo principal tudo o que nenhuma leitura aberta ainda precisa ver
    /// no WAL e, se não sobrar leitura nenhuma, reinicia o WAL. Espera a vez de escrever.
    pub fn checkpoint(&self) -> Resultado<()> {
        let vez = self.fila.adquirir(self.config.espera_maxima_escritor)?;
        self.checkpoint_com_vez(&vez, false)
    }

    fn checkpoint_com_vez(&self, _vez: &Vez<'_>, parar_antes_de_confirmar: bool) -> Resultado<()> {
        // O que vai para o arquivo principal precisa estar no WAL em disco antes.
        self.esperar_duravel(self.lsn_escrito.load(Ordering::SeqCst))?;
        let (alvo, copias, meta_alvo) = {
            let mut e = self.travar();
            e.desde_checkpoint = 0;
            if e.falhou {
                return Err(ErroBanco::Falhou);
            }
            let ja = self.lsn_checkpoint.load(Ordering::SeqCst);
            let menor_leitura = e.leitores.keys().next().copied().unwrap_or(u64::MAX);
            let alvo = self.lsn_duravel.load(Ordering::SeqCst).min(menor_leitura);
            let mut copias: Vec<(NumeroPagina, u64)> = e
                .indice
                .iter()
                .filter_map(|(n, versoes)| {
                    versoes
                        .iter()
                        .rev()
                        .find(|(l, _)| *l <= alvo && *l > ja)
                        .map(|(_, p)| (*n, *p))
                })
                .collect();
            copias.sort_unstable();
            (alvo.max(ja), copias, e.meta_em(alvo.max(ja)))
        };
        if alvo > self.lsn_checkpoint.load(Ordering::SeqCst) {
            for (n, pos) in copias {
                let bytes = self.ler_quadro(pos)?;
                self.paginador.gravar_do_disco(n, &bytes)?;
            }
            if parar_antes_de_confirmar {
                return Ok(());
            }
            self.paginador.definir_meta(meta_alvo)?;
            self.paginador.confirmar()?;
            self.lsn_checkpoint.store(alvo, Ordering::SeqCst);
            self.travar().base = meta_alvo;
            self.checkpoints.fetch_add(1, Ordering::Relaxed);
        }
        self.reiniciar_wal_se_possivel()
    }

    /// Com tudo no arquivo principal e nenhuma leitura aberta, o WAL volta a ficar vazio.
    fn reiniciar_wal_se_possivel(&self) -> Resultado<()> {
        let mut e = self.travar();
        let tudo_copiado =
            self.lsn_checkpoint.load(Ordering::SeqCst) == self.lsn_escrito.load(Ordering::SeqCst);
        // Fotos abertas no LSN do checkpoint não precisam mais do WAL (está tudo no arquivo
        // principal); só uma foto mais antiga impede o reinício.
        let ck = self.lsn_checkpoint.load(Ordering::SeqCst);
        let fotos_em_dia = e.leitores.keys().next().is_none_or(|&menor| menor >= ck);
        if !tudo_copiado || !fotos_em_dia || e.fim == TAMANHO_CABECALHO_WAL {
            return Ok(());
        }
        e.cabecalho.sal = sal_novo()?;
        e.cabecalho.sequencia += 1;
        let r = self
            .wal
            .gravar_em(0, &e.cabecalho.codificar())
            .and_then(|_| self.wal.truncar(TAMANHO_CABECALHO_WAL))
            .and_then(|_| self.wal.sincronizar());
        if r.is_err() {
            e.falhou = true;
        }
        r?;
        e.indice.clear();
        e.metas.clear();
        e.fim = TAMANHO_CABECALHO_WAL;
        e.crc = 0;
        e.paginas = 0;
        self.cache_wal
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .limpar();
        Ok(())
    }

    /// Cópia de segurança **a quente** para `destino` (e `destino-wal`), consistente na
    /// última confirmação. Os leitores continuam; os escritores esperam só durante a cópia.
    /// A cópia é gravada com nome temporário, sincronizada e renomeada no fim: ou existe
    /// inteira, ou não existe. Abre com a mesma senha. Devolve os bytes copiados.
    pub fn copiar_para<V: Vfs<Arquivo = A>>(&self, vfs: &V, destino: &Path) -> Resultado<u64> {
        if vfs.existe(destino)? {
            return Err(siouxdb_vfs::ErroVfs::JaExiste(destino.to_path_buf()).into());
        }
        let vez = self.fila.adquirir(self.config.espera_maxima_escritor)?;
        // Checkpoint antes, para o WAL da cópia ficar pequeno. Com a vez de escrever na mão,
        // nada muda o arquivo principal nem o WAL enquanto copiamos.
        self.checkpoint_com_vez(&vez, false)?;
        let fim_wal = self.travar().fim;
        let temporario = |sufixo: &str| {
            let mut s = destino.as_os_str().to_owned();
            s.push(sufixo);
            PathBuf::from(s)
        };
        let (tmp, tmp_wal, final_wal) = (
            temporario(".parcial"),
            temporario("-wal.parcial"),
            temporario("-wal"),
        );
        let copiar = |ler: &dyn Fn(u64, &mut [u8]) -> Resultado<usize>,
                      tamanho: u64,
                      para: &Path|
         -> Resultado<u64> {
            let saida = vfs.abrir(para, ModoAbertura::CriarNovo)?;
            let mut buf = vec![0u8; 1 << 20];
            let mut pos = 0u64;
            while pos < tamanho {
                let n = (tamanho - pos).min(buf.len() as u64) as usize;
                let lidos = ler(pos, &mut buf[..n])?;
                if lidos == 0 {
                    break;
                }
                saida.gravar_em(pos, &buf[..lidos])?;
                pos += lidos as u64;
            }
            saida.sincronizar()?;
            Ok(pos)
        };
        let principal = copiar(
            &|pos, b| Ok(self.paginador.ler_bruto(pos, b)?),
            self.paginador.tamanho_arquivo()?,
            &tmp,
        )?;
        let wal = copiar(&|pos, b| Ok(self.wal.ler_em(pos, b)?), fim_wal, &tmp_wal)?;
        drop(vez);
        // O WAL entra primeiro: um principal sem o WAL dele seria uma cópia antiga.
        vfs.renomear(&tmp_wal, &final_wal)?;
        vfs.renomear(&tmp, destino)?;
        Ok(principal + wal)
    }

    /// Faz o checkpoint completo e fecha.
    pub fn fechar(self) -> Resultado<()> {
        self.checkpoint()
    }

    /// Números para acompanhamento.
    pub fn estatisticas(&self) -> EstatisticasBanco {
        let e = self.travar();
        EstatisticasBanco {
            lsn_escrito: self.lsn_escrito.load(Ordering::Relaxed),
            lsn_duravel: self.lsn_duravel.load(Ordering::Relaxed),
            lsn_checkpoint: self.lsn_checkpoint.load(Ordering::Relaxed),
            confirmacoes: self.confirmacoes.load(Ordering::Relaxed),
            sincronizacoes: self.sincronizacoes.load(Ordering::Relaxed),
            checkpoints: self.checkpoints.load(Ordering::Relaxed),
            paginas_no_wal: e.paginas,
            bytes_wal: e.fim,
            leituras_abertas: e.leitores.values().sum(),
            fila_escritor: self.fila.esperando(),
        }
    }

    /// Leitura para o painel ([`siouxdb_metricas::Coletor`]). "Conexões" no modo embutido
    /// são as sessões abertas agora (leituras + o escritor).
    pub fn leitura_metricas(&self) -> LeituraMetricas {
        let est = self.estatisticas();
        let pag = self.paginador.estatisticas();
        let meta = self.travar().meta_em(est.lsn_escrito);
        LeituraMetricas {
            instante: Instant::now(),
            conexoes_ativas: est.leituras_abertas as u64 + u64::from(self.fila.ocupado()),
            conexoes_limite: 0,
            operacoes: self.leituras_pagina.load(Ordering::Relaxed) + est.confirmacoes,
            acertos_cache: pag.cache.acertos + self.acertos_wal.load(Ordering::Relaxed),
            faltas_cache: pag.cache.faltas + self.faltas_wal.load(Ordering::Relaxed),
            gravacoes: self.quadros_gravados.load(Ordering::Relaxed) + pag.gravacoes,
            confirmacoes: est.confirmacoes,
            bytes_banco: meta.total_paginas * pag.tamanho_pagina as u64 + est.bytes_wal,
            fila_escritor: est.fila_escritor as u64,
            latencia: self.latencia.foto(),
        }
    }
}

/// Uma foto do banco para leitura. Enquanto existir, o banco que ela vê não muda.
pub struct Leitura<'a, A: Arquivo> {
    banco: &'a Banco<A>,
    lsn: u64,
    meta: Meta,
}

impl<A: Arquivo> Leitura<'_, A> {
    /// LSN da confirmação que esta foto mostra.
    pub fn lsn(&self) -> u64 {
        self.lsn
    }

    /// Estado lógico nesta foto (total de páginas, raízes...).
    pub fn meta(&self) -> Meta {
        self.meta
    }

    /// Lê uma página.
    pub fn pagina(&self, numero: NumeroPagina) -> Resultado<Arc<Pagina>> {
        self.banco.pagina_em(self.lsn, &self.meta, numero)
    }

    /// Bytes de conteúdo de cada página deste banco.
    pub fn tamanho_conteudo(&self) -> usize {
        self.banco.paginador.tamanho_util() - siouxdb_armazenamento::CABECALHO_PAGINA
    }
}

impl<A: Arquivo> Drop for Leitura<'_, A> {
    fn drop(&mut self) {
        self.banco.soltar_leitura(self.lsn);
    }
}

/// Transação de escrita. Só uma por vez no banco. Sem [`Transacao::confirmar`], nada do que
/// ela fez aparece (soltar = desfazer).
pub struct Transacao<'a, A: Arquivo> {
    pub(crate) banco: &'a Banco<A>,
    vez: Option<Vez<'a>>,
    pub(crate) base_lsn: u64,
    pub(crate) meta: Meta,
    pub(crate) meta_inicial: Meta,
    pub(crate) sujas: BTreeMap<NumeroPagina, Arc<Pagina>>,
}

impl<A: Arquivo> Transacao<'_, A> {
    /// Estado lógico como esta transação vê (com as mudanças dela).
    pub fn meta(&self) -> Meta {
        self.meta
    }

    /// Página zerada do tamanho certo para este banco.
    pub fn pagina_nova(&self, tipo: TipoPagina) -> Pagina {
        self.banco.paginador.pagina_nova(tipo)
    }

    /// LSN que esta transação vai receber ao confirmar.
    pub fn lsn(&self) -> u64 {
        self.base_lsn + 1
    }

    /// Bytes de conteúdo de cada página deste banco.
    pub fn tamanho_conteudo(&self) -> usize {
        self.banco.paginador.tamanho_util() - siouxdb_armazenamento::CABECALHO_PAGINA
    }

    /// Lê uma página como esta transação vê (com as mudanças dela). Não copia a página.
    pub fn pagina(&self, numero: NumeroPagina) -> Resultado<Arc<Pagina>> {
        if let Some(p) = self.sujas.get(&numero) {
            return Ok(Arc::clone(p));
        }
        self.banco.pagina_em(self.base_lsn, &self.meta, numero)
    }

    /// Página para alterar no lugar (traz para a transação se ainda não estiver; a cópia
    /// acontece uma vez por página por transação).
    pub fn pagina_mut(&mut self, numero: NumeroPagina) -> Resultado<&mut Pagina> {
        if !self.sujas.contains_key(&numero) {
            let p = self.banco.pagina_em(self.base_lsn, &self.meta, numero)?;
            self.sujas.insert(numero, p);
        }
        Ok(Arc::make_mut(
            self.sujas.get_mut(&numero).expect("inserida acima"),
        ))
    }

    /// Troca a página inteira.
    pub fn gravar(&mut self, numero: NumeroPagina, pagina: Pagina) -> Resultado<()> {
        if numero < PAGINAS_RESERVADAS || numero >= self.meta.total_paginas {
            return Err(ErroBanco::PaginaInexistente(numero));
        }
        let util = self.banco.paginador.tamanho_util();
        if pagina.tamanho() != util {
            return Err(
                siouxdb_armazenamento::ErroArmazenamento::TamanhoDeBufferErrado {
                    esperado: util,
                    recebido: pagina.tamanho(),
                }
                .into(),
            );
        }
        self.sujas.insert(numero, Arc::new(pagina));
        Ok(())
    }

    /// Reserva uma página (reaproveita uma livre ou aumenta o banco) e devolve o número.
    pub fn alocar(&mut self, tipo: TipoPagina) -> Resultado<NumeroPagina> {
        let n = if self.meta.primeira_livre != 0 {
            let n = self.meta.primeira_livre;
            let livre = self.pagina(n)?;
            if livre.tipo() != TipoPagina::Livre {
                // Lista de livres apontando para página em uso: nunca entrega.
                return Err(ErroBanco::PaginaJaLivre(n));
            }
            self.meta.primeira_livre =
                u64::from_le_bytes(livre.conteudo()[..8].try_into().expect("8"));
            n
        } else {
            let n = self.meta.total_paginas;
            self.meta.total_paginas += 1;
            n
        };
        self.sujas.insert(n, Arc::new(self.pagina_nova(tipo)));
        Ok(n)
    }

    /// Devolve uma página para a lista de livres.
    pub fn liberar(&mut self, numero: NumeroPagina) -> Resultado<()> {
        if self.pagina(numero)?.tipo() == TipoPagina::Livre {
            return Err(ErroBanco::PaginaJaLivre(numero));
        }
        let mut livre = self.pagina_nova(TipoPagina::Livre);
        livre.conteudo_mut()[..8].copy_from_slice(&self.meta.primeira_livre.to_le_bytes());
        self.sujas.insert(numero, Arc::new(livre));
        self.meta.primeira_livre = numero;
        Ok(())
    }

    /// Define a raiz do catálogo.
    pub fn definir_raiz_catalogo(&mut self, n: NumeroPagina) {
        self.meta.raiz_catalogo = n;
    }

    /// Define a raiz do livro-razão.
    pub fn definir_raiz_razao(&mut self, n: NumeroPagina) {
        self.meta.raiz_razao = n;
    }

    /// Confirma. Devolve o LSN da confirmação. Com [`Sincronia::Total`], só retorna depois
    /// de o WAL estar no disco.
    pub fn confirmar(mut self) -> Resultado<u64> {
        let banco = self.banco;
        if self.sujas.is_empty() && self.meta == self.meta_inicial {
            return Ok(self.base_lsn);
        }
        let inicio = Instant::now();
        crate::razao::registrar(&mut self)?;
        let lsn = self.base_lsn + 1;
        self.meta.ultimo_lsn = lsn;
        let tam = banco.paginador.tamanho_pagina().bytes();
        let mut e = banco.travar();
        if e.falhou {
            return Err(ErroBanco::Falhou);
        }
        let sal = e.cabecalho.sal;
        let sequencia = e.cabecalho.sequencia;
        let mut m = Montador::novo(
            e.crc,
            self.sujas.len() * (TAMANHO_QUADRO + tam) + TAMANHO_QUADRO + TAMANHO_CONFIRMACAO,
        );
        // A cabeça do razão é a mesma página reescrita a cada confirmação (o checkpoint a copia
        // uma vez só): não conta para o ritmo de fsync e de checkpoint.
        let contadas =
            self.sujas.len() as u64 - u64::from(self.sujas.contains_key(&self.meta.raiz_razao));
        let mut novas = Vec::with_capacity(self.sujas.len());
        for (n, p) in std::mem::take(&mut self.sujas) {
            let mut p = Arc::try_unwrap(p).unwrap_or_else(|compartilhada| (*compartilhada).clone());
            p.definir_lsn(lsn);
            let disco = banco.paginador.para_disco(n, &mut p)?;
            let pos = m.quadro(&wal::campos_quadro(TIPO_PAGINA, n, lsn, sal), &disco);
            novas.push((n, e.fim + pos as u64, p));
        }
        let campos = wal::campos_quadro(TIPO_CONFIRMACAO, 0, lsn, sal);
        let conteudo = wal::conteudo_confirmacao(
            &banco.paginador,
            &campos,
            &self.meta,
            novas.len() as u32,
            m.crc,
        )?;
        m.quadro(&campos, &conteudo);
        if let Err(erro) = banco.wal.gravar_em(e.fim, &m.bytes) {
            e.falhou = true;
            return Err(erro.into());
        }
        e.fim += m.bytes.len() as u64;
        e.crc = m.crc;
        e.paginas += novas.len() as u64;
        e.desde_checkpoint += contadas;
        let novas_paginas = contadas;
        banco
            .quadros_gravados
            .fetch_add(novas.len() as u64, Ordering::Relaxed);
        {
            let mut cache = banco.cache_wal.lock().unwrap_or_else(|p| p.into_inner());
            for (n, pos, p) in novas {
                e.indice.entry(n).or_default().push((lsn, pos));
                cache.inserir((sequencia, pos), Arc::new(p));
            }
        }
        e.metas.insert(lsn, self.meta);
        banco.lsn_escrito.store(lsn, Ordering::SeqCst);
        let limite = banco.config.checkpoint_apos_paginas;
        let checkpoint_agora = limite > 0 && e.desde_checkpoint >= limite;
        drop(e);
        let vez = self.vez.take();
        if checkpoint_agora {
            if let Some(vez) = &vez {
                // Checkpoint automático na vez de quem confirmou: roda mesmo com fila cheia, e o
                // WAL não cresce sem limite sob carga. A confirmação já está no WAL; falha aqui
                // não a desfaz (se o arquivo principal ficou incerto, o paginador passa a recusar
                // operações e a próxima chamada mostra o erro).
                let _ = banco.checkpoint_com_vez(vez, false);
            }
        }
        // Solta a vez antes do fsync: o próximo escritor já grava enquanto este espera o
        // disco, e o mesmo fsync serve aos dois.
        drop(vez);
        match banco.config.sincronia {
            Sincronia::Total => banco.esperar_duravel(lsn)?,
            Sincronia::Normal => {
                // Sem fsync por confirmação, mas também sem deixar acumular: a cada
                // PAGINAS_POR_SINCRONIA quem confirmou faz um fsync fora da vez. Evita um
                // fsync gigante (e um pico de segundos) no checkpoint.
                let acumuladas = banco
                    .nao_sincronizadas
                    .fetch_add(novas_paginas, Ordering::Relaxed)
                    + novas_paginas;
                if acumuladas >= PAGINAS_POR_SINCRONIA {
                    banco.nao_sincronizadas.store(0, Ordering::Relaxed);
                    banco.esperar_duravel(lsn)?;
                }
            }
        }
        banco.confirmacoes.fetch_add(1, Ordering::Relaxed);
        banco.latencia.registrar_duracao(inicio.elapsed());
        Ok(lsn)
    }

    /// Desfaz tudo (o mesmo que soltar a transação).
    pub fn desfazer(self) {}
}

#[cfg(test)]
impl<A: Arquivo> Banco<A> {
    /// Copia páginas para o arquivo principal e para antes de confirmar (para testar queda
    /// no meio do checkpoint).
    pub(crate) fn checkpoint_pela_metade(&self) -> Resultado<()> {
        let vez = self.fila.adquirir(self.config.espera_maxima_escritor)?;
        self.checkpoint_com_vez(&vez, true)
    }
}

/// Quem sabe ler páginas: uma [`Leitura`] (foto) ou uma [`Transacao`] (com as mudanças dela).
/// As estruturas de cima (árvores, tabelas, catálogo) leem só por aqui.
pub trait Paginas {
    /// Lê uma página, sem copiar.
    fn pagina(&self, numero: NumeroPagina) -> Resultado<Arc<Pagina>>;
    /// Estado lógico (total de páginas, raízes).
    fn meta(&self) -> Meta;
    /// Bytes de conteúdo de cada página.
    fn tamanho_conteudo(&self) -> usize;
}

/// Quem também sabe alterar páginas: uma [`Transacao`].
pub trait PaginasMut: Paginas {
    /// Página para alterar no lugar.
    fn pagina_mut(&mut self, numero: NumeroPagina) -> Resultado<&mut Pagina>;
    /// Reserva uma página zerada do tipo dado.
    fn alocar(&mut self, tipo: TipoPagina) -> Resultado<NumeroPagina>;
    /// Devolve uma página para a lista de livres.
    fn liberar(&mut self, numero: NumeroPagina) -> Resultado<()>;
    /// LSN que as mudanças vão receber ao confirmar.
    fn lsn(&self) -> u64;
    /// Define a raiz do catálogo.
    fn definir_raiz_catalogo(&mut self, numero: NumeroPagina);
}

impl<A: Arquivo> Paginas for Leitura<'_, A> {
    fn pagina(&self, numero: NumeroPagina) -> Resultado<Arc<Pagina>> {
        Leitura::pagina(self, numero)
    }
    fn meta(&self) -> Meta {
        Leitura::meta(self)
    }
    fn tamanho_conteudo(&self) -> usize {
        Leitura::tamanho_conteudo(self)
    }
}

impl<A: Arquivo> Paginas for Transacao<'_, A> {
    fn pagina(&self, numero: NumeroPagina) -> Resultado<Arc<Pagina>> {
        Transacao::pagina(self, numero)
    }
    fn meta(&self) -> Meta {
        Transacao::meta(self)
    }
    fn tamanho_conteudo(&self) -> usize {
        Transacao::tamanho_conteudo(self)
    }
}

impl<A: Arquivo> PaginasMut for Transacao<'_, A> {
    fn pagina_mut(&mut self, numero: NumeroPagina) -> Resultado<&mut Pagina> {
        Transacao::pagina_mut(self, numero)
    }
    fn alocar(&mut self, tipo: TipoPagina) -> Resultado<NumeroPagina> {
        Transacao::alocar(self, tipo)
    }
    fn liberar(&mut self, numero: NumeroPagina) -> Resultado<()> {
        Transacao::liberar(self, numero)
    }
    fn lsn(&self) -> u64 {
        Transacao::lsn(self)
    }
    fn definir_raiz_catalogo(&mut self, numero: NumeroPagina) {
        Transacao::definir_raiz_catalogo(self, numero);
    }
}
