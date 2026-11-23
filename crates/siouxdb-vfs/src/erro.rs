use std::fmt;
use std::path::PathBuf;

/// Resultado das operações de disco.
pub type Resultado<T> = Result<T, ErroVfs>;

/// Erros da camada de disco.
#[derive(Debug)]
pub enum ErroVfs {
    /// Erro do sistema operacional.
    Io(std::io::Error),
    /// O arquivo não existe.
    NaoEncontrado(PathBuf),
    /// O arquivo já existe (ao criar com [`crate::ModoAbertura::CriarNovo`]).
    JaExiste(PathBuf),
    /// O arquivo terminou antes do esperado.
    FimInesperado {
        /// Onde a leitura começou.
        posicao: u64,
        /// Quantos bytes eram esperados.
        esperado: usize,
        /// Quantos vieram.
        lidos: usize,
    },
    /// Não há espaço em disco.
    DiscoCheio,
    /// Arquivo aberto só para leitura.
    SomenteLeitura,
    /// Outro processo (ou outra abertura) já está usando o arquivo com exclusividade.
    EmUso(PathBuf),
    /// Falha provocada de propósito pelo disco simulado.
    FalhaSimulada(&'static str),
}

impl fmt::Display for ErroVfs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErroVfs::EmUso(c) => write!(
                f,
                "o arquivo já está em uso por outro programa: {}",
                c.display()
            ),
            ErroVfs::Io(e) => write!(f, "erro de disco: {e}"),
            ErroVfs::NaoEncontrado(c) => write!(f, "arquivo não encontrado: {}", c.display()),
            ErroVfs::JaExiste(c) => write!(f, "arquivo já existe: {}", c.display()),
            ErroVfs::FimInesperado {
                posicao,
                esperado,
                lidos,
            } => {
                write!(
                    f,
                    "fim de arquivo inesperado na posição {posicao}: esperava {esperado} bytes, vieram {lidos}"
                )
            }
            ErroVfs::DiscoCheio => write!(f, "disco cheio"),
            ErroVfs::SomenteLeitura => write!(f, "arquivo aberto só para leitura"),
            ErroVfs::FalhaSimulada(o) => write!(f, "falha simulada: {o}"),
        }
    }
}

impl std::error::Error for ErroVfs {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ErroVfs::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ErroVfs {
    fn from(e: std::io::Error) -> Self {
        ErroVfs::Io(e)
    }
}
