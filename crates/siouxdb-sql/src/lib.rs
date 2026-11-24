//! SQL do SiouxDB.
//!
//! Subconjunto prático, com tipos em português ou inglês:
//!
//! ```sql
//! CREATE TABLE [IF NOT EXISTS] clientes (id INTEIRO PRIMARY KEY, nome TEXTO NOT NULL, saldo REAL);
//! CREATE [UNIQUE] INDEX [IF NOT EXISTS] clientes_nome ON clientes (nome);
//! INSERT [OR REPLACE | OR IGNORE] INTO clientes (id, nome) VALUES (1, 'ANA'), (?, ?)
//!     [ON CONFLICT DO NOTHING | ON CONFLICT DO UPDATE];
//! SELECT nome, saldo FROM clientes [AS OF 120] WHERE saldo > 10 AND nome LIKE 'A%'
//!     [GROUP BY ...] [HAVING ...] ORDER BY saldo DESC LIMIT 10 OFFSET 20;
//! SELECT cidade, COUNT(*), SUM(saldo), AVG(saldo), MIN(nome), MAX(nome) FROM ... GROUP BY cidade;
//! UPDATE clientes SET saldo = saldo + 1 WHERE id = $1;
//! DELETE FROM clientes WHERE saldo IS NULL;
//! DROP TABLE|INDEX [IF EXISTS] nome;  EXPLAIN SELECT ...;
//! VERIFY ['lsn:hash'];  -- confere o livro-razão (e a âncora guardada fora)
//! SELECT * FROM sys_tabelas;  -- e sys_colunas, sys_indices
//! ```
//!
//! O planejador usa a chave primária (ponto ou faixa) ou um índice secundário quando o WHERE
//! tem `coluna = constante` ou faixas; senão, varre a tabela. O filtro completo sempre é
//! aplicado depois, então o plano só muda a velocidade, nunca o resultado. `AS OF <lsn>` lê
//! a tabela como estava naquela confirmação.

mod avaliar;
mod erro;
mod executor;
mod lexico;
mod memoria;
mod parser;

pub use erro::{ErroSql, Resultado};
pub use memoria::{LIMITE_PADRAO, com_limite_memoria, definir_limite_memoria, limite_memoria};
use siouxdb_catalogo::Valor;
use siouxdb_transacoes::{Paginas, PaginasMut};

/// Resultado de um comando.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Saida {
    /// Nomes das colunas (vazio em comandos que não devolvem linhas).
    pub colunas: Vec<String>,
    /// Linhas devolvidas.
    pub linhas: Vec<Vec<Valor>>,
    /// Linhas inseridas, alteradas ou removidas.
    pub afetadas: u64,
    /// Num INSERT em tabela com numeração automática: o último número usado.
    pub ultimo_id: Option<i64>,
}

/// Todos os comandos do texto só leem? (Para escolher entre leitura e transação.)
pub fn somente_leitura(sql: &str) -> Resultado<bool> {
    Ok(executor::comandos(sql)?
        .iter()
        .all(parser::Comando::so_leitura))
}

/// Executa um SELECT (ou EXPLAIN) numa leitura (foto do banco).
pub fn consultar<P: Paginas + ?Sized>(p: &P, sql: &str, params: &[Valor]) -> Resultado<Saida> {
    let cmds = executor::comandos(sql)?;
    if cmds.len() != 1 {
        return Err(ErroSql::Semantica("consultar recebe um comando só".into()));
    }
    executor::consultar_comando(p, &cmds[0], params)
}

/// Executa um ou mais comandos só de leitura (SELECT, EXPLAIN, VERIFY), todos na mesma foto.
pub fn consultar_todos<P: Paginas + ?Sized>(
    p: &P,
    sql: &str,
    params: &[Valor],
) -> Resultado<Vec<Saida>> {
    executor::comandos(sql)?
        .iter()
        .map(|c| executor::consultar_comando(p, c, params))
        .collect()
}

/// Executa um ou mais comandos (separados por `;`) dentro da transação. Nada é confirmado
/// aqui: quem chama confirma (tudo vale junto) ou desfaz.
pub fn executar<P: PaginasMut + ?Sized>(
    tx: &mut P,
    sql: &str,
    params: &[Valor],
) -> Resultado<Vec<Saida>> {
    executor::comandos(sql)?
        .iter()
        .map(|c| executor::executar_comando(tx, c, params))
        .collect()
}

#[cfg(test)]
mod testes;
