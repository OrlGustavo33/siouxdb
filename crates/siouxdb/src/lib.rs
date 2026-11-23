//! # SiouxDB
//!
//! Banco de dados relacional embutido, num arquivo só: cifrado por padrão, verificável
//! (cadeia de hashes) e com histórico consultável (`AS OF`).
//!
//! **Estado: pré-alfa.** Hoje existem a camada de disco ([`vfs`]) e o armazenamento em
//! páginas verificadas ([`armazenamento`]). O caminho até a v0.1
//! está em `docs/PLANO_IMPLEMENTACAO.md`. Não use para guardar dados reais.

/// Camada de acesso a disco (real e simulada).
pub use siouxdb_vfs as vfs;

/// Páginas verificadas, cabeçalho duplo à prova de queda e cache SIEVE.
pub use siouxdb_armazenamento as armazenamento;

/// Versão desta biblioteca.
pub const VERSAO: &str = env!("CARGO_PKG_VERSION");

/// Versão do formato de arquivo que esta biblioteca grava. Zero = ainda instável.
pub const VERSAO_FORMATO: u32 = siouxdb_armazenamento::VERSAO_FORMATO;
