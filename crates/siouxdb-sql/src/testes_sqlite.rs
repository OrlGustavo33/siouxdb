//! Comparação com o SQLite: consultas aleatórias (determinísticas pela semente) rodam nos
//! dois bancos com os mesmos dados, e os resultados têm de ser iguais.
//!
//! O gerador fica dentro do que os dois tratam igual. Diferenças **propositais** do
//! SiouxDB ficam de fora (estão em `docs/DIFERENCAS_SQLITE.md`): divisão por zero dá erro (o
//! SQLite devolve NULL), comparar tipos diferentes dá erro, `LIKE` diferencia maiúsculas,
//! booleano é um tipo (o SQLite usa 0/1, aqui normalizado na comparação).
//!
//! `SIOUX_SIM_FATOR=10 cargo test -p siouxdb-sql --release sqlite` roda 10× mais.

use crate::{Saida, consultar, executar};
use siouxdb_armazenamento::TamanhoPagina;
use siouxdb_catalogo::Valor;
use siouxdb_transacoes::{Banco, Config};
use siouxdb_vfs::{ConfigSimulacao, Vfs, VfsSimulado};
use std::cmp::Ordering;
use std::path::PathBuf;

type B = Banco<<VfsSimulado as Vfs>::Arquivo>;

struct Sorteio(u64);

impl Sorteio {
    fn n(&mut self, max: u64) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) % max.max(1)
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.n(100) < pct
    }
    fn um<'a>(&mut self, v: &[&'a str]) -> &'a str {
        v[self.n(v.len() as u64) as usize]
    }
}

const PALAVRAS: [&str; 8] = [
    "ana",
    "bia",
    "carlos",
    "",
    "a_b",
    "abc",
    "zeca",
    "ana maria",
];
const REAIS: [&str; 6] = ["0.5", "1.25", "-2.75", "3.0", "10.5", "0.0"];

/// Valor normalizado para comparar: booleano vira inteiro (o SQLite não tem booleano).
#[derive(Debug, Clone)]
enum Norm {
    Nulo,
    Num(f64),
    Texto(String),
}

impl PartialEq for Norm {
    fn eq(&self, o: &Norm) -> bool {
        match (self, o) {
            (Norm::Nulo, Norm::Nulo) => true,
            (Norm::Num(a), Norm::Num(b)) => (a - b).abs() <= 1e-9 * (1.0 + a.abs()),
            (Norm::Texto(a), Norm::Texto(b)) => a == b,
            _ => false,
        }
    }
}

fn ordem(a: &Norm, b: &Norm) -> Ordering {
    let posto = |v: &Norm| match v {
        Norm::Nulo => 0,
        Norm::Num(_) => 1,
        Norm::Texto(_) => 2,
    };
    match (a, b) {
        (Norm::Num(x), Norm::Num(y)) => x.total_cmp(y),
        (Norm::Texto(x), Norm::Texto(y)) => x.cmp(y),
        _ => posto(a).cmp(&posto(b)),
    }
}

fn de_sioux(v: &Valor) -> Norm {
    match v {
        Valor::Nulo => Norm::Nulo,
        Valor::Inteiro(i) => Norm::Num(*i as f64),
        Valor::Real(r) => Norm::Num(*r),
        Valor::Booleano(b) => Norm::Num(f64::from(u8::from(*b))),
        Valor::Texto(t) => Norm::Texto(t.clone()),
        outro => Norm::Texto(format!("{outro:?}")),
    }
}

fn de_sqlite(v: rusqlite::types::ValueRef<'_>) -> Norm {
    use rusqlite::types::ValueRef;
    match v {
        ValueRef::Null => Norm::Nulo,
        ValueRef::Integer(i) => Norm::Num(i as f64),
        ValueRef::Real(r) => Norm::Num(r),
        ValueRef::Text(t) => Norm::Texto(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Norm::Texto(format!("{b:?}")),
    }
}

/// Gera dados e consultas. `qual` = colunas com prefixo da tabela (consulta com JOIN).
struct Gerador {
    s: Sorteio,
    qual: bool,
}

impl Gerador {
    fn col(&self, tabela: &str, c: &str) -> String {
        if self.qual {
            format!("{tabela}.{c}")
        } else {
            c.to_string()
        }
    }

    fn inteiro(&mut self, prof: u32) -> String {
        let r = self.s.n(if prof == 0 { 3 } else { 14 });
        match r {
            0 => {
                let c = self.s.um(&["a", "b", "id"]);
                self.col("t1", c)
            }
            1 if self.qual => {
                let c = self.s.um(&["e", "t1_id", "id"]);
                format!("t2.{c}")
            }
            1 => self.col("t1", "a"),
            2 => format!("{}", self.s.n(25) as i64 - 5),
            3 | 4 => {
                let op = self.s.um(&["+", "-", "*"]);
                format!(
                    "({} {op} {})",
                    self.inteiro(prof - 1),
                    self.inteiro(prof - 1)
                )
            }
            5 => format!("ABS({})", self.inteiro(prof - 1)),
            6 => format!("COALESCE({}, {})", self.inteiro(prof - 1), self.s.n(10)),
            7 => format!("LENGTH({})", self.texto(prof - 1)),
            8 => {
                // Divisão e resto inteiros (divisor constante, nunca zero).
                let op = self.s.um(&["/", "%"]);
                let k = self.s.um(&["1", "2", "3", "7", "-2", "-5"]);
                format!("({} {op} {k})", self.inteiro(prof - 1))
            }
            9 => format!("-{}", self.inteiro(prof - 1)),
            11 => format!(
                "INSTR({}, '{}')",
                self.texto(prof - 1),
                self.s.um(&["a", "an", "", "z", "_"])
            ),
            12 => format!("IFNULL({}, {})", self.inteiro(prof - 1), self.inteiro(0)),
            13 => format!("NULLIF({}, {})", self.inteiro(prof - 1), self.s.n(6)),
            _ => format!(
                "COALESCE({}, {})",
                self.inteiro(prof - 1),
                self.inteiro(prof - 1)
            ),
        }
    }

    fn texto(&mut self, prof: u32) -> String {
        match self.s.n(if prof == 0 { 2 } else { 9 }) {
            5 => {
                let ini = self.s.n(9) as i64 - 4;
                if self.s.chance(50) {
                    format!(
                        "SUBSTR({}, {ini}, {})",
                        self.texto(prof - 1),
                        self.s.n(9) as i64 - 3
                    )
                } else {
                    format!("SUBSTR({}, {ini})", self.texto(prof - 1))
                }
            }
            6 => {
                let f = self.s.um(&["TRIM", "LTRIM", "RTRIM"]);
                if self.s.chance(50) {
                    format!("{f}({})", self.texto(prof - 1))
                } else {
                    format!(
                        "{f}({}, '{}')",
                        self.texto(prof - 1),
                        self.s.um(&["a", "ab", " _", "z"])
                    )
                }
            }
            7 => format!(
                "REPLACE({}, '{}', '{}')",
                self.texto(prof - 1),
                self.s.um(&["a", "an", "", "_"]),
                self.s.um(&["", "X", "aa"])
            ),
            8 => format!("IFNULL({}, 'x')", self.texto(prof - 1)),
            0 => self.col("t1", "c"),
            1 => format!("'{}'", self.s.um(&PALAVRAS)),
            2 if self.s.chance(50) => {
                format!("({} || {})", self.texto(prof - 1), self.texto(prof - 1))
            }
            2 => format!("UPPER({})", self.texto(prof - 1)),
            3 => format!("LOWER({})", self.texto(prof - 1)),
            _ => format!("COALESCE({}, 'nulo')", self.texto(prof - 1)),
        }
    }

    fn real(&mut self) -> String {
        match self.s.n(8) {
            0 | 1 => self.col("t1", "d"),
            2 => self.s.um(&REAIS).to_string(),
            3 => format!("({} * 2)", self.col("t1", "d")),
            // Inteiro com real vira real.
            4 => format!("({} + {})", self.col("t1", "a"), self.col("t1", "d")),
            5 => format!("({} / 2.0)", self.inteiro(1)),
            6 => format!("ROUND({})", self.real_simples()),
            _ => format!("ROUND({}, {})", self.real_simples(), self.s.n(4)),
        }
    }

    /// Reais com dízimas e meios (casos difíceis do ROUND).
    fn real_simples(&mut self) -> String {
        match self.s.n(4) {
            0 => format!("({} / 3.0)", self.inteiro(0)),
            1 => format!("({} / 8.0)", self.inteiro(0)),
            2 => self
                .s
                .um(&[
                    "2.675",
                    "0.125",
                    "-2.5",
                    "1.005",
                    "0.5",
                    "-0.5",
                    "2.5",
                    "1234.5678",
                ])
                .to_string(),
            _ => format!("({} * 1.1)", self.col("t1", "d")),
        }
    }

    fn predicado(&mut self, prof: u32) -> String {
        let r = self.s.n(if prof == 0 { 7 } else { 14 });
        let cmp = self.s.um(&["=", "<>", "<", "<=", ">", ">="]);
        match r {
            0 | 1 => format!("{} {cmp} {}", self.inteiro(1), self.inteiro(1)),
            2 => format!("{} {cmp} {}", self.texto(1), self.texto(0)),
            3 => {
                let x = if self.s.chance(50) {
                    self.inteiro(1)
                } else {
                    self.texto(0)
                };
                let nao = if self.s.chance(50) { " NOT" } else { "" };
                format!("{x} IS{nao} NULL")
            }
            4 => {
                let lo = self.s.n(15) as i64 - 5;
                let nao = if self.s.chance(30) { " NOT" } else { "" };
                format!(
                    "{}{nao} BETWEEN {lo} AND {}",
                    self.inteiro(1),
                    lo + self.s.n(10) as i64
                )
            }
            5 => {
                let nao = if self.s.chance(30) { " NOT" } else { "" };
                let l: Vec<String> = (0..1 + self.s.n(4))
                    .map(|_| format!("{}", self.s.n(20) as i64 - 3))
                    .collect();
                format!("{}{nao} IN ({})", self.inteiro(1), l.join(", "))
            }
            6 => {
                let p = self
                    .s
                    .um(&["a%", "%a", "%a%", "_n_", "ana%", "%", "a\\_b", "z%a"]);
                let nao = if self.s.chance(30) { " NOT" } else { "" };
                format!("{}{nao} LIKE '{p}'", self.col("t1", "c"))
            }
            7 => format!("NOT ({})", self.predicado(prof - 1)),
            10 | 11 => {
                // Coluna contra constante: o planejador usa chave, índice ou faixa.
                let (c, v) = match self.s.n(5) {
                    0 => ("id", format!("{}", self.s.n(60))),
                    1 => ("a", format!("{}", self.s.n(12) as i64 - 3)),
                    2 => ("a", self.s.um(&["2.5", "-0.5", "3.0", "8.75"]).to_string()),
                    3 => ("id", self.s.um(&["10.5", "0.5", "25.0"]).to_string()),
                    _ => ("c", format!("'{}'", self.s.um(&PALAVRAS))),
                };
                let c = self.col("t1", c);
                format!("{c} {cmp} {v}")
            }
            12 => {
                // IN com NULL: lógica de três valores.
                let nao = if self.s.chance(50) { " NOT" } else { "" };
                format!(
                    "{}{nao} IN ({}, NULL, {})",
                    self.inteiro(1),
                    self.s.n(8),
                    self.s.n(8)
                )
            }
            13 => format!("{} {cmp} {}", self.real(), self.inteiro(0)),
            8 => format!(
                "({} AND {})",
                self.predicado(prof - 1),
                self.predicado(prof - 1)
            ),
            _ => format!(
                "({} OR {})",
                self.predicado(prof - 1),
                self.predicado(prof - 1)
            ),
        }
    }

    fn saida(&mut self) -> String {
        match self.s.n(4) {
            0 | 1 => self.inteiro(2),
            2 => self.texto(1),
            _ => self.real(),
        }
    }

    /// Devolve o SQL e se a ordem do resultado é garantida (então compara linha a linha).
    fn consulta(&mut self) -> (String, bool) {
        match self.s.n(10) {
            0..=3 => self.simples(),
            4..=6 => self.agrupada(),
            7 => self.agregada(),
            _ => self.juncao(),
        }
    }

    fn onde(&mut self) -> String {
        if self.s.chance(75) {
            format!(" WHERE {}", self.predicado(2))
        } else {
            String::new()
        }
    }

    fn limite(&mut self) -> String {
        if self.s.chance(50) {
            let off = if self.s.chance(40) {
                format!(" OFFSET {}", self.s.n(8))
            } else {
                String::new()
            };
            format!(" LIMIT {}{off}", self.s.n(12))
        } else {
            String::new()
        }
    }

    fn simples(&mut self) -> (String, bool) {
        self.qual = false;
        let itens: Vec<String> = (0..1 + self.s.n(3))
            .enumerate()
            .map(|(i, _)| format!("{} AS s{i}", self.saida()))
            .collect();
        let onde = self.onde();
        if self.s.chance(60) {
            let quantos = itens.len() as u64;
            let mut chaves: Vec<String> = (0..self.s.n(3))
                .map(|_| {
                    let e = match self.s.n(3) {
                        0 => format!("s{}", self.s.n(quantos)),
                        1 => format!("{}", 1 + self.s.n(quantos)),
                        _ => self.saida(),
                    };
                    let d = if self.s.chance(50) { " DESC" } else { "" };
                    format!("{e}{d}")
                })
                .collect();
            chaves.push("id".into()); // desempate: ordem total
            let lim = self.limite();
            (
                format!(
                    "SELECT {} FROM t1{onde} ORDER BY {}{lim}",
                    itens.join(", "),
                    chaves.join(", ")
                ),
                true,
            )
        } else {
            (format!("SELECT {} FROM t1{onde}", itens.join(", ")), false)
        }
    }

    fn agregado(&mut self) -> String {
        match self.s.n(9) {
            0 => "COUNT(*)".into(),
            1 => format!("COUNT({})", self.saida()),
            2 => format!("SUM({})", self.inteiro(1)),
            3 => format!("MIN({})", self.inteiro(1)),
            4 => format!("MAX({})", self.texto(0)),
            5 => format!("AVG({})", self.inteiro(1)),
            6 => format!("SUM({})", self.real()),
            7 => format!("MIN({})", self.texto(0)),
            _ => format!("MAX({})", self.real()),
        }
    }

    fn agrupada(&mut self) -> (String, bool) {
        self.qual = false;
        let grupos: Vec<String> = (0..1 + self.s.n(2))
            .map(|_| {
                let c = self.s.um(&["a", "b", "c", "a % 3"]);
                c.replace("a % 3", "(a * 2)")
            })
            .collect();
        let mut itens = grupos.clone();
        if self.s.chance(30) && grupos[0] != "c" {
            itens.push(format!("({} + 1)", grupos[0]));
        }
        itens.extend((0..1 + self.s.n(3)).map(|_| self.agregado()));
        let onde = self.onde();
        let tendo = if self.s.chance(40) {
            format!(
                " HAVING COUNT(*) {} {}",
                self.s.um(&[">", ">=", "<", "="]),
                self.s.n(4)
            )
        } else {
            String::new()
        };
        let sql = format!(
            "SELECT {} FROM t1{onde} GROUP BY {}{tendo}",
            itens.join(", "),
            grupos.join(", ")
        );
        if self.s.chance(60) {
            // Os grupos são únicos: ordenar por todos dá ordem total.
            let lim = self.limite();
            (format!("{sql} ORDER BY {}{lim}", grupos.join(", ")), true)
        } else {
            (sql, false)
        }
    }

    fn agregada(&mut self) -> (String, bool) {
        self.qual = false;
        let itens: Vec<String> = (0..1 + self.s.n(3)).map(|_| self.agregado()).collect();
        let onde = self.onde();
        (format!("SELECT {} FROM t1{onde}", itens.join(", ")), true)
    }

    fn juncao(&mut self) -> (String, bool) {
        self.qual = true;
        let esquerda = if self.s.chance(50) { "LEFT " } else { "" };
        let mut on = "t2.t1_id = t1.id".to_string();
        if self.s.chance(30) {
            on = format!("{on} AND t2.e > {}", self.s.n(10));
        }
        let itens = [
            "t1.id".to_string(),
            "t2.id".to_string(),
            self.saida(),
            if self.s.chance(50) {
                "t2.f".into()
            } else {
                "t2.e".into()
            },
        ];
        let onde = self.onde();
        let lim = self.limite();
        (
            format!(
                "SELECT {} FROM t1 {esquerda}JOIN t2 ON {on}{onde} ORDER BY t1.id, t2.id{lim}",
                itens.join(", ")
            ),
            true,
        )
    }
}

/// Cria as tabelas e os mesmos dados nos dois bancos.
fn preparar(semente: u64) -> (VfsSimulado, B, rusqlite::Connection) {
    let vfs = VfsSimulado::novo(ConfigSimulacao::default());
    let b = B::criar(
        &vfs,
        &PathBuf::from("d.sdb"),
        TamanhoPagina::K4,
        None,
        Config::default(),
    )
    .unwrap();
    let lite = rusqlite::Connection::open_in_memory().unwrap();
    let mut s = Sorteio(semente.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut cmds = vec![
        "CREATE TABLE t1 (id INTEGER PRIMARY KEY, a INTEGER, b INTEGER, c TEXT, d REAL)"
            .to_string(),
        "CREATE TABLE t2 (id INTEGER PRIMARY KEY, t1_id INTEGER, e INTEGER, f TEXT)".to_string(),
        "CREATE INDEX t1_a ON t1 (a)".to_string(),
        "CREATE INDEX t1_c ON t1 (c)".to_string(),
        "CREATE INDEX t2_t1 ON t2 (t1_id)".to_string(),
    ];
    fn nulo_ou(s: &mut Sorteio, v: String) -> String {
        if s.chance(15) { "NULL".to_string() } else { v }
    }
    for id in 1..=(20 + s.n(30)) {
        let v = [
            format!("{}", s.n(12) as i64 - 3),
            format!("{}", s.n(30) as i64 - 10),
            format!("'{}'", s.um(&PALAVRAS)),
            s.um(&REAIS).to_string(),
        ];
        let v: Vec<String> = v.into_iter().map(|x| nulo_ou(&mut s, x)).collect();
        cmds.push(format!("INSERT INTO t1 VALUES ({id}, {})", v.join(", ")));
    }
    for id in 1..=(30 + s.n(40)) {
        let v = [
            format!("{}", 1 + s.n(55)),
            format!("{}", s.n(15)),
            format!("'{}'", s.um(&PALAVRAS)),
        ];
        let v: Vec<String> = v.into_iter().map(|x| nulo_ou(&mut s, x)).collect();
        cmds.push(format!("INSERT INTO t2 VALUES ({id}, {})", v.join(", ")));
    }
    let mut tx = b.escrever().unwrap();
    for c in &cmds {
        executar(&mut tx, c, &[]).unwrap_or_else(|e| panic!("{c}: {e}"));
        lite.execute(c, [])
            .unwrap_or_else(|e| panic!("sqlite {c}: {e}"));
    }
    tx.confirmar().unwrap();
    (vfs, b, lite)
}

fn rodar_sqlite(lite: &rusqlite::Connection, sql: &str) -> Result<Vec<Vec<Norm>>, String> {
    let mut st = lite.prepare(sql).map_err(|e| e.to_string())?;
    let n = st.column_count();
    let mut linhas = Vec::new();
    let mut rs = st.query([]).map_err(|e| e.to_string())?;
    while let Some(r) = rs.next().map_err(|e| e.to_string())? {
        linhas.push((0..n).map(|i| de_sqlite(r.get_ref(i).unwrap())).collect());
    }
    Ok(linhas)
}

fn rodar_sioux(b: &B, sql: &str) -> Result<Vec<Vec<Norm>>, String> {
    let l = b.ler().unwrap();
    consultar(&l, sql, &[])
        .map(|s: Saida| {
            s.linhas
                .iter()
                .map(|l| l.iter().map(de_sioux).collect())
                .collect()
        })
        .map_err(|e| e.to_string())
}

fn ordenar(v: &mut [Vec<Norm>]) {
    v.sort_by(|a, b| {
        a.iter()
            .zip(b)
            .map(|(x, y)| ordem(x, y))
            .find(|o| *o != Ordering::Equal)
            .unwrap_or(Ordering::Equal)
    });
}

fn fator() -> u64 {
    std::env::var("SIOUX_SIM_FATOR")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
}

#[test]
fn mesmos_resultados_que_o_sqlite() {
    let mut consultas = 0;
    let (mut com_linhas, mut vazias, mut erro_nos_dois) = (0u64, 0u64, 0u64);
    let mut diferencas = Vec::new();
    for semente in 0..12 * fator() {
        let (_vfs, b, lite) = preparar(semente);
        let mut g = Gerador {
            s: Sorteio(semente.wrapping_mul(0xD1B5_4A32_D192_ED03) | 1),
            qual: false,
        };
        for _ in 0..250 {
            let (sql, ordenada) = g.consulta();
            consultas += 1;
            let (mut nos, mut eles) = (rodar_sioux(&b, &sql), rodar_sqlite(&lite, &sql));
            if !ordenada {
                if let (Ok(x), Ok(y)) = (&mut nos, &mut eles) {
                    ordenar(x);
                    ordenar(y);
                }
            }
            let igual = match (&nos, &eles) {
                (Ok(x), Ok(y)) => {
                    if y.is_empty() {
                        vazias += 1;
                    } else {
                        com_linhas += 1;
                    }
                    x == y
                }
                (Err(_), Err(_)) => {
                    erro_nos_dois += 1;
                    true
                }
                _ => false,
            };
            if !igual && diferencas.len() < 15 {
                diferencas.push(format!(
                    "semente {semente}: {sql}\n  siouxdb: {nos:?}\n  sqlite:  {eles:?}"
                ));
            }
        }
    }
    eprintln!(
        "{consultas} consultas: {com_linhas} com linhas, {vazias} vazias, {erro_nos_dois} com erro nos dois"
    );
    // O gerador tem de produzir consultas que trazem dados, senão o teste não prova nada.
    assert!(com_linhas * 2 > consultas, "poucas consultas com resultado");
    assert!(
        diferencas.is_empty(),
        "{} diferença(s) em {consultas} consultas:\n{}",
        diferencas.len(),
        diferencas.join("\n\n")
    );
}
