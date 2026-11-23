use siouxdb_armazenamento::NumeroPagina;
use siouxdb_transacoes::ErroBanco;
use std::fmt;

/// Resultado das operações de índice.
pub type Resultado<T> = Result<T, ErroIndice>;

/// Erros dos índices.
#[derive(Debug)]
pub enum ErroIndice {
    /// Erro do banco (disco, cifra, transação).
    Banco(ErroBanco),
    /// Chave + valor grandes demais para caber num nó.
    EntradaGrandeDemais {
        /// Tamanho pedido (chave + valor).
        tamanho: usize,
        /// Máximo aceito neste tamanho de página.
        maximo: usize,
    },
    /// A página não tem a forma de um nó válido. Como toda página é verificada ao ser lida,
    /// isso indica defeito do programa, não do disco.
    Corrompido {
        /// Página com problema.
        pagina: NumeroPagina,
        /// O que estava errado.
        motivo: &'static str,
    },
}

impl fmt::Display for ErroIndice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErroIndice::Banco(e) => write!(f, "{e}"),
            ErroIndice::EntradaGrandeDemais { tamanho, maximo } => {
                write!(
                    f,
                    "entrada de {tamanho} bytes não cabe no índice (máximo {maximo})"
                )
            }
            ErroIndice::Corrompido { pagina, motivo } => {
                write!(f, "nó de índice inválido na página {pagina}: {motivo}")
            }
        }
    }
}

impl std::error::Error for ErroIndice {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ErroIndice::Banco(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ErroBanco> for ErroIndice {
    fn from(e: ErroBanco) -> Self {
        ErroIndice::Banco(e)
    }
}
