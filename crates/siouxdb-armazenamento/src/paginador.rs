//! Paginador: cria e abre o arquivo, aloca, lê, grava e confirma páginas.
//!
//! # O que é garantido nesta camada
//!
//! - **Nunca aceita lixo**: toda página lida passa por verificação (CRC32C) e conferência
//!   de número; gravação pela metade ou página trocada vira erro, nunca dado errado.
//! - **O cabeçalho sempre abre**: ele é gravado em cópias alternadas; uma queda no meio
//!   deixa valendo a cópia anterior.
//! - **Confirmação atômica para quem não sobrescreve página viva**: o que foi alocado
//!   depois da última confirmação simplesmente não existe depois de uma queda, e páginas
//!   liberadas só voltam a ser entregues **depois** de a confirmação seguinte estar em
//!   disco. Então quem altera copiando (aloca nova, grava, troca a raiz, libera a velha)
//!   tem "tudo ou nada" a cada [`Paginador::confirmar`].
//!
//! Sobrescrever uma página já confirmada **não** é atômico aqui: depois de uma queda ela
//! pode estar antiga, nova ou ilegível (erro). Essa garantia vem do WAL (marco M3).
//!
//! A lista de páginas livres se conserta sozinha: se uma queda a deixar inconsistente, ela
//! é cortada no ponto do problema. Perde-se espaço (recuperável por compactação), mas uma
//! página em uso nunca é entregue como livre.

use crate::{
    Cabecalho, CacheSieve, ErroArmazenamento, EstatisticasCache, NumeroPagina, PAGINAS_RESERVADAS,
    Pagina, Resultado, TAMANHO_CABECALHO, TamanhoPagina, TipoPagina,
};
use siouxdb_vfs::{Arquivo, ModoAbertura, Vfs};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// Números do paginador, para acompanhamento.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EstatisticasPaginador {
    /// Tamanho das páginas em bytes.
    pub tamanho_pagina: usize,
    /// Total de páginas do arquivo (contando as reservadas).
    pub total_paginas: NumeroPagina,
    /// Geração do cabeçalho (quantas confirmações o arquivo já teve, mais uma).
    pub geracao: u64,
    /// Páginas lidas do disco (faltas de cache que viraram leitura).
    pub leituras_de_disco: u64,
    /// Páginas gravadas.
    pub gravacoes: u64,
    /// Confirmações concluídas.
    pub confirmacoes: u64,
    /// Páginas liberadas esperando a próxima confirmação.
    pub liberadas_pendentes: usize,
    /// Contadores do cache de páginas.
    pub cache: EstatisticasCache,
}

struct Estado {
    cabecalho: Cabecalho,
    /// Liberadas depois da última confirmação: só entram na lista de livres depois da próxima.
    liberadas: Vec<NumeroPagina>,
    falhou: bool,
}

/// Dono do arquivo do banco. Pode ser compartilhado entre threads.
pub struct Paginador<A: Arquivo> {
    arquivo: A,
    tamanho: TamanhoPagina,
    estado: Mutex<Estado>,
    cache: Mutex<CacheSieve<NumeroPagina, Arc<Pagina>>>,
    leituras_de_disco: AtomicU64,
    gravacoes: AtomicU64,
    confirmacoes: AtomicU64,
}

impl<A: Arquivo> Paginador<A> {
    /// Cria um arquivo novo (falha se já existir).
    pub fn criar<V: Vfs<Arquivo = A>>(
        vfs: &V,
        caminho: &Path,
        tamanho: TamanhoPagina,
        capacidade_cache: usize,
    ) -> Resultado<Self> {
        let arquivo = vfs.abrir(caminho, ModoAbertura::CriarNovo)?;
        let tam = tamanho.bytes();
        let mut inicio = vec![0u8; tam * PAGINAS_RESERVADAS as usize];
        let mut cabecalho = Cabecalho::novo(tamanho);
        inicio[..TAMANHO_CABECALHO].copy_from_slice(&cabecalho.codificar());
        cabecalho.geracao = 1;
        inicio[tam..tam + TAMANHO_CABECALHO].copy_from_slice(&cabecalho.codificar());
        arquivo.gravar_em(0, &inicio)?;
        arquivo.sincronizar()?;
        Ok(Self::montar(arquivo, cabecalho, capacidade_cache))
    }

    /// Abre um arquivo existente. Vale a cópia íntegra do cabeçalho de maior geração.
    pub fn abrir<V: Vfs<Arquivo = A>>(
        vfs: &V,
        caminho: &Path,
        capacidade_cache: usize,
    ) -> Resultado<Self> {
        let arquivo = vfs.abrir(caminho, ModoAbertura::LeituraGravacao)?;
        let tamanho_arquivo = arquivo.tamanho()?;
        if tamanho_arquivo < TAMANHO_CABECALHO as u64 {
            return Err(ErroArmazenamento::NaoEhSiouxDb);
        }
        let ler_copia = |posicao: u64| -> Resultado<Cabecalho> {
            if posicao + TAMANHO_CABECALHO as u64 > tamanho_arquivo {
                return Err(ErroArmazenamento::CabecalhoCorrompido);
            }
            let mut b = [0u8; TAMANHO_CABECALHO];
            arquivo.ler_exato_em(posicao, &mut b)?;
            Cabecalho::decodificar(&b)
        };
        let copia_a = ler_copia(0);
        let copia_b = match &copia_a {
            Ok(a) => ler_copia(Cabecalho::posicao(true, a.tamanho_pagina))
                .ok()
                .filter(|b| b.tamanho_pagina == a.tamanho_pagina),
            // Sem a cópia A não se sabe o tamanho da página: procura a B nas posições possíveis.
            Err(_) => TamanhoPagina::TODOS.into_iter().find_map(|t| {
                ler_copia(Cabecalho::posicao(true, t))
                    .ok()
                    .filter(|b| b.tamanho_pagina == t)
            }),
        };
        let cabecalho = match (copia_a, copia_b) {
            (Ok(a), Some(b)) => {
                if b.geracao > a.geracao {
                    b
                } else {
                    a
                }
            }
            (Ok(a), None) => a,
            (Err(_), Some(b)) => b,
            (Err(e), None) => return Err(e),
        };
        let minimo = cabecalho
            .total_paginas
            .checked_mul(cabecalho.tamanho_pagina.bytes() as u64)
            .ok_or(ErroArmazenamento::CabecalhoCorrompido)?;
        if tamanho_arquivo < minimo {
            return Err(ErroArmazenamento::ArquivoTruncado);
        }
        Ok(Self::montar(arquivo, cabecalho, capacidade_cache))
    }

    fn montar(arquivo: A, cabecalho: Cabecalho, capacidade_cache: usize) -> Self {
        Paginador {
            arquivo,
            tamanho: cabecalho.tamanho_pagina,
            estado: Mutex::new(Estado {
                cabecalho,
                liberadas: Vec::new(),
                falhou: false,
            }),
            cache: Mutex::new(CacheSieve::novo(capacidade_cache)),
            leituras_de_disco: AtomicU64::new(0),
            gravacoes: AtomicU64::new(0),
            confirmacoes: AtomicU64::new(0),
        }
    }

    /// Tamanho das páginas deste arquivo.
    pub fn tamanho_pagina(&self) -> TamanhoPagina {
        self.tamanho
    }

    /// Cópia do cabeçalho como está em memória (inclui o que ainda não foi confirmado).
    pub fn cabecalho(&self) -> Cabecalho {
        self.travar_estado().cabecalho.clone()
    }

    /// Página zerada do tamanho deste arquivo, pronta para preencher e gravar.
    pub fn pagina_nova(&self, tipo: TipoPagina) -> Pagina {
        Pagina::nova(self.tamanho, tipo)
    }

    /// Define a página raiz do catálogo (vale a partir da próxima confirmação).
    pub fn definir_raiz_catalogo(&self, pagina: NumeroPagina) -> Resultado<()> {
        self.estado_usavel()?.cabecalho.raiz_catalogo = pagina;
        Ok(())
    }

    /// Define a página raiz do livro-razão (vale a partir da próxima confirmação).
    pub fn definir_raiz_razao(&self, pagina: NumeroPagina) -> Resultado<()> {
        self.estado_usavel()?.cabecalho.raiz_razao = pagina;
        Ok(())
    }

    /// Define o último LSN confirmado (vale a partir da próxima confirmação).
    pub fn definir_ultimo_lsn(&self, lsn: u64) -> Resultado<()> {
        self.estado_usavel()?.cabecalho.ultimo_lsn = lsn;
        Ok(())
    }

    /// Reserva uma página do tipo dado, já gravada zerada, e devolve o número dela.
    /// Reaproveita uma página livre se houver; senão, aumenta o arquivo.
    pub fn alocar(&self, tipo: TipoPagina) -> Resultado<NumeroPagina> {
        let mut e = self.estado_usavel()?;
        while e.cabecalho.primeira_livre != 0 {
            let n = e.cabecalho.primeira_livre;
            let total = e.cabecalho.total_paginas;
            let dentro = |p: NumeroPagina| (PAGINAS_RESERVADAS..total).contains(&p);
            let proxima = if dentro(n) {
                self.ler_sem_conferir_limites(n)
                    .ok()
                    .filter(|p| p.tipo() == TipoPagina::Livre)
                    .map(|p| {
                        let prox =
                            u64::from_le_bytes(p.conteudo()[..8].try_into().expect("8 bytes"));
                        if dentro(prox) && prox != n { prox } else { 0 }
                    })
            } else {
                None
            };
            match proxima {
                Some(prox) => {
                    self.gravar_sem_conferir_limites(n, Pagina::nova(self.tamanho, tipo))?;
                    e.cabecalho.primeira_livre = prox;
                    return Ok(n);
                }
                // Lista danificada por uma queda: corta aqui (ver a documentação do módulo).
                None => e.cabecalho.primeira_livre = 0,
            }
        }
        let n = e.cabecalho.total_paginas;
        self.gravar_sem_conferir_limites(n, Pagina::nova(self.tamanho, tipo))?;
        e.cabecalho.total_paginas = n + 1;
        Ok(n)
    }

    /// Lê uma página (do cache ou do disco), sempre verificada.
    pub fn ler(&self, numero: NumeroPagina) -> Resultado<Arc<Pagina>> {
        self.conferir_limites(&*self.estado_usavel()?, numero)?;
        self.ler_sem_conferir_limites(numero)
    }

    /// Grava uma página na posição `numero` (que precisa ter sido alocada).
    pub fn gravar(&self, numero: NumeroPagina, pagina: Pagina) -> Resultado<()> {
        let e = self.estado_usavel()?;
        self.conferir_limites(&e, numero)?;
        if pagina.tamanho() != self.tamanho.bytes() {
            return Err(ErroArmazenamento::TamanhoDeBufferErrado {
                esperado: self.tamanho.bytes(),
                recebido: pagina.tamanho(),
            });
        }
        self.gravar_sem_conferir_limites(numero, pagina)
    }

    /// Devolve uma página ao banco. Ela continua legível até a próxima confirmação e só
    /// volta a ser entregue por [`Paginador::alocar`] depois dela.
    pub fn liberar(&self, numero: NumeroPagina) -> Resultado<()> {
        let mut e = self.estado_usavel()?;
        self.conferir_limites(&e, numero)?;
        if !e.liberadas.contains(&numero) {
            e.liberadas.push(numero);
        }
        Ok(())
    }

    /// Torna durável tudo o que foi feito desde a última confirmação.
    ///
    /// Ordem: sincroniza as páginas, grava o cabeçalho na cópia mais antiga, sincroniza de
    /// novo. Se der erro, o paginador passa a recusar operações
    /// ([`ErroArmazenamento::ConfirmacaoFalhou`]): depois de um `fsync` que falhou não dá
    /// para saber o que chegou ao disco, então o certo é fechar e abrir de novo.
    pub fn confirmar(&self) -> Resultado<()> {
        let mut e = self.estado_usavel()?;
        let novo = match self.confirmar_no_disco(&e.cabecalho) {
            Ok(novo) => novo,
            Err(erro) => {
                e.falhou = true;
                return Err(erro);
            }
        };
        e.cabecalho = novo;
        self.confirmacoes.fetch_add(1, Ordering::Relaxed);
        self.encadear_liberadas(&mut e);
        Ok(())
    }

    fn confirmar_no_disco(&self, atual: &Cabecalho) -> Resultado<Cabecalho> {
        self.arquivo.sincronizar()?;
        let novo = self.gravar_proximo_cabecalho(atual)?;
        self.arquivo.sincronizar()?;
        Ok(novo)
    }

    /// Grava (sem sincronizar) o cabeçalho da geração seguinte na cópia mais antiga.
    fn gravar_proximo_cabecalho(&self, atual: &Cabecalho) -> Resultado<Cabecalho> {
        let mut novo = atual.clone();
        novo.geracao += 1;
        self.arquivo
            .gravar_em(novo.posicao_da_copia(), &novo.codificar())?;
        Ok(novo)
    }

    /// Depois de confirmado, as páginas liberadas não são mais referenciadas por nenhum
    /// cabeçalho válido: agora podem virar páginas livres. Se uma gravação falhar aqui, o
    /// resto fica fora da lista (perde espaço, não perde dado).
    fn encadear_liberadas(&self, e: &mut Estado) {
        for n in std::mem::take(&mut e.liberadas) {
            let mut livre = Pagina::nova(self.tamanho, TipoPagina::Livre);
            livre.conteudo_mut()[..8].copy_from_slice(&e.cabecalho.primeira_livre.to_le_bytes());
            if self.gravar_sem_conferir_limites(n, livre).is_err() {
                break;
            }
            e.cabecalho.primeira_livre = n;
        }
    }

    /// Números para acompanhamento.
    pub fn estatisticas(&self) -> EstatisticasPaginador {
        let e = self.travar_estado();
        EstatisticasPaginador {
            tamanho_pagina: self.tamanho.bytes(),
            total_paginas: e.cabecalho.total_paginas,
            geracao: e.cabecalho.geracao,
            leituras_de_disco: self.leituras_de_disco.load(Ordering::Relaxed),
            gravacoes: self.gravacoes.load(Ordering::Relaxed),
            confirmacoes: self.confirmacoes.load(Ordering::Relaxed),
            liberadas_pendentes: e.liberadas.len(),
            cache: self.travar_cache().estatisticas(),
        }
    }

    fn travar_estado(&self) -> MutexGuard<'_, Estado> {
        self.estado.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn travar_cache(&self) -> MutexGuard<'_, CacheSieve<NumeroPagina, Arc<Pagina>>> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn estado_usavel(&self) -> Resultado<MutexGuard<'_, Estado>> {
        let e = self.travar_estado();
        if e.falhou {
            return Err(ErroArmazenamento::ConfirmacaoFalhou);
        }
        Ok(e)
    }

    fn conferir_limites(&self, e: &Estado, numero: NumeroPagina) -> Resultado<()> {
        if (PAGINAS_RESERVADAS..e.cabecalho.total_paginas).contains(&numero) {
            Ok(())
        } else {
            Err(ErroArmazenamento::PaginaInexistente(numero))
        }
    }

    fn posicao(&self, numero: NumeroPagina) -> u64 {
        numero * self.tamanho.bytes() as u64
    }

    fn ler_sem_conferir_limites(&self, numero: NumeroPagina) -> Resultado<Arc<Pagina>> {
        if let Some(p) = self.travar_cache().obter(&numero) {
            return Ok(p);
        }
        let mut dados = vec![0u8; self.tamanho.bytes()].into_boxed_slice();
        let lidos = self.arquivo.ler_em(self.posicao(numero), &mut dados)?;
        self.leituras_de_disco.fetch_add(1, Ordering::Relaxed);
        if lidos < dados.len() {
            return Err(ErroArmazenamento::PaginaCorrompida(numero));
        }
        let pagina = Arc::new(Pagina::de_bytes(dados, numero)?);
        self.travar_cache().inserir(numero, Arc::clone(&pagina));
        Ok(pagina)
    }

    fn gravar_sem_conferir_limites(
        &self,
        numero: NumeroPagina,
        mut pagina: Pagina,
    ) -> Resultado<()> {
        let resultado = self
            .arquivo
            .gravar_em(self.posicao(numero), pagina.selar(numero));
        self.gravacoes.fetch_add(1, Ordering::Relaxed);
        let mut cache = self.travar_cache();
        match resultado {
            Ok(()) => {
                cache.inserir(numero, Arc::new(pagina));
                Ok(())
            }
            Err(e) => {
                // Não se sabe o que ficou no disco: o cache não pode continuar com a versão velha.
                cache.remover(&numero);
                Err(e.into())
            }
        }
    }
}

#[cfg(test)]
mod testes {
    use super::*;
    use siouxdb_vfs::{ConfigSimulacao, VfsSimulado};
    use std::path::PathBuf;

    type Pag = Paginador<<VfsSimulado as Vfs>::Arquivo>;

    fn disco(semente: u64) -> VfsSimulado {
        VfsSimulado::novo(ConfigSimulacao {
            semente,
            ..ConfigSimulacao::default()
        })
    }

    fn caminho() -> PathBuf {
        PathBuf::from("banco.sioux")
    }

    fn com_texto(p: &Pag, tipo: TipoPagina, texto: &[u8]) -> Pagina {
        let mut pagina = p.pagina_nova(tipo);
        pagina.conteudo_mut()[..texto.len()].copy_from_slice(texto);
        pagina
    }

    fn texto(p: &Pagina, tamanho: usize) -> Vec<u8> {
        p.conteudo()[..tamanho].to_vec()
    }

    #[test]
    fn cria_grava_confirma_e_reabre() {
        for tamanho in TamanhoPagina::TODOS {
            let vfs = disco(1);
            let p = Pag::criar(&vfs, &caminho(), tamanho, 16).unwrap();
            let a = p.alocar(TipoPagina::Catalogo).unwrap();
            let b = p.alocar(TipoPagina::NoArvore).unwrap();
            assert_eq!((a, b), (2, 3));
            p.gravar(a, com_texto(&p, TipoPagina::Catalogo, b"catalogo"))
                .unwrap();
            p.gravar(b, com_texto(&p, TipoPagina::NoArvore, b"arvore"))
                .unwrap();
            p.definir_raiz_catalogo(a).unwrap();
            p.definir_ultimo_lsn(77).unwrap();
            p.confirmar().unwrap();
            drop(p);
            vfs.queda();

            let p = Pag::abrir(&vfs, &caminho(), 16).unwrap();
            let c = p.cabecalho();
            assert_eq!(
                (
                    c.tamanho_pagina,
                    c.total_paginas,
                    c.raiz_catalogo,
                    c.ultimo_lsn
                ),
                (tamanho, 4, a, 77)
            );
            assert_eq!(texto(&p.ler(a).unwrap(), 8), b"catalogo");
            assert_eq!(p.ler(b).unwrap().tipo(), TipoPagina::NoArvore);
            assert!(matches!(
                p.ler(1),
                Err(ErroArmazenamento::PaginaInexistente(1))
            ));
            assert!(matches!(
                p.ler(4),
                Err(ErroArmazenamento::PaginaInexistente(4))
            ));
        }
    }

    #[test]
    fn recusa_criar_por_cima_e_arquivo_estranho() {
        let vfs = disco(1);
        Pag::criar(&vfs, &caminho(), TamanhoPagina::K4, 4).unwrap();
        assert!(Pag::criar(&vfs, &caminho(), TamanhoPagina::K4, 4).is_err());

        let outro = PathBuf::from("texto.txt");
        let arq = vfs.abrir(&outro, ModoAbertura::CriarNovo).unwrap();
        arq.gravar_em(0, &[b'x'; 9000]).unwrap();
        assert!(matches!(
            Pag::abrir(&vfs, &outro, 4),
            Err(ErroArmazenamento::NaoEhSiouxDb)
        ));
        arq.truncar(10).unwrap();
        assert!(matches!(
            Pag::abrir(&vfs, &outro, 4),
            Err(ErroArmazenamento::NaoEhSiouxDb)
        ));
    }

    #[test]
    fn cache_evita_leitura_de_disco() {
        let vfs = disco(1);
        let p = Pag::criar(&vfs, &caminho(), TamanhoPagina::K4, 8).unwrap();
        let n = p.alocar(TipoPagina::NoArvore).unwrap();
        p.confirmar().unwrap();
        drop(p);
        let p = Pag::abrir(&vfs, &caminho(), 8).unwrap();
        for _ in 0..5 {
            p.ler(n).unwrap();
        }
        let e = p.estatisticas();
        assert_eq!(
            (e.leituras_de_disco, e.cache.acertos, e.cache.faltas),
            (1, 4, 1)
        );
    }

    #[test]
    fn pagina_liberada_so_volta_depois_de_confirmar() {
        let vfs = disco(1);
        let p = Pag::criar(&vfs, &caminho(), TamanhoPagina::K4, 8).unwrap();
        let a = p.alocar(TipoPagina::NoArvore).unwrap();
        let b = p.alocar(TipoPagina::NoArvore).unwrap();
        p.confirmar().unwrap();

        p.liberar(a).unwrap();
        p.liberar(a).unwrap(); // repetido não duplica
        let c = p.alocar(TipoPagina::NoArvore).unwrap();
        assert!(
            c != a && c != b,
            "página liberada foi entregue antes da confirmação"
        );
        assert_eq!(p.estatisticas().liberadas_pendentes, 1);
        p.liberar(b).unwrap();
        p.confirmar().unwrap();

        let (x, y) = (
            p.alocar(TipoPagina::Catalogo).unwrap(),
            p.alocar(TipoPagina::Catalogo).unwrap(),
        );
        assert_eq!([x.min(y), x.max(y)], [a, b]);
        assert_eq!(p.ler(a).unwrap().tipo(), TipoPagina::Catalogo);
        let total = p.cabecalho().total_paginas;
        assert_eq!(
            p.alocar(TipoPagina::Catalogo).unwrap(),
            total,
            "sem livres, o arquivo cresce"
        );

        // A lista de livres sobrevive a fechar e abrir.
        p.liberar(x).unwrap();
        p.confirmar().unwrap();
        p.confirmar().unwrap();
        drop(p);
        let p = Pag::abrir(&vfs, &caminho(), 8).unwrap();
        assert_eq!(p.alocar(TipoPagina::Razao).unwrap(), x);
    }

    #[test]
    fn queda_antes_de_confirmar_volta_ao_estado_confirmado() {
        for semente in 0..200 {
            let vfs = disco(semente);
            let p = Pag::criar(&vfs, &caminho(), TamanhoPagina::K4, 8).unwrap();
            let a = p.alocar(TipoPagina::NoArvore).unwrap();
            p.gravar(a, com_texto(&p, TipoPagina::NoArvore, b"confirmado"))
                .unwrap();
            p.definir_raiz_catalogo(a).unwrap();
            p.confirmar().unwrap();
            let confirmado = p.cabecalho();

            for i in 0..5u8 {
                let n = p.alocar(TipoPagina::BlocoFrio).unwrap();
                p.gravar(n, com_texto(&p, TipoPagina::BlocoFrio, &[i; 32]))
                    .unwrap();
            }
            p.definir_raiz_catalogo(5).unwrap();
            drop(p);
            vfs.queda();

            let p = Pag::abrir(&vfs, &caminho(), 8).unwrap();
            assert_eq!(p.cabecalho(), confirmado, "semente {semente}");
            assert_eq!(texto(&p.ler(a).unwrap(), 10), b"confirmado");
            assert!(p.ler(a + 1).is_err());
            // O arquivo continua utilizável.
            let n = p.alocar(TipoPagina::Catalogo).unwrap();
            assert_eq!(n, a + 1);
            p.confirmar().unwrap();
        }
    }

    #[test]
    fn sobrescrita_sem_confirmar_nunca_vira_lixo_aceito() {
        let (mut antigas, mut novas, mut erros) = (0, 0, 0);
        for semente in 0..300 {
            let vfs = disco(semente);
            let p = Pag::criar(&vfs, &caminho(), TamanhoPagina::K8, 8).unwrap();
            let n = p.alocar(TipoPagina::NoArvore).unwrap();
            p.gravar(n, com_texto(&p, TipoPagina::NoArvore, &[0xAA; 8000]))
                .unwrap();
            p.confirmar().unwrap();
            p.gravar(n, com_texto(&p, TipoPagina::NoArvore, &[0xBB; 8000]))
                .unwrap();
            drop(p);
            vfs.queda();

            let p = Pag::abrir(&vfs, &caminho(), 8).unwrap();
            match p.ler(n) {
                Ok(pagina) if texto(&pagina, 8000) == [0xAA; 8000] => antigas += 1,
                Ok(pagina) if texto(&pagina, 8000) == [0xBB; 8000] => novas += 1,
                Ok(_) => panic!("semente {semente}: página misturada foi aceita"),
                Err(ErroArmazenamento::PaginaCorrompida(_)) => erros += 1,
                Err(e) => panic!("semente {semente}: erro inesperado: {e}"),
            }
        }
        assert!(
            antigas > 0 && novas > 0 && erros > 0,
            "a simulação precisa cobrir os três casos: {antigas}/{novas}/{erros}"
        );
    }

    /// Alteração por cópia: a versão nova vai para uma página nova, a raiz troca e a velha
    /// é liberada. Com queda em qualquer ponto, o arquivo abre numa versão inteira.
    #[test]
    fn alteracao_por_copia_e_tudo_ou_nada() {
        let (mut ficou_na_antiga, mut foi_para_a_nova) = (0, 0);
        for semente in 0..400u64 {
            let vfs = disco(semente);
            let p = Pag::criar(&vfs, &caminho(), TamanhoPagina::K4, 8).unwrap();
            let mut raiz = p.alocar(TipoPagina::Catalogo).unwrap();
            p.gravar(raiz, com_texto(&p, TipoPagina::Catalogo, b"versao-000"))
                .unwrap();
            p.definir_raiz_catalogo(raiz).unwrap();
            p.confirmar().unwrap();

            let rodadas = 1 + semente % 4;
            let mut versao = 0;
            for r in 1..=rodadas {
                let nova = p.alocar(TipoPagina::Catalogo).unwrap();
                p.gravar(
                    nova,
                    com_texto(
                        &p,
                        TipoPagina::Catalogo,
                        format!("versao-{r:03}").as_bytes(),
                    ),
                )
                .unwrap();
                p.definir_raiz_catalogo(nova).unwrap();
                p.liberar(raiz).unwrap();
                raiz = nova;
                if r < rodadas {
                    p.confirmar().unwrap();
                    versao = r;
                }
            }
            // Na última rodada a queda acontece num ponto que depende da semente.
            match semente % 3 {
                0 => {}
                1 => {
                    p.arquivo.sincronizar().unwrap();
                }
                _ => {
                    p.arquivo.sincronizar().unwrap();
                    let atual = p.cabecalho();
                    p.gravar_proximo_cabecalho(&atual).unwrap();
                }
            }
            drop(p);
            vfs.queda();

            let p = Pag::abrir(&vfs, &caminho(), 8).unwrap();
            let lida = p.ler(p.cabecalho().raiz_catalogo).unwrap();
            let conteudo = texto(&lida, 10);
            if conteudo == format!("versao-{versao:03}").as_bytes() {
                ficou_na_antiga += 1;
            } else if conteudo == format!("versao-{rodadas:03}").as_bytes() {
                foi_para_a_nova += 1;
            } else {
                panic!(
                    "semente {semente}: raiz aponta para {:?}",
                    String::from_utf8_lossy(&conteudo)
                );
            }
            // E segue funcionando: aloca, grava e confirma sem entregar a página da raiz.
            for _ in 0..4 {
                let n = p.alocar(TipoPagina::NoArvore).unwrap();
                assert_ne!(
                    n,
                    p.cabecalho().raiz_catalogo,
                    "semente {semente}: página em uso entregue como livre"
                );
            }
            p.confirmar().unwrap();
            assert_eq!(
                texto(&p.ler(p.cabecalho().raiz_catalogo).unwrap(), 10),
                conteudo
            );
        }
        assert!(
            ficou_na_antiga > 0 && foi_para_a_nova > 0,
            "{ficou_na_antiga}/{foi_para_a_nova}"
        );
    }

    #[test]
    fn abre_pela_outra_copia_se_uma_estragar() {
        for tamanho in TamanhoPagina::TODOS {
            for estragar_b in [false, true] {
                let vfs = disco(3);
                let p = Pag::criar(&vfs, &caminho(), tamanho, 4).unwrap();
                p.alocar(TipoPagina::Catalogo).unwrap();
                p.confirmar().unwrap(); // geração 2, na cópia A
                p.alocar(TipoPagina::Catalogo).unwrap();
                p.confirmar().unwrap(); // geração 3, na cópia B
                drop(p);

                let arq = vfs
                    .abrir(&caminho(), ModoAbertura::LeituraGravacao)
                    .unwrap();
                arq.gravar_em(Cabecalho::posicao(estragar_b, tamanho), &[0x5A; 64])
                    .unwrap();
                arq.sincronizar().unwrap();
                drop(arq);

                let p = Pag::abrir(&vfs, &caminho(), 4).unwrap();
                let c = p.cabecalho();
                // Sem a B vale a geração 2 (uma página a menos); sem a A, a 3.
                let esperado = if estragar_b { (2, 3) } else { (3, 4) };
                assert_eq!((c.geracao, c.total_paginas), esperado);
                assert_eq!(c.tamanho_pagina, tamanho);
            }
        }
    }

    #[test]
    fn duas_copias_estragadas_e_arquivo_cortado_dao_erro_claro() {
        let vfs = disco(1);
        let p = Pag::criar(&vfs, &caminho(), TamanhoPagina::K4, 4).unwrap();
        p.alocar(TipoPagina::Catalogo).unwrap();
        p.confirmar().unwrap();
        drop(p);
        let arq = vfs
            .abrir(&caminho(), ModoAbertura::LeituraGravacao)
            .unwrap();
        arq.truncar(2 * 4096 + 100).unwrap();
        assert!(matches!(
            Pag::abrir(&vfs, &caminho(), 4),
            Err(ErroArmazenamento::ArquivoTruncado)
        ));
        arq.gravar_em(20, &[1]).unwrap();
        arq.gravar_em(4096 + 20, &[1]).unwrap();
        assert!(matches!(
            Pag::abrir(&vfs, &caminho(), 4),
            Err(ErroArmazenamento::CabecalhoCorrompido)
        ));
    }

    #[test]
    fn confirmacao_que_falha_trava_o_paginador() {
        let vfs = VfsSimulado::novo(ConfigSimulacao {
            falha_sincronizar_por_mil: 1000,
            ..ConfigSimulacao::default()
        });
        // Com o `fsync` sempre falhando nem dá para criar.
        assert!(Pag::criar(&vfs, &caminho(), TamanhoPagina::K4, 4).is_err());

        let mut travou = false;
        for semente in 0..50 {
            let vfs = VfsSimulado::novo(ConfigSimulacao {
                semente,
                falha_sincronizar_por_mil: 300,
                ..ConfigSimulacao::default()
            });
            let Ok(p) = Pag::criar(&vfs, &caminho(), TamanhoPagina::K4, 4) else {
                continue;
            };
            for _ in 0..20 {
                let Ok(n) = p.alocar(TipoPagina::NoArvore) else {
                    break;
                };
                if p.confirmar().is_err() {
                    travou = true;
                    assert!(matches!(
                        p.ler(n),
                        Err(ErroArmazenamento::ConfirmacaoFalhou)
                    ));
                    assert!(matches!(
                        p.alocar(TipoPagina::NoArvore),
                        Err(ErroArmazenamento::ConfirmacaoFalhou)
                    ));
                    assert!(matches!(
                        p.confirmar(),
                        Err(ErroArmazenamento::ConfirmacaoFalhou)
                    ));
                    break;
                }
            }
            // Depois de reabrir, o arquivo está inteiro numa das gerações.
            drop(p);
            vfs.queda();
            if let Ok(p) = Pag::abrir(&vfs, &caminho(), 4) {
                for n in PAGINAS_RESERVADAS..p.cabecalho().total_paginas {
                    p.ler(n).unwrap();
                }
            } else {
                panic!("semente {semente}: arquivo não abriu depois de falha de fsync");
            }
        }
        assert!(travou);
    }
}
