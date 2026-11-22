//! Camada de acesso a disco do SiouxDB.
//!
//! Todo o motor lê e grava arquivos só por estes dois contratos ([`Vfs`] e [`Arquivo`]).
//! Assim o mesmo código roda sobre:
//!
//! - [`VfsReal`]: o sistema de arquivos de verdade;
//! - [`VfsSimulado`]: um disco em memória, determinístico, que simula queda de energia
//!   (o que não passou por [`Arquivo::sincronizar`] pode sumir ou ficar pela metade),
//!   disco cheio e falhas de `fsync`. É a base da simulação determinística dos testes.
//!
//! Leituras e gravações são sempre por posição (`*_em`), sem cursor compartilhado.

mod erro;
mod real;
mod simulado;

pub use erro::{ErroVfs, Resultado};
pub use real::VfsReal;
pub use simulado::{ConfigSimulacao, VfsSimulado};

use std::path::Path;

/// Como abrir um arquivo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModoAbertura {
    /// Só leitura; o arquivo precisa existir.
    Leitura,
    /// Leitura e gravação; o arquivo precisa existir.
    LeituraGravacao,
    /// Leitura e gravação; cria o arquivo vazio se não existir.
    CriarSeNaoExistir,
    /// Leitura e gravação; falha se o arquivo já existir.
    CriarNovo,
}

/// Um arquivo aberto. Pode ser usado por várias threads ao mesmo tempo.
pub trait Arquivo: Send + Sync {
    /// Lê a partir de `posicao` até encher `destino` ou chegar ao fim do arquivo.
    /// Devolve quantos bytes foram lidos (menos que `destino.len()` só no fim do arquivo).
    fn ler_em(&self, posicao: u64, destino: &mut [u8]) -> Resultado<usize>;

    /// Grava `dados` inteiros a partir de `posicao`, aumentando o arquivo se preciso.
    fn gravar_em(&self, posicao: u64, dados: &[u8]) -> Resultado<()>;

    /// Garante que tudo o que foi gravado até aqui sobrevive a uma queda de energia.
    fn sincronizar(&self) -> Resultado<()>;

    /// Tamanho atual em bytes.
    fn tamanho(&self) -> Resultado<u64>;

    /// Corta (ou aumenta com zeros) o arquivo para `tamanho` bytes.
    fn truncar(&self, tamanho: u64) -> Resultado<()>;

    /// Lê exatamente `destino.len()` bytes; erro se o arquivo acabar antes.
    fn ler_exato_em(&self, posicao: u64, destino: &mut [u8]) -> Resultado<()> {
        let lidos = self.ler_em(posicao, destino)?;
        if lidos < destino.len() {
            return Err(ErroVfs::FimInesperado {
                posicao,
                esperado: destino.len(),
                lidos,
            });
        }
        Ok(())
    }
}

/// Um sistema de arquivos.
pub trait Vfs: Send + Sync {
    /// O tipo de arquivo aberto por este sistema.
    type Arquivo: Arquivo;

    /// Abre (ou cria) um arquivo.
    fn abrir(&self, caminho: &Path, modo: ModoAbertura) -> Resultado<Self::Arquivo>;

    /// O arquivo existe?
    fn existe(&self, caminho: &Path) -> Resultado<bool>;

    /// Apaga o arquivo.
    fn apagar(&self, caminho: &Path) -> Resultado<()>;

    /// Renomeia de forma atômica, substituindo o destino se existir. Depois de uma
    /// queda, o arquivo está com o nome antigo ou com o novo, nunca pela metade.
    fn renomear(&self, de: &Path, para: &Path) -> Resultado<()>;
}
