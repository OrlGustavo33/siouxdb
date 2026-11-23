use siouxdb_indices::ErroIndice;
use siouxdb_transacoes::ErroBanco;
use std::fmt;

/// Resultado das operações de tabela e catálogo.
pub type Resultado<T> = Result<T, ErroCatalogo>;

/// Erros de tabela e catálogo.
#[derive(Debug)]
pub enum ErroCatalogo {
    /// Erro do banco (disco, cifra, transação).
    Banco(ErroBanco),
    /// Erro do índice.
    Indice(ErroIndice),
    /// Já existe tabela com esse nome.
    TabelaJaExiste(String),
    /// Não existe tabela com esse nome.
    TabelaNaoExiste(String),
    /// Nome de tabela ou coluna fora das regras.
    NomeInvalido(String),
    /// Definição de tabela inválida (sem coluna, sem chave, coluna repetida...).
    DefinicaoInvalida(String),
    /// A linha não bate com as colunas da tabela.
    LinhaInvalida(String),
    /// Já existe linha com essa chave primária.
    ChaveDuplicada,
    /// A linha (com a chave) não cabe numa página. Valores grandes fora da página ainda não
    /// existem.
    LinhaGrandeDemais {
        /// Tamanho do registro.
        tamanho: usize,
        /// Máximo neste tamanho de página.
        maximo: usize,
    },
    /// Dado gravado com forma inesperada. Indica defeito do programa, não do disco.
    Corrompido(&'static str),
}

impl fmt::Display for ErroCatalogo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErroCatalogo::Banco(e) => write!(f, "{e}"),
            ErroCatalogo::Indice(e) => write!(f, "{e}"),
            ErroCatalogo::TabelaJaExiste(n) => write!(f, "a tabela \"{n}\" já existe"),
            ErroCatalogo::TabelaNaoExiste(n) => write!(f, "a tabela \"{n}\" não existe"),
            ErroCatalogo::NomeInvalido(m) => write!(f, "nome inválido: {m}"),
            ErroCatalogo::DefinicaoInvalida(m) => write!(f, "definição de tabela inválida: {m}"),
            ErroCatalogo::LinhaInvalida(m) => write!(f, "linha inválida: {m}"),
            ErroCatalogo::ChaveDuplicada => {
                write!(f, "já existe uma linha com essa chave primária")
            }
            ErroCatalogo::LinhaGrandeDemais { tamanho, maximo } => {
                write!(
                    f,
                    "linha de {tamanho} bytes não cabe na página (máximo {maximo})"
                )
            }
            ErroCatalogo::Corrompido(m) => write!(f, "dado interno inválido: {m}"),
        }
    }
}

impl std::error::Error for ErroCatalogo {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ErroCatalogo::Banco(e) => Some(e),
            ErroCatalogo::Indice(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ErroBanco> for ErroCatalogo {
    fn from(e: ErroBanco) -> Self {
        ErroCatalogo::Banco(e)
    }
}

impl From<ErroIndice> for ErroCatalogo {
    fn from(e: ErroIndice) -> Self {
        match e {
            ErroIndice::Banco(b) => ErroCatalogo::Banco(b),
            outro => ErroCatalogo::Indice(outro),
        }
    }
}
