//! SiouxDB contra SQLite, na mesma máquina e no mesmo disco.
//!
//! ```text
//! cargo run --release -p siouxdb-sql --example contra_sqlite -- [pasta] [linhas]
//! ```
//!
//! Condições: os dois em arquivo no disco, cache de 64 MiB, páginas de 8 KiB (SiouxDB) e
//! 4 KiB (padrão do SQLite), SQLite em modo WAL. Durabilidade igual: `synchronous=FULL` ×
//! `Sincronia::Total` e `synchronous=NORMAL` × `Sincronia::Normal`. O SQLite usa comandos
//! preparados (como um programa de verdade usaria); o SiouxDB ainda não tem, então analisa
//! o SQL a cada execução. O SiouxDB roda com e sem cifra; o SQLite padrão não cifra.

use rusqlite::{Connection, params};
use siouxdb_armazenamento::{ParametrosSenha, SenhaNova, TamanhoPagina};
use siouxdb_catalogo::Valor;
use siouxdb_transacoes::{Banco, Config, Sincronia};
use siouxdb_vfs::{Vfs, VfsReal};
use std::path::{Path, PathBuf};
use std::time::Instant;

type Bd = Banco<<VfsReal as Vfs>::Arquivo>;

struct Sorteio(u64);
impl Sorteio {
    fn n(&mut self, max: u64) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) % max
    }
}

fn nome(i: u64) -> String {
    format!("cliente numero {:08}", (i * 7919) % 1_000_003)
}

/// Operações por segundo.
fn taxa(n: u64, t: Instant) -> f64 {
    n as f64 / t.elapsed().as_secs_f64()
}

trait Motor {
    fn rotulo(&self) -> String;
    /// `n` transações de 1 INSERT cada.
    fn inserir_uma_por_vez(&mut self, ini: u64, n: u64);
    /// `n` INSERTs numa transação só.
    fn inserir_em_lote(&mut self, ini: u64, n: u64);
    /// Uma consulta que devolve um inteiro (para conferir que os dois dão o mesmo).
    fn um_inteiro(&mut self, sql: &str, p: i64) -> i64;
    /// Uma consulta inteira; devolve quantas linhas vieram.
    fn linhas(&mut self, sql: &str) -> usize;
    fn executar(&mut self, sql: &str) -> u64;
}

const CRIAR: &str = "CREATE TABLE pedidos (id INTEGER PRIMARY KEY, cliente INTEGER, grupo INTEGER, total INTEGER, obs TEXT)";

struct Sioux {
    b: Bd,
    cifrado: bool,
    sincronia: Sincronia,
}

impl Sioux {
    fn novo(pasta: &Path, cifrado: bool, sincronia: Sincronia) -> Sioux {
        let caminho = pasta.join(format!("sioux-{cifrado}-{sincronia:?}.sdb"));
        let _ = std::fs::remove_file(&caminho);
        let _ = std::fs::remove_file(caminho.with_extension("sdb-wal"));
        let senha = cifrado.then_some(SenhaNova {
            senha: b"senha de teste 123",
            parametros: ParametrosSenha::para_testes(),
        });
        let config = Config {
            sincronia,
            cache_paginas: 8192, // 64 MiB
            ..Config::default()
        };
        let b = Bd::criar(&VfsReal::novo(), &caminho, TamanhoPagina::K8, senha, config).unwrap();
        let mut s = Sioux {
            b,
            cifrado,
            sincronia,
        };
        s.executar(CRIAR);
        s.executar("CREATE TABLE clientes (id INTEGER PRIMARY KEY, nome TEXT)");
        s
    }
}

fn linha_pedido(i: u64) -> [Valor; 5] {
    [
        Valor::Inteiro(i as i64),
        Valor::Inteiro((i % 2000) as i64),
        Valor::Inteiro((i % 100) as i64),
        Valor::Inteiro(((i * 104_729) % 100_000) as i64),
        Valor::Texto(nome(i)),
    ]
}

const INSERIR: &str = "INSERT INTO pedidos VALUES (?, ?, ?, ?, ?)";

impl Motor for Sioux {
    fn rotulo(&self) -> String {
        format!(
            "SiouxDB {} {}",
            if self.cifrado { "cifrado" } else { "sem cifra" },
            match self.sincronia {
                Sincronia::Total => "(Total)",
                Sincronia::Normal => "(Normal)",
            }
        )
    }
    fn inserir_uma_por_vez(&mut self, ini: u64, n: u64) {
        for i in ini..ini + n {
            let mut tx = self.b.escrever().unwrap();
            crate::executar_sioux(&mut tx, INSERIR, &linha_pedido(i));
            tx.confirmar().unwrap();
        }
    }
    fn inserir_em_lote(&mut self, ini: u64, n: u64) {
        let mut tx = self.b.escrever().unwrap();
        for i in ini..ini + n {
            crate::executar_sioux(&mut tx, INSERIR, &linha_pedido(i));
        }
        tx.confirmar().unwrap();
    }
    fn um_inteiro(&mut self, sql: &str, p: i64) -> i64 {
        let l = self.b.ler().unwrap();
        let s = siouxdb_sql::consultar(&l, sql, &[Valor::Inteiro(p)]).unwrap();
        match s.linhas.first().map(|l| &l[0]) {
            Some(Valor::Inteiro(i)) => *i,
            _ => -1,
        }
    }
    fn linhas(&mut self, sql: &str) -> usize {
        let l = self.b.ler().unwrap();
        siouxdb_sql::consultar(&l, sql, &[]).unwrap().linhas.len()
    }
    fn executar(&mut self, sql: &str) -> u64 {
        let mut tx = self.b.escrever().unwrap();
        let s = siouxdb_sql::executar(&mut tx, sql, &[]).unwrap();
        tx.confirmar().unwrap();
        s.iter().map(|x| x.afetadas).sum()
    }
}

fn executar_sioux(
    tx: &mut siouxdb_transacoes::Transacao<'_, <VfsReal as Vfs>::Arquivo>,
    sql: &str,
    p: &[Valor],
) {
    siouxdb_sql::executar(tx, sql, p).unwrap();
}

struct Lite {
    c: Connection,
    sincronia: Sincronia,
}

impl Lite {
    fn novo(pasta: &Path, sincronia: Sincronia) -> Lite {
        let caminho = pasta.join(format!("sqlite-{sincronia:?}.db"));
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", caminho.display()));
        }
        let c = Connection::open(&caminho).unwrap();
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
        c.pragma_update(
            None,
            "synchronous",
            if sincronia == Sincronia::Total {
                "FULL"
            } else {
                "NORMAL"
            },
        )
        .unwrap();
        c.pragma_update(None, "cache_size", -65536).unwrap(); // 64 MiB
        c.execute(CRIAR, []).unwrap();
        c.execute(
            "CREATE TABLE clientes (id INTEGER PRIMARY KEY, nome TEXT)",
            [],
        )
        .unwrap();
        Lite { c, sincronia }
    }
}

impl Motor for Lite {
    fn rotulo(&self) -> String {
        format!(
            "SQLite {} ({})",
            rusqlite::version(),
            if self.sincronia == Sincronia::Total {
                "FULL"
            } else {
                "NORMAL"
            }
        )
    }
    fn inserir_uma_por_vez(&mut self, ini: u64, n: u64) {
        let mut st = self.c.prepare_cached(INSERIR).unwrap();
        for i in ini..ini + n {
            st.execute(params![
                i as i64,
                (i % 2000) as i64,
                (i % 100) as i64,
                ((i * 104_729) % 100_000) as i64,
                nome(i)
            ])
            .unwrap();
        }
    }
    fn inserir_em_lote(&mut self, ini: u64, n: u64) {
        let tx = self.c.transaction().unwrap();
        {
            let mut st = tx.prepare_cached(INSERIR).unwrap();
            for i in ini..ini + n {
                st.execute(params![
                    i as i64,
                    (i % 2000) as i64,
                    (i % 100) as i64,
                    ((i * 104_729) % 100_000) as i64,
                    nome(i)
                ])
                .unwrap();
            }
        }
        tx.commit().unwrap();
    }
    fn um_inteiro(&mut self, sql: &str, p: i64) -> i64 {
        let mut st = self.c.prepare_cached(sql).unwrap();
        st.query_row([p], |r| r.get::<_, i64>(0)).unwrap_or(-1)
    }
    fn linhas(&mut self, sql: &str) -> usize {
        let mut st = self.c.prepare_cached(sql).unwrap();
        let n = st.column_count();
        let mut rs = st.query([]).unwrap();
        let mut k = 0;
        while let Some(r) = rs.next().unwrap() {
            for i in 0..n {
                let _ = r.get_ref(i).unwrap();
            }
            k += 1;
        }
        k
    }
    fn executar(&mut self, sql: &str) -> u64 {
        self.c.execute_batch(sql).unwrap();
        self.c.changes()
    }
}

/// Resultado de uma rodada: (nome da operação, operações por segundo, unidade).
fn rodada(m: &mut dyn Motor, linhas: u64) -> Vec<(&'static str, f64, &'static str)> {
    let mut r = Vec::new();
    let total = m_sinc(m);
    // 1. Uma linha por transação (o caso de um caixa gravando venda a venda).
    let n1 = if total { 300 } else { 3000 };
    let t = Instant::now();
    m.inserir_uma_por_vez(0, n1);
    r.push(("INSERT, 1 por transação", taxa(n1, t), "transações/s"));
    // 2. Carga em lote.
    let t = Instant::now();
    m.inserir_em_lote(n1, linhas - n1);
    r.push((
        "INSERT em lote (1 transação)",
        taxa(linhas - n1, t),
        "linhas/s",
    ));
    m.executar("CREATE INDEX pedidos_cliente ON pedidos (cliente)");
    let mut tx_cli = String::from("INSERT INTO clientes VALUES ");
    for c in 0..2000 {
        if c > 0 {
            tx_cli.push(',');
        }
        tx_cli.push_str(&format!("({c}, 'cliente {c}')"));
    }
    m.executar(&tx_cli);
    // 3. Busca pela chave primária.
    let mut s = Sorteio(42);
    let n = 50_000;
    let t = Instant::now();
    let mut soma = 0i64;
    for _ in 0..n {
        soma = soma.wrapping_add(
            m.um_inteiro("SELECT total FROM pedidos WHERE id = ?", s.n(linhas) as i64),
        );
    }
    r.push(("SELECT pela chave", taxa(n, t), "consultas/s"));
    // 4. Busca por índice secundário (devolve a contagem do cliente).
    let n = 5_000;
    let t = Instant::now();
    for _ in 0..n {
        soma = soma.wrapping_add(m.um_inteiro(
            "SELECT COUNT(*) FROM pedidos WHERE cliente = ?",
            s.n(2000) as i64,
        ));
    }
    r.push((
        "SELECT por índice (COUNT de um cliente)",
        taxa(n, t),
        "consultas/s",
    ));
    std::hint::black_box(soma);
    // 5. Varredura com agregação.
    let n = 5;
    let t = Instant::now();
    for _ in 0..n {
        assert_eq!(
            m.linhas("SELECT grupo, COUNT(*), SUM(total), MAX(total) FROM pedidos GROUP BY grupo"),
            100
        );
    }
    r.push((
        "GROUP BY na tabela inteira",
        taxa(n * linhas, t),
        "linhas lidas/s",
    ));
    // 6. Top 10.
    let n = 5;
    let t = Instant::now();
    for _ in 0..n {
        assert_eq!(
            m.linhas("SELECT id, total FROM pedidos ORDER BY total DESC LIMIT 10"),
            10
        );
    }
    r.push((
        "ORDER BY total DESC LIMIT 10",
        taxa(n * linhas, t),
        "linhas lidas/s",
    ));
    // 7. JOIN + GROUP BY.
    let n = 3;
    let t = Instant::now();
    for _ in 0..n {
        assert_eq!(
            m.linhas("SELECT c.nome, SUM(p.total) FROM pedidos p JOIN clientes c ON c.id = p.cliente GROUP BY c.nome"),
            2000
        );
    }
    r.push((
        "JOIN pedidos × clientes + GROUP BY",
        taxa(n * linhas, t),
        "linhas lidas/s",
    ));
    // 8. UPDATE de 10% numa transação.
    let t = Instant::now();
    let afetadas = m.executar("UPDATE pedidos SET total = total + 1 WHERE grupo < 10");
    r.push(("UPDATE de 10% (1 transação)", taxa(afetadas, t), "linhas/s"));
    r
}

fn m_sinc(m: &dyn Motor) -> bool {
    m.rotulo().contains("Total") || m.rotulo().contains("FULL")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let pasta = args
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("siouxdb-contra-sqlite"));
    let linhas: u64 = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(200_000);
    std::fs::create_dir_all(&pasta).unwrap();
    println!("{linhas} pedidos, pasta {}\n", pasta.display());
    for sincronia in [Sincronia::Normal, Sincronia::Total] {
        let mut motores: Vec<Box<dyn Motor>> = vec![
            Box::new(Lite::novo(&pasta, sincronia)),
            Box::new(Sioux::novo(&pasta, false, sincronia)),
            Box::new(Sioux::novo(&pasta, true, sincronia)),
        ];
        let mut tabela: Vec<(String, Vec<(&str, f64, &str)>)> = Vec::new();
        for m in motores.iter_mut() {
            let rotulo = m.rotulo();
            eprintln!("rodando {rotulo}...");
            tabela.push((rotulo, rodada(m.as_mut(), linhas)));
        }
        println!(
            "## Durabilidade {}",
            if sincronia == Sincronia::Total {
                "total (fsync por confirmação)"
            } else {
                "normal"
            }
        );
        print!("| Operação |");
        for (r, _) in &tabela {
            print!(" {r} |");
        }
        println!(" SiouxDB sem cifra ÷ SQLite |");
        print!("|---|");
        for _ in 0..=tabela.len() {
            print!("---|");
        }
        println!();
        for i in 0..tabela[0].1.len() {
            let (op, _, unid) = tabela[0].1[i];
            print!("| {op} ({unid}) |");
            for (_, v) in &tabela {
                print!(" {} |", formatar(v[i].1));
            }
            println!(" {:.2}× |", tabela[1].1[i].1 / tabela[0].1[i].1);
        }
        println!();
    }
    let _ = std::fs::remove_dir_all(&pasta);
}

fn formatar(v: f64) -> String {
    let i = v.round() as u64;
    let s = i.to_string();
    let mut out = String::new();
    for (k, c) in s.chars().enumerate() {
        if k > 0 && (s.len() - k) % 3 == 0 {
            out.push('.');
        }
        out.push(c);
    }
    out
}
