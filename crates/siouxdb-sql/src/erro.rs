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
}

impl fmt::Display for ErroSql {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErroSql::Sintaxe { posicao, mensagem } => {
                write!(f, "erro de sintaxe na posição {posicao}: {mensagem}")
            }
            ErroSql::Semantica(m) => write!(f, "{m}"),
            ErroSql::Catalogo(e) => write!(f, "{e}"),
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
