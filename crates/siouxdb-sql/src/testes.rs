use crate::{ErroSql, Saida, consultar, executar, somente_leitura};
use siouxdb_armazenamento::TamanhoPagina;
use siouxdb_catalogo::Valor;
use siouxdb_transacoes::{Banco, Config};
use siouxdb_vfs::{ConfigSimulacao, Vfs, VfsSimulado};
use std::path::PathBuf;

type B = Banco<<VfsSimulado as Vfs>::Arquivo>;

fn banco() -> (VfsSimulado, B) {
    let vfs = VfsSimulado::novo(ConfigSimulacao::default());
    let b = B::criar(
        &vfs,
        &PathBuf::from("b.sdb"),
        TamanhoPagina::K4,
        None,
        Config::default(),
    )
    .unwrap();
    (vfs, b)
}

fn exec(b: &B, sql: &str, params: &[Valor]) -> Vec<Saida> {
    let mut tx = b.escrever().unwrap();
    let r = executar(&mut tx, sql, params).unwrap_or_else(|e| panic!("{sql}: {e}"));
    tx.confirmar().unwrap();
    r
}

fn q(b: &B, sql: &str, params: &[Valor]) -> Vec<Vec<Valor>> {
    consultar(&b.ler().unwrap(), sql, params)
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .linhas
}

fn erro(b: &B, sql: &str) -> ErroSql {
    let mut tx = b.escrever().unwrap();
    executar(&mut tx, sql, &[]).expect_err(sql)
}

fn plano(b: &B, sql: &str, params: &[Valor]) -> String {
    match &q(b, &format!("EXPLAIN {sql}"), params)[0][0] {
        Valor::Texto(t) => t.clone(),
        _ => unreachable!(),
    }
}

fn v<T: Into<Valor>>(x: T) -> Valor {
    x.into()
}

#[test]
fn ciclo_completo() {
    let (_vfs, b) = banco();
    exec(
        &b,
        "CREATE TABLE clientes (id INTEIRO PRIMARY KEY, nome TEXTO NOT NULL, cidade VARCHAR(40), saldo REAL)",
        &[],
    );
    exec(
        &b,
        "create table if not exists clientes (x int primary key)",
        &[],
    );
    let r = exec(
        &b,
        "INSERT INTO clientes VALUES (1, 'ANA', 'SP', 10), (2, 'BIA', 'RJ', 20.5), (3, 'CAIO', 'SP', NULL);
         INSERT INTO clientes (id, nome) VALUES (?, ?), ($3, 'EVA')",
        &[v(4), v("DUDA"), v(5)],
    );
    assert_eq!((r[0].afetadas, r[1].afetadas), (3, 2));
    assert_eq!(
        q(&b, "SELECT COUNT(*) FROM clientes", &[]),
        vec![vec![v(5)]]
    );
    assert_eq!(
        q(
            &b,
            "SELECT nome FROM clientes WHERE cidade = 'SP' ORDER BY nome DESC",
            &[]
        ),
        vec![vec![v("CAIO")], vec![v("ANA")]]
    );
    // Inteiro numa coluna REAL é aceito; comparação inteiro x real funciona.
    assert_eq!(
        q(&b, "SELECT id FROM clientes WHERE saldo > 10", &[]),
        vec![vec![v(2)]]
    );
    assert_eq!(
        q(
            &b,
            "SELECT id FROM clientes WHERE saldo IS NULL ORDER BY 1",
            &[]
        ),
        vec![vec![v(3)], vec![v(4)], vec![v(5)]]
    );
    assert_eq!(
        q(
            &b,
            "SELECT id, nome || '-' || COALESCE(cidade, '?') AS rotulo FROM clientes WHERE id IN (1, 4) ORDER BY id",
            &[]
        ),
        vec![vec![v(1), v("ANA-SP")], vec![v(4), v("DUDA-?")]]
    );
    assert_eq!(
        q(
            &b,
            "SELECT id FROM clientes WHERE nome LIKE '_A%' ORDER BY id",
            &[]
        ),
        vec![vec![v(3)]]
    );
    assert_eq!(
        q(
            &b,
            "SELECT id FROM clientes WHERE id BETWEEN 2 AND 4 AND NOT id = 3",
            &[]
        ),
        vec![vec![v(2)], vec![v(4)]]
    );
    assert_eq!(
        q(
            &b,
            "SELECT id FROM clientes ORDER BY id LIMIT 2 OFFSET 1",
            &[]
        ),
        vec![vec![v(2)], vec![v(3)]]
    );
    assert_eq!(
        q(
            &b,
            "SELECT 1 + 2 * 3, 7 / 2, 7.0 / 2, -id FROM clientes WHERE id = 1",
            &[]
        ),
        vec![vec![v(7), v(3), v(3.5), v(-1)]]
    );

    // Agrupamento, HAVING, agregados e ordenação pelo apelido.
    assert_eq!(
        q(
            &b,
            "SELECT cidade, COUNT(*) AS n, SUM(saldo), MAX(nome) FROM clientes GROUP BY cidade HAVING COUNT(*) >= 1 ORDER BY n DESC, cidade",
            &[]
        ),
        vec![
            vec![Valor::Nulo, v(2), Valor::Nulo, v("EVA")],
            vec![v("SP"), v(2), v(10.0), v("CAIO")],
            vec![v("RJ"), v(1), v(20.5), v("BIA")],
        ]
    );
    assert_eq!(
        q(
            &b,
            "SELECT COUNT(*), SUM(saldo) FROM clientes WHERE id > 100",
            &[]
        ),
        vec![vec![v(0), Valor::Nulo]]
    );
    assert_eq!(
        q(&b, "SELECT AVG(saldo) FROM clientes", &[]),
        vec![vec![v(15.25)]]
    );

    // UPDATE (inclusive trocando a chave) e DELETE.
    let lsn_antes = b.estatisticas().lsn_escrito;
    assert_eq!(
        exec(
            &b,
            "UPDATE clientes SET saldo = saldo + 1 WHERE cidade = 'SP'",
            &[]
        )[0]
        .afetadas,
        2
    );
    assert_eq!(
        q(&b, "SELECT saldo FROM clientes WHERE id = 1", &[]),
        vec![vec![v(11.0)]]
    );
    exec(&b, "UPDATE clientes SET id = 10 WHERE id = 5", &[]);
    assert_eq!(
        q(&b, "SELECT nome FROM clientes WHERE id = 10", &[]),
        vec![vec![v("EVA")]]
    );
    assert!(q(&b, "SELECT * FROM clientes WHERE id = 5", &[]).is_empty());
    assert_eq!(
        exec(&b, "DELETE FROM clientes WHERE cidade IS NULL", &[])[0].afetadas,
        2
    );
    assert_eq!(
        q(&b, "SELECT COUNT(*) FROM clientes", &[]),
        vec![vec![v(3)]]
    );
    // Como estava antes das mudanças.
    assert_eq!(
        q(
            &b,
            &format!("SELECT COUNT(*) FROM clientes AS OF {lsn_antes}"),
            &[]
        ),
        vec![vec![v(5)]]
    );
    assert_eq!(
        q(
            &b,
            &format!("SELECT saldo FROM clientes AS OF {lsn_antes} WHERE id = 1"),
            &[]
        ),
        vec![vec![v(10.0)]]
    );

    // Conflitos.
    assert!(matches!(
        erro(&b, "INSERT INTO clientes (id, nome) VALUES (1, 'X')"),
        ErroSql::Catalogo(_)
    ));
    assert_eq!(
        exec(
            &b,
            "INSERT OR IGNORE INTO clientes (id, nome) VALUES (1, 'X'), (50, 'NOVO')",
            &[]
        )[0]
        .afetadas,
        1
    );
    exec(
        &b,
        "INSERT INTO clientes (id, nome) VALUES (1, 'TROCADO') ON CONFLICT DO UPDATE",
        &[],
    );
    assert_eq!(
        q(&b, "SELECT nome, saldo FROM clientes WHERE id = 1", &[]),
        vec![vec![v("TROCADO"), Valor::Nulo]]
    );

    // Tabelas do sistema.
    assert_eq!(
        q(&b, "SELECT nome, linhas FROM sys_tabelas", &[]),
        vec![vec![v("clientes"), v(4)]]
    );
    assert_eq!(
        q(
            &b,
            "SELECT COUNT(*) FROM sys_colunas WHERE tabela = 'clientes'",
            &[]
        ),
        vec![vec![v(4)]]
    );
    assert!(somente_leitura("SELECT 1; EXPLAIN SELECT 2").unwrap());
    assert!(!somente_leitura("SELECT 1; DELETE FROM x").unwrap());
}

#[test]
fn erros_claros_e_transacao_tudo_ou_nada() {
    let (_vfs, b) = banco();
    exec(
        &b,
        "CREATE TABLE t (id INT PRIMARY KEY, n TEXT); INSERT INTO t VALUES (100, 'x')",
        &[],
    );
    match erro(&b, "SELEC * FROM t") {
        ErroSql::Sintaxe { posicao, .. } => assert_eq!(posicao, 0),
        e => panic!("{e}"),
    }
    match erro(&b, "SELECT * FROM t WHERE") {
        ErroSql::Sintaxe { posicao, .. } => assert_eq!(posicao, 21),
        e => panic!("{e}"),
    }
    for (sql, trecho) in [
        ("SELECT nada FROM t", "coluna \"nada\""),
        ("SELECT * FROM t WHERE n > 1", "comparar"),
        ("INSERT INTO t VALUES ('x', 'y')", "é INTEIRO"),
        ("SELECT * FROM nao_existe", "não existe"),
        ("CREATE TABLE sem_chave (a INT)", "PRIMARY KEY"),
        ("SELECT * FROM t WHERE id = ?", "parâmetro"),
        ("INSERT INTO sys_tabelas VALUES (1)", "sistema"),
        ("SELECT 1/0", "divisão por zero"),
        ("SELECT * FROM t GROUP BY n", "GROUP BY"),
        ("SELECT * FROM t WHERE COUNT(*) > 1", "SELECT, HAVING"),
    ] {
        let e = erro(&b, sql).to_string();
        assert!(e.contains(trecho), "{sql}: \"{e}\" não tem \"{trecho}\"");
    }
    // Um erro no meio do lote: quem chama não confirma, e nada do lote fica.
    let mut tx = b.escrever().unwrap();
    assert!(
        executar(
            &mut tx,
            "INSERT INTO t VALUES (1, 'a'); INSERT INTO t VALUES (1, 'b')",
            &[]
        )
        .is_err()
    );
    tx.desfazer();
    assert_eq!(q(&b, "SELECT COUNT(*) FROM t", &[]), vec![vec![v(1)]]);
    // Nomes entre aspas podem ser palavras reservadas; comentários são ignorados.
    exec(
        &b,
        "CREATE TABLE \"order\" (\"select\" INT PRIMARY KEY) -- comentário\n; /* outro */",
        &[],
    );
    exec(&b, "INSERT INTO \"order\" VALUES (1)", &[]);
    assert_eq!(
        q(&b, "SELECT \"select\" FROM \"order\"", &[]),
        vec![vec![v(1)]]
    );
    assert_eq!(q(&b, "SELECT 'it''s'", &[]), vec![vec![v("it's")]]);
}

#[test]
fn planejador_usa_chave_e_indices() {
    let (_vfs, b) = banco();
    exec(
        &b,
        "CREATE TABLE p (uf TEXT, id INT, nome TEXT, idade INT, PRIMARY KEY (uf, id))",
        &[],
    );
    exec(&b, "CREATE INDEX p_idade ON p (idade)", &[]);
    exec(&b, "CREATE UNIQUE INDEX p_nome ON p (nome)", &[]);
    exec(
        &b,
        "INSERT INTO p VALUES ('SP', 1, 'A', 30), ('SP', 2, 'B', 40), ('RJ', 1, 'C', 30)",
        &[],
    );
    assert_eq!(
        plano(&b, "SELECT * FROM p WHERE uf = 'SP' AND id = 2", &[]),
        "busca pela chave primária"
    );
    assert_eq!(
        plano(&b, "SELECT * FROM p WHERE uf = 'SP'", &[]),
        "faixa da chave primária"
    );
    assert_eq!(
        plano(&b, "SELECT * FROM p WHERE idade = 30", &[]),
        "índice p_idade"
    );
    assert_eq!(
        plano(&b, "SELECT * FROM p WHERE nome = ?", &[v("B")]),
        "índice p_nome"
    );
    assert_eq!(
        plano(&b, "SELECT * FROM p WHERE idade + 1 = 31", &[]),
        "varredura completa"
    );
    assert_eq!(
        plano(&b, "SELECT * FROM p AS OF 1 WHERE idade = 30", &[]),
        "varredura completa"
    );
    assert_eq!(
        q(&b, "SELECT id FROM p WHERE idade = 30 AND uf = 'RJ'", &[]),
        vec![vec![v(1)]]
    );
    let e = erro(&b, "INSERT INTO p VALUES ('MG', 9, 'A', 1)").to_string();
    assert!(e.contains("p_nome"), "{e}");
    exec(&b, "UPDATE p SET idade = 41 WHERE nome = 'B'", &[]);
    assert_eq!(
        q(&b, "SELECT nome FROM p WHERE idade = 41", &[]),
        vec![vec![v("B")]]
    );
    assert!(q(&b, "SELECT nome FROM p WHERE idade = 40", &[]).is_empty());
    assert_eq!(
        q(
            &b,
            "SELECT nome, colunas, unico FROM sys_indices ORDER BY nome",
            &[]
        )
        .len(),
        2
    );
    exec(
        &b,
        "DROP INDEX p_idade; DROP INDEX IF EXISTS p_idade; DROP TABLE IF EXISTS nada",
        &[],
    );
    assert_eq!(
        plano(&b, "SELECT * FROM p WHERE idade = 30", &[]),
        "varredura completa"
    );
}

struct Sorteio(u64);
impl Sorteio {
    fn ate(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) % n.max(1)
    }
}

type Linha = (i64, i64, Option<String>, Option<f64>);
type Teste = Box<dyn Fn(&Linha) -> bool>;

/// Consultas aleatórias pelo planejador x o mesmo filtro calculado em Rust.
#[test]
fn diferencial_planejador_contra_filtro_em_rust() {
    let (_vfs, b) = banco();
    exec(
        &b,
        "CREATE TABLE d (a INT, b INT, c TEXT, x REAL, PRIMARY KEY (a, b))",
        &[],
    );
    exec(
        &b,
        "CREATE INDEX d_c ON d (c); CREATE INDEX d_xa ON d (x, a)",
        &[],
    );
    let mut s = Sorteio(99);
    let mut dados: Vec<(i64, i64, Option<String>, Option<f64>)> = Vec::new();
    let mut tx = b.escrever().unwrap();
    for a in -5..15i64 {
        for bb in 0..12i64 {
            if s.ate(3) == 0 {
                continue;
            }
            let c = (s.ate(6) != 0)
                .then(|| ["", "a", "b", "ab", "b\0", "z"][s.ate(6) as usize].to_string());
            let x = (s.ate(5) != 0).then(|| s.ate(7) as f64 - 3.0);
            executar(
                &mut tx,
                "INSERT INTO d VALUES (?, ?, ?, ?)",
                &[
                    v(a),
                    v(bb),
                    c.clone().map_or(Valor::Nulo, Valor::Texto),
                    x.map_or(Valor::Nulo, Valor::Real),
                ],
            )
            .unwrap();
            dados.push((a, bb, c, x));
        }
    }
    tx.confirmar().unwrap();
    let ops = ["=", "<", "<=", ">", ">="];
    let mut planos = std::collections::HashSet::new();
    for _ in 0..1500 {
        // 1 a 3 condições sobre colunas aleatórias.
        let mut conds = Vec::new();
        let mut testes: Vec<Teste> = Vec::new();
        for _ in 0..1 + s.ate(3) {
            let op = ops[s.ate(5) as usize];
            let cmp = move |o: std::cmp::Ordering| match op {
                "=" => o.is_eq(),
                "<" => o.is_lt(),
                "<=" => o.is_le(),
                ">" => o.is_gt(),
                _ => o.is_ge(),
            };
            match s.ate(4) {
                0 => {
                    let k = s.ate(24) as i64 - 7;
                    conds.push(format!("a {op} {k}"));
                    testes.push(Box::new(move |r| cmp(r.0.cmp(&k))));
                }
                1 => {
                    let k = s.ate(14) as i64 - 1;
                    conds.push(format!("b {op} {k}"));
                    testes.push(Box::new(move |r| cmp(r.1.cmp(&k))));
                }
                2 => {
                    let k = ["", "a", "ab", "b", "c"][s.ate(5) as usize].to_string();
                    conds.push(format!("c {op} '{k}'"));
                    testes.push(Box::new(move |r| {
                        r.2.as_ref()
                            .is_some_and(|c| cmp(c.as_bytes().cmp(k.as_bytes())))
                    }));
                }
                _ => {
                    let k = s.ate(9) as i64 - 4;
                    // às vezes como inteiro, às vezes como real
                    let lit = if s.ate(2) == 0 {
                        format!("{k}")
                    } else {
                        format!("{k}.0")
                    };
                    conds.push(format!("x {op} {lit}"));
                    testes.push(Box::new(move |r| {
                        r.3.is_some_and(|x| cmp(x.total_cmp(&(k as f64))))
                    }));
                }
            }
        }
        let onde = conds.join(" AND ");
        let sql = format!("SELECT a, b FROM d WHERE {onde} ORDER BY a, b");
        let obtido: Vec<(i64, i64)> = q(&b, &sql, &[])
            .into_iter()
            .map(|l| match (&l[0], &l[1]) {
                (Valor::Inteiro(a), Valor::Inteiro(b)) => (*a, *b),
                _ => unreachable!(),
            })
            .collect();
        let mut esperado: Vec<(i64, i64)> = dados
            .iter()
            .filter(|r| testes.iter().all(|t| t(r)))
            .map(|r| (r.0, r.1))
            .collect();
        esperado.sort();
        assert_eq!(obtido, esperado, "{sql}");
        planos.insert(plano(&b, &sql, &[]));
    }
    // O teste precisa ter exercitado todos os caminhos.
    for p in [
        "busca pela chave primária",
        "faixa da chave primária",
        "índice d_c",
        "índice d_xa",
        "varredura completa",
    ] {
        assert!(planos.contains(p), "plano não exercitado: {p} ({planos:?})");
    }
}
