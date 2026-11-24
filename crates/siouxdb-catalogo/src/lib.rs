//! Tabelas do SiouxDB.
//!
//! - [`valor`]: tipos, valores e as codificações de chave (ordenável) e de linha;
//! - [`log`]: a camada quente — um livro-razão em que toda gravação **anexa** uma versão e
//!   nada é sobrescrito;
//! - [`tabela`]: tabela versionada, com índice da chave primária (árvore B+), histórico por
//!   linha e leitura "como estava no LSN tal";
//! - [`catalogo`]: criar, abrir, listar e apagar tabelas, e as visões `sys_tabelas` e
//!   `sys_colunas`.
//!
//! Tudo funciona sobre [`siouxdb_transacoes::Paginas`]: numa leitura com foto ou dentro de
//! uma transação.

pub mod catalogo;
pub mod datahora;
pub mod decimal;
mod erro;
pub mod log;
pub mod tabela;
pub mod valor;

pub use decimal::Decimal;
pub use erro::{ErroCatalogo, Resultado};
pub use tabela::{
    Coluna, Contagem, DefIndice, DefTabela, Padrao, ResumoCompactacao, Tabela, Varredura,
    VarreduraIndice, VersaoLinha, fim_do_prefixo,
};
pub use valor::{Tipo, Valor};

#[cfg(test)]
mod testes;
