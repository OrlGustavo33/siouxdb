//! Índices do SiouxDB.
//!
//! Hoje: a [`arvore`] B+ em páginas, que serve de índice da chave primária das tabelas e de
//! estrutura do catálogo. Os índices ART, adaptativo e aprendido (PGM) entram na v0.4, por
//! cima da mesma interface de páginas ([`siouxdb_transacoes::Paginas`]).

pub mod arvore;
mod erro;
mod no;

pub use erro::{ErroIndice, Resultado};

#[cfg(test)]
mod testes;
