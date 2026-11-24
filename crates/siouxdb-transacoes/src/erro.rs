use siouxdb_armazenamento::{ErroArmazenamento, NumeroPagina};
use std::fmt;
use std::time::Duration;

/// Resultado das operações do banco.
pub type Resultado<T> = Result<T, ErroBanco>;

/// Erros do banco.
#[derive(Debug)]
pub enum ErroBanco {
    /// Erro do armazenamento (disco, cifra, página corrompida...).
    Armazenamento(ErroArmazenamento),
    /// Esperou a vez de escrever mais que o permitido.
    EsperaEsgotada(Duration),
    /// Página fora do banco (na foto em uso) ou reservada.
    PaginaInexistente(NumeroPagina),
    /// Tentou liberar uma página que já está livre.
    PaginaJaLivre(NumeroPagina),
    /// O WAL tem uma confirmação que não autentica: foi alterado por quem não tem a chave.
    WalAdulterado,
    /// Uma gravação ou `fsync` falhou antes; o estado em disco é incerto. Feche e abra de novo.
    Falhou,
    /// A estrutura do livro-razão está danificada (rode o `VERIFY` para o diagnóstico).
    Razao(String),
}

impl fmt::Display for ErroBanco {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErroBanco::Armazenamento(e) => write!(f, "{e}"),
            ErroBanco::EsperaEsgotada(d) => {
                write!(
                    f,
                    "esperou {} ms pela vez de escrever e desistiu",
                    d.as_millis()
                )
            }
            ErroBanco::PaginaInexistente(n) => {
                write!(f, "página {n} não existe nesta foto do banco")
            }
            ErroBanco::PaginaJaLivre(n) => write!(f, "página {n} já está livre"),
            ErroBanco::WalAdulterado => write!(f, "o WAL foi alterado por quem não tem a chave"),
            ErroBanco::Falhou => {
                write!(f, "uma gravação falhou antes; feche e abra o banco de novo")
            }
            ErroBanco::Razao(m) => write!(f, "livro-razão: {m}"),
        }
    }
}

impl std::error::Error for ErroBanco {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ErroBanco::Armazenamento(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ErroArmazenamento> for ErroBanco {
    fn from(e: ErroArmazenamento) -> Self {
        ErroBanco::Armazenamento(e)
    }
}

impl From<siouxdb_vfs::ErroVfs> for ErroBanco {
    fn from(e: siouxdb_vfs::ErroVfs) -> Self {
        ErroBanco::Armazenamento(e.into())
    }
}
