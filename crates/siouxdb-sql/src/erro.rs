use siouxdb_catalogo::ErroCatalogo;
use std::fmt;

/// Resultado das operações de SQL.
pub type Resultado<T> = Result<T, ErroSql>;

/// Erros de SQL.
#[derive(Debug)]
pub enum ErroSql {
    /// Erro de escrita do comando (posição em caracteres, a partir de 0).
    Sintaxe {
        /// Onde.
        posicao: usize,
        /// O quê.
        mensagem: String,
    },
    /// Comando bem escrito, mas que não faz sentido (coluna inexistente, tipos...).
    Semantica(String),
    /// Erro das tabelas, índices ou do banco.
    Catalogo(ErroCatalogo),
    /// A consulta precisou guardar mais que o limite de memória
    /// ([`definir_limite_memoria`](crate::definir_limite_memoria)).
    MemoriaEsgotada {
        /// O limite, em bytes.
        limite: usize,
    },
}

impl fmt::Display for ErroSql {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErroSql::Sintaxe { posicao, mensagem } => {
                write!(f, "erro de sintaxe na posição {posicao}: {mensagem}")
            }
            ErroSql::Semantica(m) => write!(f, "{m}"),
            ErroSql::Catalogo(e) => write!(f, "{e}"),
            ErroSql::MemoriaEsgotada { limite } => write!(
                f,
                "a consulta passou do limite de memória ({} MiB): use LIMIT, um WHERE mais \
                 restrito ou um índice que sirva ao ORDER BY",
                limite >> 20
            ),
        }
    }
}

impl std::error::Error for ErroSql {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ErroSql::Catalogo(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ErroCatalogo> for ErroSql {
    fn from(e: ErroCatalogo) -> Self {
        ErroSql::Catalogo(e)
    }
}

impl From<siouxdb_transacoes::ErroBanco> for ErroSql {
    fn from(e: siouxdb_transacoes::ErroBanco) -> Self {
        ErroSql::Catalogo(e.into())
    }
}

pub(crate) fn semantica<T>(m: impl Into<String>) -> Resultado<T> {
    Err(ErroSql::Semantica(m.into()))
}
