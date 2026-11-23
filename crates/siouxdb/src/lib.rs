//! # SiouxDB
//!
//! Banco de dados relacional embutido, num arquivo só: cifrado por padrão, verificável
//! (cadeia de hashes) e com histórico consultável (`AS OF`).
//!
//! **Estado: pré-alfa.** Hoje existem a camada de disco ([`vfs`]), o armazenamento em
//! páginas verificadas e cifradas ([`armazenamento`]) e as transações com WAL
//! ([`transacoes`]), a árvore B+ ([`indices`]) e as tabelas com catálogo ([`catalogo`]).
//! e o [`sql`]. O caminho até a v0.1
//! está em `docs/PLANO_IMPLEMENTACAO.md`. Não use para guardar dados reais.

/// Camada de acesso a disco (real e simulada).
pub use siouxdb_vfs as vfs;

/// Páginas verificadas, cabeçalho duplo à prova de queda e cache SIEVE.
pub use siouxdb_armazenamento as armazenamento;

/// Transações: WAL, escritor único com fila justa, confirmação em grupo, leitores com foto.
pub use siouxdb_transacoes as transacoes;

/// Índices: árvore B+ em páginas.
pub use siouxdb_indices as indices;

/// Tabelas versionadas (log quente + índice da chave primária) e catálogo `sys_*`.
pub use siouxdb_catalogo as catalogo;

/// SQL: CREATE/DROP TABLE e INDEX, INSERT, SELECT (WHERE, GROUP BY, ORDER BY, AS OF), UPDATE, DELETE.
pub use siouxdb_sql as sql;

/// Contadores, histogramas e o coletor do painel em tempo real.
pub use siouxdb_metricas as metricas;

/// Versão desta biblioteca.
pub const VERSAO: &str = env!("CARGO_PKG_VERSION");

/// Versão do formato de arquivo que esta biblioteca grava (a 1 está congelada).
pub const VERSAO_FORMATO: u32 = siouxdb_armazenamento::VERSAO_FORMATO;

pub mod manutencao;

/// Banco num arquivo do disco de verdade (o tipo que um programa normalmente usa).
pub type BancoArquivo = transacoes::Banco<<vfs::VfsReal as vfs::Vfs>::Arquivo>;

/// Abre o banco em `caminho`, ou cria se não existir (cifrado se `senha` vier; páginas de
/// 8 KiB; senha com o custo padrão do Argon2id).
pub fn abrir_ou_criar(
    caminho: &std::path::Path,
    senha: Option<&[u8]>,
    config: transacoes::Config,
) -> transacoes::Resultado<BancoArquivo> {
    let vfs = vfs::VfsReal::novo();
    if caminho.exists() {
        return BancoArquivo::abrir(&vfs, caminho, senha, config);
    }
    let senha = senha.map(|s| armazenamento::SenhaNova {
        senha: s,
        parametros: armazenamento::ParametrosSenha::default(),
    });
    BancoArquivo::criar(
        &vfs,
        caminho,
        armazenamento::TamanhoPagina::K8,
        senha,
        config,
    )
}
