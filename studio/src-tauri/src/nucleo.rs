//! Núcleo do Studio, sem nada de janela: sessão aberta, coleta do painel a cada segundo,
//! verificação e a carga de demonstração. Testável com `cargo test`.

use serde::Serialize;
use siouxdb::armazenamento::{
    NumeroPagina, PAGINAS_RESERVADAS, ParametrosSenha, SenhaNova, TamanhoPagina, TipoPagina,
};
use siouxdb::metricas::{Coletor, FotoPainel, LimitesAlerta};
use siouxdb::transacoes::{Banco, Config, ErroBanco};
use siouxdb::vfs::{Vfs, VfsReal};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type Bd = Banco<<VfsReal as Vfs>::Arquivo>;

/// De onde vêm os dados do painel.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Origem {
    /// Arquivo aberto ou criado pela pessoa.
    Arquivo,
    /// Arquivo temporário com carga gerada pelo Studio.
    Demonstracao,
}

/// Dados do banco aberto.
#[derive(Debug, Clone, Serialize)]
pub struct Resumo {
    /// De onde vêm os dados.
    pub origem: Origem,
    /// Caminho do arquivo.
    pub caminho: String,
    /// Tamanho de página em KiB.
    pub pagina_kb: u64,
    /// O arquivo é cifrado?
    pub cifrado: bool,
    /// Total de páginas.
    pub paginas: u64,
    /// LSN da última confirmação.
    pub lsn: u64,
    /// LSN até onde o arquivo principal já recebeu o WAL.
    pub lsn_checkpoint: u64,
    /// Páginas esperando no WAL.
    pub paginas_wal: u64,
    /// `fsync`s do WAL e confirmações nesta abertura (mostra a confirmação em grupo).
    pub sincronizacoes: u64,
    /// Confirmações nesta abertura.
    pub confirmacoes: u64,
    /// Versão do motor.
    pub versao: &'static str,
}

/// Resultado de uma verificação completa.
#[derive(Debug, Clone, Serialize)]
pub struct Verificacao {
    /// Quando terminou (segundos desde 1970).
    pub quando: u64,
    /// Páginas lidas.
    pub paginas: u64,
    /// Problemas encontrados (vazio = tudo íntegro), inclusive os do livro-razão.
    pub problemas: Vec<String>,
    /// Confirmações conferidas no livro-razão.
    pub confirmacoes: u64,
    /// Âncora atual do razão (`lsn:hash`), para guardar fora do banco.
    pub ancora: Option<String>,
    /// Quanto demorou.
    pub duracao_ms: u64,
}

/// O que o painel recebe a cada segundo.
#[derive(Debug, Clone, Serialize)]
pub struct PainelResposta {
    /// Banco aberto.
    pub resumo: Resumo,
    /// Indicadores, séries e alertas.
    pub foto: FotoPainel,
    /// Limites de alerta em uso.
    pub limites: LimitesAlerta,
    /// Última verificação, se houve.
    pub verificacao: Option<Verificacao>,
}

struct Carga {
    parar: Arc<AtomicBool>,
    tarefas: Vec<JoinHandle<()>>,
}

impl Drop for Carga {
    fn drop(&mut self) {
        self.parar.store(true, Ordering::Relaxed);
        for t in self.tarefas.drain(..) {
            let _ = t.join();
        }
    }
}

struct Sessao {
    origem: Origem,
    caminho: PathBuf,
    banco: Arc<Bd>,
    coletor: Coletor,
    verificacao: Option<Verificacao>,
    carga: Option<Carga>,
}

impl Drop for Sessao {
    fn drop(&mut self) {
        // Para a carga antes de apagar o arquivo temporário.
        self.carga.take();
        if self.origem == Origem::Demonstracao {
            let _ = std::fs::remove_file(&self.caminho);
            let mut wal = self.caminho.as_os_str().to_owned();
            wal.push("-wal");
            let _ = std::fs::remove_file(PathBuf::from(wal));
        }
    }
}

/// Estado do Studio (um banco aberto por vez).
pub struct Studio {
    sessao: Mutex<Option<Sessao>>,
    limites: Mutex<LimitesAlerta>,
}

impl Studio {
    /// Studio sem banco aberto.
    pub fn novo() -> Arc<Self> {
        Arc::new(Studio {
            sessao: Mutex::new(None),
            limites: Mutex::new(LimitesAlerta::default()),
        })
    }

    /// Tarefa que alimenta o coletor uma vez por segundo enquanto o Studio existir.
    pub fn iniciar_coleta(studio: &Arc<Studio>) {
        let fraco = Arc::downgrade(studio);
        std::thread::Builder::new()
            .name("coleta-painel".into())
            .spawn(move || {
                while let Some(studio) = fraco.upgrade() {
                    studio.coletar();
                    drop(studio);
                    std::thread::sleep(Duration::from_secs(1));
                }
            })
            .expect("não foi possível iniciar a coleta do painel");
    }

    /// Registra uma leitura no coletor da sessão aberta.
    pub fn coletar(&self) {
        if let Some(s) = self.travar().as_mut() {
            let leitura = s.banco.leitura_metricas();
            s.coletor.registrar(leitura);
        }
    }

    fn travar(&self) -> MutexGuard<'_, Option<Sessao>> {
        self.sessao.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn limites(&self) -> LimitesAlerta {
        self.limites
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Abre um banco existente.
    pub fn abrir(&self, caminho: &str, senha: Option<&str>) -> Result<Resumo, String> {
        let caminho = caminho_valido(caminho)?;
        if !caminho.exists() {
            return Err(format!("o arquivo não existe: {}", caminho.display()));
        }
        // O anterior só é fechado se este abrir: um erro não derruba o que estava aberto.
        // (Se for o mesmo arquivo, a trava recusa: feche antes.)
        let senha = senha.filter(|s| !s.is_empty()).map(str::as_bytes);
        let b = Bd::abrir(&VfsReal::novo(), &caminho, senha, Config::default())
            .map_err(|e| e.to_string())?;
        Ok(self.instalar(Origem::Arquivo, caminho, Arc::new(b), None))
    }

    /// Cria um banco novo (recusa se já existir). Com `senha`, cifrado; `None` = em claro.
    pub fn criar(&self, caminho: &str, kb: u32, senha: Option<&str>) -> Result<Resumo, String> {
        let caminho = caminho_valido(caminho)?;
        let tamanho = TamanhoPagina::de_bytes(kb.saturating_mul(1024))
            .ok_or_else(|| format!("tamanho de página inválido: {kb} KiB (use 4, 8, 16 ou 32)"))?;
        if let Some(s) = senha
            && s.chars().count() < 8
        {
            return Err("a senha precisa de pelo menos 8 caracteres".into());
        }
        let senha = senha.map(|s| SenhaNova {
            senha: s.as_bytes(),
            parametros: ParametrosSenha::default(),
        });
        let b = Bd::criar(
            &VfsReal::novo(),
            &caminho,
            tamanho,
            senha,
            Config::default(),
        )
        .map_err(|e| e.to_string())?;
        Ok(self.instalar(Origem::Arquivo, caminho, Arc::new(b), None))
    }

    /// Cria um banco temporário cifrado e gera carga nele (3 escritores e 2 leitores ao
    /// mesmo tempo), para ver o painel funcionando.
    pub fn demonstracao(&self) -> Result<Resumo, String> {
        self.fechar();
        let caminho =
            std::env::temp_dir().join(format!("siouxdb-studio-demo-{}.sdb", std::process::id()));
        // O nome é só nosso (leva o PID): sobra de uma execução anterior pode ser apagada.
        let _ = std::fs::remove_file(&caminho);
        let mut wal = caminho.as_os_str().to_owned();
        wal.push("-wal");
        let _ = std::fs::remove_file(PathBuf::from(wal));
        let senha = format!("demo-{}-{:?}", std::process::id(), Instant::now());
        let senha = SenhaNova {
            senha: senha.as_bytes(),
            parametros: ParametrosSenha::para_testes(),
        };
        let config = Config {
            checkpoint_apos_paginas: 2000,
            ..Config::default()
        };
        let b = Arc::new(
            Bd::criar(
                &VfsReal::novo(),
                &caminho,
                TamanhoPagina::K4,
                Some(senha),
                config,
            )
            .map_err(|e| e.to_string())?,
        );
        let parar = Arc::new(AtomicBool::new(false));
        let vivas = Arc::new(Mutex::new(Vec::<NumeroPagina>::new()));
        let mut tarefas = Vec::new();
        for i in 0..5u64 {
            let (b, parar, vivas) = (Arc::clone(&b), Arc::clone(&parar), Arc::clone(&vivas));
            let nome = if i < 3 { "escritor" } else { "leitor" };
            tarefas.push(
                std::thread::Builder::new()
                    .name(format!("demo-{nome}-{i}"))
                    .spawn(move || {
                        if i < 3 {
                            escritor(&b, &parar, &vivas, i)
                        } else {
                            leitor(&b, &parar, &vivas, i)
                        }
                    })
                    .map_err(|e| e.to_string())?,
            );
        }
        let carga = Carga { parar, tarefas };
        Ok(self.instalar(Origem::Demonstracao, caminho, b, Some(carga)))
    }

    fn instalar(
        &self,
        origem: Origem,
        caminho: PathBuf,
        b: Arc<Bd>,
        carga: Option<Carga>,
    ) -> Resumo {
        let mut coletor = Coletor::novo(self.limites());
        coletor.registrar(b.leitura_metricas());
        let sessao = Sessao {
            origem,
            caminho,
            banco: b,
            coletor,
            verificacao: None,
            carga,
        };
        let resumo = resumo(&sessao);
        let anterior = self.travar().replace(sessao);
        drop(anterior); // fora da trava: pode parar a carga e apagar o temporário
        resumo
    }

    /// Fecha o banco aberto (e para a demonstração, apagando os arquivos temporários).
    pub fn fechar(&self) {
        let antiga = self.travar().take();
        drop(antiga);
    }

    /// Foto atual do painel, se houver banco aberto.
    pub fn painel(&self) -> Option<PainelResposta> {
        let limites = self.limites();
        self.travar().as_ref().map(|s| PainelResposta {
            resumo: resumo(s),
            foto: s.coletor.foto().clone(),
            limites,
            verificacao: s.verificacao.clone(),
        })
    }

    /// Lê e confere todas as páginas numa foto do banco (não para a carga).
    pub fn verificar(&self) -> Result<Verificacao, String> {
        let b = self
            .travar()
            .as_ref()
            .map(|s| Arc::clone(&s.banco))
            .ok_or("nenhum banco aberto")?;
        let inicio = Instant::now();
        let leitura = b.ler().map_err(|e| e.to_string())?;
        let total = leitura.meta().total_paginas;
        let mut problemas = Vec::new();
        for n in PAGINAS_RESERVADAS..total {
            if let Err(e) = leitura.pagina(n)
                && problemas.len() < 100
            {
                problemas.push(e.to_string());
            }
        }
        let razao = siouxdb::transacoes::razao::verificar(&leitura, None);
        problemas.extend(razao.problemas.iter().cloned());
        drop(leitura);
        let v = Verificacao {
            quando: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            paginas: total,
            problemas,
            confirmacoes: razao.confirmacoes,
            ancora: razao.ancora.map(|a| a.to_string()),
            duracao_ms: inicio.elapsed().as_millis() as u64,
        };
        if let Some(s) = self.travar().as_mut()
            && Arc::ptr_eq(&s.banco, &b)
        {
            s.verificacao = Some(v.clone());
        }
        Ok(v)
    }

    /// Troca os limites de alerta (para a sessão atual e as próximas).
    pub fn definir_limites(&self, limites: LimitesAlerta) {
        *self.limites.lock().unwrap_or_else(|e| e.into_inner()) = limites.clone();
        if let Some(s) = self.travar().as_mut() {
            s.coletor.definir_limites(limites);
        }
    }
}

fn caminho_valido(caminho: &str) -> Result<PathBuf, String> {
    let c = caminho.trim();
    if c.is_empty() {
        return Err("informe o caminho do arquivo".into());
    }
    Ok(Path::new(c).to_path_buf())
}

fn resumo(s: &Sessao) -> Resumo {
    let p = s.banco.paginador();
    let est = s.banco.estatisticas();
    let total = s.banco.ler().map(|l| l.meta().total_paginas).unwrap_or(0);
    Resumo {
        origem: s.origem.clone(),
        caminho: s.caminho.display().to_string(),
        pagina_kb: p.tamanho_pagina().bytes() as u64 / 1024,
        cifrado: p.cifrado(),
        paginas: total,
        lsn: est.lsn_escrito,
        lsn_checkpoint: est.lsn_checkpoint,
        paginas_wal: est.paginas_no_wal,
        sincronizacoes: est.sincronizacoes,
        confirmacoes: est.confirmacoes,
        versao: siouxdb::VERSAO,
    }
}

/// Gerador pseudoaleatório simples (xorshift), só para a demonstração.
struct Sorteio(u64);

impl Sorteio {
    fn proximo(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn ate(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.proximo() % n }
    }
}

/// Pausa da onda: ~40 s entre carga alta e baixa, com picos raros.
fn pausa(inicio: Instant, sorteio: &mut Sorteio) -> Duration {
    let t = inicio.elapsed().as_secs_f64();
    let onda = 0.5 + 0.5 * (t * std::f64::consts::TAU / 40.0).sin();
    if sorteio.ate(300) == 0 {
        return Duration::ZERO; // pico
    }
    Duration::from_micros((1_000.0 + 14_000.0 * (1.0 - onda)) as u64)
}

/// Escritor da demonstração: transações de 1 a 6 operações, banco limitado a ~16 MiB.
fn escritor(b: &Bd, parar: &AtomicBool, vivas: &Mutex<Vec<NumeroPagina>>, id: u64) {
    const LIMITE: usize = 4000;
    let mut sorteio = Sorteio(0x9E37_79B9_7F4A_7C15 ^ (id + 1).wrapping_mul(0xA24B_AED4_963E_E407));
    let inicio = Instant::now();
    while !parar.load(Ordering::Relaxed) {
        let Ok(mut tx) = b.escrever_esperando(Duration::from_secs(2)) else {
            continue;
        };
        let mut lista = vivas.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut ok = true;
        for _ in 0..1 + sorteio.ate(6) {
            let r = sorteio.ate(10);
            let res = if lista.len() < 64 || (r < 4 && lista.len() < LIMITE) {
                tx.alocar(TipoPagina::NoArvore).and_then(|n| {
                    let mut p = tx.pagina_nova(TipoPagina::NoArvore);
                    p.conteudo_mut()[..8].copy_from_slice(&sorteio.proximo().to_le_bytes());
                    lista.push(n);
                    tx.gravar(n, p)
                })
            } else if r < 9 {
                let n = lista[sorteio.ate(lista.len() as u64) as usize];
                let marca = sorteio.proximo().to_le_bytes();
                tx.pagina_mut(n)
                    .map(|p| p.conteudo_mut()[..8].copy_from_slice(&marca))
            } else {
                let i = sorteio.ate(lista.len() as u64) as usize;
                let n = lista.swap_remove(i);
                tx.liberar(n)
            };
            if res.is_err() {
                ok = false;
                break;
            }
        }
        if ok {
            // A lista compartilhada muda junto com a confirmação (ainda com a vez de escrever).
            *vivas.lock().unwrap_or_else(|e| e.into_inner()) = lista;
            if matches!(tx.confirmar(), Err(ErroBanco::Falhou)) {
                return;
            }
        } else {
            tx.desfazer();
        }
        std::thread::sleep(pausa(inicio, &mut sorteio));
    }
}

/// Leitor da demonstração: abre uma foto e lê páginas, a maioria das mais novas.
fn leitor(b: &Bd, parar: &AtomicBool, vivas: &Mutex<Vec<NumeroPagina>>, id: u64) {
    let mut sorteio = Sorteio(0xD1B5_4A32_D192_ED03 ^ id);
    let inicio = Instant::now();
    while !parar.load(Ordering::Relaxed) {
        let lista = vivas.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Ok(l) = b.ler()
            && !lista.is_empty()
        {
            let tam = lista.len() as u64;
            for _ in 0..20 {
                let i = if sorteio.ate(10) < 8 {
                    tam - 1 - sorteio.ate(tam.min(200))
                } else {
                    sorteio.ate(tam)
                };
                // Página criada depois da foto não existe nela: é esperado.
                let _ = l.pagina(lista[i as usize]);
            }
        }
        std::thread::sleep(pausa(inicio, &mut sorteio) / 2);
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    fn pasta() -> PathBuf {
        let d = std::env::temp_dir().join(format!("siouxdb-studio-teste-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn criar_abrir_verificar_e_painel() {
        let studio = Studio::novo();
        assert!(studio.painel().is_none());
        assert!(studio.verificar().is_err());
        let arq = pasta().join("criar.sdb");
        let _ = std::fs::remove_file(&arq);
        let _ = std::fs::remove_file(pasta().join("criar.sdb-wal"));
        let caminho = arq.display().to_string();
        assert!(studio.criar(&caminho, 3, None).is_err());
        assert!(studio.criar(&caminho, 16, Some("curta")).is_err());
        let r = studio.criar(&caminho, 16, Some("senha-de-teste")).unwrap();
        assert!(r.cifrado);
        assert_eq!((r.pagina_kb, r.paginas), (16, 2));
        assert!(
            studio.criar(&caminho, 16, None).is_err(),
            "não pode criar por cima"
        );
        assert!(studio.abrir("/nao/existe.sdb", None).is_err());
        assert!(
            studio.painel().is_some(),
            "erro ao abrir outro não fecha o atual"
        );
        studio.coletar();
        let v = studio.verificar().unwrap();
        assert!(v.problemas.is_empty());
        let painel = studio.painel().unwrap();
        assert_eq!(painel.resumo.origem, Origem::Arquivo);
        assert!(painel.verificacao.is_some());
        assert_eq!(painel.foto.leituras, 2);
        let json = serde_json::to_value(&painel).unwrap();
        assert_eq!(json["resumo"]["origem"], "arquivo");
        assert!(json["foto"]["series"]["operacoes_por_s"].is_array());

        // Com o banco aberto, abrir o mesmo arquivo de novo é recusado pela trava.
        let em_uso = studio.abrir(&caminho, Some("senha-de-teste")).unwrap_err();
        assert!(em_uso.contains("em uso"), "{em_uso}");
        studio.fechar();
        assert!(studio.painel().is_none());
        let sem = studio.abrir(&caminho, None).unwrap_err();
        assert!(sem.contains("senha"), "{sem}");
        assert!(studio.abrir(&caminho, Some("errada-123")).is_err());
        assert_eq!(
            studio
                .abrir(&caminho, Some("senha-de-teste"))
                .unwrap()
                .paginas,
            2
        );
        assert!(studio.abrir("  ", None).is_err());
        studio.fechar();
        std::fs::remove_file(&arq).unwrap();
        let _ = std::fs::remove_file(pasta().join("criar.sdb-wal"));
    }

    #[test]
    fn demonstracao_gera_carga_e_limpa_ao_fechar() {
        let studio = Studio::novo();
        let r = studio.demonstracao().unwrap();
        assert_eq!(r.origem, Origem::Demonstracao);
        assert!(r.cifrado);
        std::thread::sleep(Duration::from_millis(800));
        studio.coletar();
        let painel = studio.painel().unwrap();
        let foto = painel.foto;
        assert!(
            foto.indicadores.operacoes_por_s > 0.0,
            "{:?}",
            foto.indicadores
        );
        assert!(foto.indicadores.latencia_p99_us.is_some());
        assert!(painel.resumo.confirmacoes > 0);
        assert!(painel.resumo.sincronizacoes <= painel.resumo.confirmacoes);
        let v = studio.verificar().unwrap();
        assert!(v.problemas.is_empty(), "{:?}", v.problemas);
        studio.definir_limites(LimitesAlerta {
            latencia_p99_us: 0.001,
            ..LimitesAlerta::default()
        });
        std::thread::sleep(Duration::from_millis(200));
        studio.coletar();
        assert!(
            studio
                .painel()
                .unwrap()
                .foto
                .alertas
                .iter()
                .any(|a| a.indicador == "latencia_p99_us")
        );
        studio.fechar();
        assert!(
            !Path::new(&r.caminho).exists(),
            "arquivo temporário deveria ter sido apagado"
        );
        assert!(!Path::new(&format!("{}-wal", r.caminho)).exists());
    }
}
