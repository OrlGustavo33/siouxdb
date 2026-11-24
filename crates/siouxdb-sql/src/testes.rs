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

/// (a, b, c, x, m em quartos: m = quartos / 4)
type Linha = (i64, i64, Option<String>, Option<f64>, Option<i64>);
type Teste = Box<dyn Fn(&Linha) -> bool>;

/// Consultas aleatórias pelo planejador x o mesmo filtro calculado em Rust.
#[test]
fn diferencial_planejador_contra_filtro_em_rust() {
    let (_vfs, b) = banco();
    exec(
        &b,
        "CREATE TABLE d (a INT, b INT, c TEXT, x REAL, m DECIMAL, PRIMARY KEY (a, b))",
        &[],
    );
    exec(
        &b,
        "CREATE INDEX d_c ON d (c); CREATE INDEX d_xa ON d (x, a); CREATE INDEX d_m ON d (m)",
        &[],
    );
    let mut s = Sorteio(99);
    let mut dados: Vec<Linha> = Vec::new();
    let mut tx = b.escrever().unwrap();
    for a in -5..15i64 {
        for bb in 0..12i64 {
            if s.ate(3) == 0 {
                continue;
            }
            let c = (s.ate(6) != 0)
                .then(|| ["", "a", "b", "ab", "b\0", "z"][s.ate(6) as usize].to_string());
            let x = (s.ate(5) != 0).then(|| s.ate(7) as f64 - 3.0);
            let m = (s.ate(5) != 0).then(|| s.ate(17) as i64 - 8);
            executar(
                &mut tx,
                "INSERT INTO d VALUES (?, ?, ?, ?, ?)",
                &[
                    v(a),
                    v(bb),
                    c.clone().map_or(Valor::Nulo, Valor::Texto),
                    x.map_or(Valor::Nulo, Valor::Real),
                    // decimal mandado como texto: "-1.75", "0.25"...
                    m.map_or(Valor::Nulo, |q| Valor::Texto(format!("{}", q as f64 / 4.0))),
                ],
            )
            .unwrap();
            dados.push((a, bb, c, x, m));
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
            match s.ate(5) {
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
                3 => {
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
                4 => {
                    // decimal comparado com literal inteiro ou com casas ("0.75")
                    let q = s.ate(21) as i64 - 10;
                    let lit = if q % 4 == 0 && s.ate(2) == 0 {
                        format!("{}", q / 4)
                    } else {
                        format!("{:?}", q as f64 / 4.0)
                    };
                    conds.push(format!("m {op} {lit}"));
                    testes.push(Box::new(move |r| r.4.is_some_and(|m| cmp(m.cmp(&q)))));
                }
                _ => unreachable!(),
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
        "índice d_m",
        "varredura completa",
    ] {
        assert!(planos.contains(p), "plano não exercitado: {p} ({planos:?})");
    }
}

#[test]
fn decimal_data_padrao_auto_e_alter() {
    let (_vfs, b) = banco();
    exec(
        &b,
        "CREATE TABLE vendas (
            id SERIAL PRIMARY KEY,
            total DECIMAL NOT NULL,
            quando DATAHORA DEFAULT AGORA(),
            status TEXTO NOT NULL DEFAULT 'aberta',
            obs TEXTO)",
        &[],
    );
    // Numeração automática: omitida, NULL e explícita.
    let r = exec(&b, "INSERT INTO vendas (total) VALUES (0.1), (0.2)", &[]);
    assert_eq!((r[0].afetadas, r[0].ultimo_id), (2, Some(2)));
    let r = exec(
        &b,
        "INSERT INTO vendas (id, total, quando) VALUES (100, '19.99', '2026-11-23 10:00:00')",
        &[],
    );
    assert_eq!(r[0].ultimo_id, Some(100));
    let r = exec(
        &b,
        "INSERT INTO vendas (id, total, status) VALUES (NULL, 5, 'paga')",
        &[],
    );
    assert_eq!(r[0].ultimo_id, Some(101));
    assert_eq!(
        q(&b, "SELECT id, status FROM vendas ORDER BY id", &[])
            .iter()
            .map(|l| l[0].clone())
            .collect::<Vec<_>>(),
        vec![v(1), v(2), v(100), v(101)]
    );
    assert_eq!(
        q(&b, "SELECT status FROM vendas WHERE id = 1", &[]),
        vec![vec![v("aberta")]]
    );
    // O padrão AGORA() preencheu a data.
    assert_eq!(
        q(
            &b,
            "SELECT COUNT(*) FROM vendas WHERE quando > '2026-01-01'",
            &[]
        ),
        vec![vec![v(4)]]
    );

    // Decimal é exato onde o ponto flutuante erra.
    let dec = |s: &str| Valor::Decimal(siouxdb_catalogo::Decimal::de_texto(s).unwrap());
    assert_eq!(
        q(&b, "SELECT SUM(total) FROM vendas WHERE id <= 2", &[]),
        vec![vec![dec("0.3")]]
    );
    // A conta em REAL (0.3 - 0.1 = 0.19999999999999998) é arredondada ao comparar com decimal.
    assert_eq!(
        q(&b, "SELECT id FROM vendas WHERE total = 0.3 - 0.1", &[]),
        vec![vec![v(2)]]
    );
    assert_eq!(
        q(&b, "SELECT id FROM vendas WHERE total = 0.2", &[]),
        vec![vec![v(2)]]
    );
    assert_eq!(
        q(
            &b,
            "SELECT total * 3, total / 3, total + 1, -total FROM vendas WHERE id = 100",
            &[]
        ),
        vec![vec![
            dec("59.97"),
            dec("6.6633333333"),
            dec("20.99"),
            dec("-19.99")
        ]]
    );
    assert_eq!(
        q(
            &b,
            "SELECT AVG(total), MAX(total), MIN(total) FROM vendas",
            &[]
        ),
        vec![vec![dec("6.3225"), dec("19.99"), dec("0.1")]]
    );
    assert_eq!(
        q(
            &b,
            "SELECT id FROM vendas WHERE total > 1 ORDER BY total DESC",
            &[]
        ),
        vec![vec![v(100)], vec![v(101)]]
    );
    exec(&b, "CREATE INDEX vendas_total ON vendas (total)", &[]);
    assert_eq!(
        plano(&b, "SELECT * FROM vendas WHERE total >= 5", &[]),
        "índice vendas_total"
    );
    assert_eq!(
        q(
            &b,
            "SELECT id FROM vendas WHERE total >= 5 AND total < 19.99",
            &[]
        ),
        vec![vec![v(101)]]
    );
    exec(
        &b,
        "UPDATE vendas SET total = total * 1.1 WHERE id = 101",
        &[],
    );
    assert_eq!(
        q(&b, "SELECT total FROM vendas WHERE id = 101", &[]),
        vec![vec![dec("5.5")]]
    );

    // Datas: comparação, faixa, ordenação e texto inválido.
    exec(
        &b,
        "UPDATE vendas SET quando = '2020-05-01' WHERE id IN (2, 101)",
        &[],
    );
    exec(
        &b,
        "UPDATE vendas SET quando = '2026-11-22T23:30:00-03:00' WHERE id = 1",
        &[],
    );
    assert_eq!(
        q(
            &b,
            "SELECT id FROM vendas WHERE quando >= '2026-11-23' AND quando < '2026-11-24' ORDER BY quando",
            &[]
        ),
        vec![vec![v(1)], vec![v(100)]]
    );
    assert_eq!(
        q(&b, "SELECT quando FROM vendas WHERE id = 100", &[]),
        vec![vec![Valor::DataHora(1_795_428_000_000_000)]]
    );
    assert!(
        erro(
            &b,
            "INSERT INTO vendas (total, quando) VALUES (1, '23/11/2026')"
        )
        .to_string()
        .contains("não é uma data")
    );
    assert!(
        erro(&b, "INSERT INTO vendas (total) VALUES ('abc')")
            .to_string()
            .contains("decimal")
    );
    assert!(
        erro(&b, "INSERT INTO vendas (obs) VALUES ('sem total')")
            .to_string()
            .contains("não aceita nulo")
    );

    // ALTER TABLE: coluna nova com padrão aparece nas linhas antigas, sem regravar nada.
    let lsn_antes = b.estatisticas().lsn_escrito;
    exec(
        &b,
        "ALTER TABLE vendas ADD COLUMN desconto DECIMAL NOT NULL DEFAULT 0; ALTER TABLE vendas ADD cupom TEXTO",
        &[],
    );
    assert_eq!(
        q(&b, "SELECT desconto, cupom FROM vendas WHERE id = 1", &[]),
        vec![vec![dec("0"), Valor::Nulo]]
    );
    exec(
        &b,
        "UPDATE vendas SET desconto = 1.5, cupom = 'X' WHERE id = 2",
        &[],
    );
    assert_eq!(
        q(&b, "SELECT SUM(desconto), COUNT(cupom) FROM vendas", &[]),
        vec![vec![dec("1.5"), v(1)]]
    );
    exec(
        &b,
        "INSERT INTO vendas (total, desconto) VALUES (7, 0.5)",
        &[],
    );
    assert_eq!(
        q(
            &b,
            &format!("SELECT desconto FROM vendas AS OF {lsn_antes} WHERE id = 2"),
            &[]
        ),
        vec![vec![dec("0")]]
    );
    assert!(
        erro(&b, "ALTER TABLE vendas ADD COLUMN x TEXTO NOT NULL")
            .to_string()
            .contains("valor padrão")
    );
    assert!(
        erro(&b, "ALTER TABLE vendas ADD COLUMN total INT")
            .to_string()
            .contains("já existe")
    );
    assert!(
        erro(&b, "ALTER TABLE vendas ADD COLUMN d DATAHORA DEFAULT 5")
            .to_string()
            .contains("DATAHORA")
    );
    // Renomear coluna e tabela: índice e dados continuam.
    exec(
        &b,
        "ALTER TABLE vendas RENAME COLUMN total TO valor; ALTER TABLE vendas RENAME TO pedidos",
        &[],
    );
    assert_eq!(
        plano(&b, "SELECT * FROM pedidos WHERE valor = 7", &[]),
        "índice vendas_total"
    );
    assert_eq!(
        q(&b, "SELECT id FROM pedidos WHERE valor = 7", &[]),
        vec![vec![v(102)]]
    );
    assert!(
        erro(&b, "SELECT * FROM vendas")
            .to_string()
            .contains("não existe")
    );
    assert_eq!(
        q(
            &b,
            "SELECT tipo FROM sys_colunas WHERE tabela = 'pedidos' AND nome IN ('valor', 'quando') ORDER BY nome",
            &[]
        ),
        vec![vec![v("DATAHORA")], vec![v("DECIMAL")]]
    );
}

fn fator_simulacao() -> u64 {
    std::env::var("SIOUX_SIM_FATOR")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
}

/// Texto SQL aleatório (pedaços válidos misturados com lixo) nunca derruba o analisador.
#[test]
fn sql_aleatorio_nunca_causa_panico() {
    let pedacos = [
        "SELECT",
        "FROM",
        "WHERE",
        "INSERT",
        "INTO",
        "VALUES",
        "UPDATE",
        "SET",
        "DELETE",
        "CREATE",
        "TABLE",
        "INDEX",
        "ALTER",
        "ADD",
        "COLUMN",
        "DROP",
        "PRIMARY",
        "KEY",
        "NOT",
        "NULL",
        "AND",
        "OR",
        "IN",
        "BETWEEN",
        "LIKE",
        "IS",
        "GROUP",
        "BY",
        "ORDER",
        "HAVING",
        "LIMIT",
        "OFFSET",
        "AS",
        "OF",
        "DEFAULT",
        "UNIQUE",
        "ON",
        "CONFLICT",
        "DO",
        "t",
        "id",
        "nome",
        "COUNT",
        "SUM",
        "(",
        ")",
        ",",
        ";",
        "*",
        "=",
        "<",
        ">",
        "<=",
        ">=",
        "<>",
        "||",
        "+",
        "-",
        "/",
        "%",
        ".",
        "?",
        "$1",
        "$0",
        "$",
        "1",
        "0.5",
        "1e999",
        "99999999999999999999",
        "'texto'",
        "'aberto",
        "\"x\"",
        "\"",
        "--",
        "/*",
        "*/",
        "\n",
        "ã",
        "\u{0}",
        "INTEIRO",
        "DECIMAL",
        "DATAHORA",
        "SERIAL",
        "AGORA",
        "EXPLAIN",
    ];
    let mut s = Sorteio(2026);
    let (_vfs, b) = banco();
    exec(&b, "CREATE TABLE t (id INT PRIMARY KEY, nome TEXT)", &[]);
    for _ in 0..20_000 * fator_simulacao() {
        let n = 1 + s.ate(14);
        let sql: String = (0..n)
            .map(|_| pedacos[s.ate(pedacos.len() as u64) as usize])
            .collect::<Vec<_>>()
            .join(" ");
        let _ = somente_leitura(&sql);
        // Os que passam no analisador também rodam (numa transação desfeita).
        let mut tx = b.escrever().unwrap();
        let _ = executar(&mut tx, &sql, &[v(1)]);
        tx.desfazer();
    }
}

/// Estraga páginas do arquivo **mantendo a verificação válida** (como faria um defeito do
/// próprio programa) e usa o banco de todas as formas: o resultado pode ser erro, nunca
/// pânico nem laço sem fim.
#[test]
fn pagina_estragada_com_verificacao_valida_nunca_causa_panico() {
    use siouxdb_armazenamento::crc32c;
    use siouxdb_vfs::{Arquivo, ModoAbertura};
    let caminho = PathBuf::from("b.sdb");
    let preparar = |vfs: &VfsSimulado| {
        let b = B::criar(vfs, &caminho, TamanhoPagina::K4, None, Config::default()).unwrap();
        exec(
            &b,
            "CREATE TABLE a (id SERIAL PRIMARY KEY, nome TEXT, valor DECIMAL, quando DATAHORA DEFAULT AGORA());
             CREATE INDEX a_nome ON a (nome); CREATE TABLE z (k TEXT PRIMARY KEY, doc TEXT)",
            &[],
        );
        let mut tx = b.escrever().unwrap();
        for i in 0..400 {
            executar(
                &mut tx,
                "INSERT INTO a (nome, valor) VALUES (?, ?)",
                &[v(format!("nome {}", i % 37)), v(i)],
            )
            .unwrap();
        }
        executar(
            &mut tx,
            "INSERT INTO z VALUES ('grande', ?), ('pequeno', 'x')",
            &[v("g".repeat(20_000))],
        )
        .unwrap();
        executar(
            &mut tx,
            "UPDATE a SET valor = valor + 1 WHERE id < 50; DELETE FROM a WHERE id > 390",
            &[],
        )
        .unwrap();
        tx.confirmar().unwrap();
        b.checkpoint().unwrap();
    };
    let usar = |vfs: &VfsSimulado| {
        let Ok(b) = B::abrir(vfs, &caminho, None, Config::default()) else {
            return;
        };
        for sql in [
            "SELECT * FROM sys_tabelas",
            "SELECT * FROM sys_colunas",
            "SELECT * FROM sys_indices",
            "SELECT COUNT(*), SUM(valor), MAX(nome) FROM a",
            "SELECT * FROM a WHERE nome = 'nome 5'",
            "SELECT * FROM a WHERE id BETWEEN 10 AND 20",
            "SELECT * FROM a AS OF 1",
            "SELECT k, doc FROM z",
        ] {
            if let Ok(l) = b.ler() {
                let _ = consultar(&l, sql, &[]);
            }
        }
        if let Ok(l) = b.ler() {
            if let Ok(tabelas) = siouxdb_catalogo::catalogo::tabelas(&l) {
                for t in tabelas {
                    let _ = t.verificar_indice(&l);
                    let _ = t.historico(&l, &[v(5)]);
                }
            }
        }
        for sql in [
            "INSERT INTO a (nome, valor) VALUES ('novo', 1)",
            "UPDATE a SET nome = 'x' WHERE id = 7",
            "DELETE FROM a WHERE id = 8",
            "INSERT INTO z VALUES ('outro', 'y')",
            "CREATE TABLE nova (id INT PRIMARY KEY)",
            "DROP TABLE z",
            "ALTER TABLE a ADD COLUMN extra INT DEFAULT 0",
        ] {
            if let Ok(mut tx) = b.escrever() {
                if executar(&mut tx, sql, &[]).is_ok() {
                    let _ = tx.confirmar();
                }
            }
        }
        if let Ok(mut tx) = b.escrever() {
            if let Ok(Some(t)) = siouxdb_catalogo::catalogo::tabela(&tx, "a") {
                if t.compactar(&mut tx, u64::MAX).is_ok() {
                    let _ = tx.confirmar();
                }
            }
        }
        let _ = b.checkpoint();
    };
    let mut s = Sorteio(77);
    for rodada in 0..400 * fator_simulacao() {
        let vfs = VfsSimulado::novo(ConfigSimulacao::default());
        preparar(&vfs);
        let arq = vfs.abrir(&caminho, ModoAbertura::LeituraGravacao).unwrap();
        let paginas = arq.tamanho().unwrap() / 4096;
        // Estraga de 1 a 3 páginas: bytes aleatórios, zeros, ou um campo trocado por um extremo.
        for _ in 0..1 + s.ate(3) {
            let n = 2 + s.ate(paginas - 2);
            let mut pg = vec![0u8; 4096];
            arq.ler_exato_em(n * 4096, &mut pg).unwrap();
            match s.ate(4) {
                0 => {
                    for _ in 0..1 + s.ate(8) {
                        let i = 24 + s.ate(4072) as usize;
                        pg[i] = s.ate(256) as u8;
                    }
                }
                1 => {
                    let i = 24 + s.ate(200) as usize;
                    let extremos = [
                        0u64,
                        1,
                        2,
                        u64::MAX,
                        u32::MAX as u64,
                        n,
                        paginas,
                        paginas + 1,
                        0xFFFF,
                    ];
                    let v = extremos[s.ate(9) as usize].to_le_bytes();
                    let tam = [1usize, 2, 4, 8][s.ate(4) as usize];
                    pg[i..i + tam].copy_from_slice(&v[..tam]);
                }
                2 => pg[24..24 + 64].fill(0xFF),
                _ => pg[4] = s.ate(9) as u8,
            }
            let crc = crc32c(&pg[4..]);
            pg[..4].copy_from_slice(&crc.to_le_bytes());
            arq.gravar_em(n * 4096, &pg).unwrap();
        }
        arq.sincronizar().unwrap();
        drop(arq);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| usar(&vfs)));
        assert!(r.is_ok(), "pânico na rodada {rodada}");
    }
}

/// WAL cortado ou com bytes trocados: abrir dá certo (recupera o que estava inteiro) ou dá
/// erro; o que abre continua utilizável.
#[test]
fn wal_estragado_nunca_causa_panico() {
    use siouxdb_vfs::{Arquivo, ModoAbertura};
    let caminho = PathBuf::from("b.sdb");
    let mut s = Sorteio(5);
    for rodada in 0..300 * fator_simulacao() {
        let vfs = VfsSimulado::novo(ConfigSimulacao::default());
        let b = B::criar(
            &vfs,
            &caminho,
            TamanhoPagina::K4,
            None,
            Config {
                checkpoint_apos_paginas: 0,
                ..Config::default()
            },
        )
        .unwrap();
        exec(&b, "CREATE TABLE t (id INT PRIMARY KEY, n TEXT)", &[]);
        for i in 0..6 {
            exec(
                &b,
                "INSERT INTO t VALUES (?, ?)",
                &[v(i), v(format!("linha {i}"))],
            );
        }
        drop(b);
        let wal = vfs
            .abrir(&PathBuf::from("b.sdb-wal"), ModoAbertura::LeituraGravacao)
            .unwrap();
        let tam = wal.tamanho().unwrap();
        match s.ate(3) {
            0 => wal.truncar(s.ate(tam)).unwrap(),
            1 => {
                for _ in 0..1 + s.ate(4) {
                    wal.gravar_em(s.ate(tam), &[s.ate(256) as u8]).unwrap();
                }
            }
            _ => wal
                .gravar_em(s.ate(tam), &vec![0u8; 1 + s.ate(5000) as usize])
                .unwrap(),
        }
        wal.sincronizar().unwrap();
        drop(wal);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Ok(b) = B::abrir(&vfs, &caminho, None, Config::default()) {
                // O que foi recuperado é um prefixo inteiro das confirmações.
                let n = match consultar(&b.ler().unwrap(), "SELECT COUNT(*) FROM sys_tabelas", &[])
                {
                    Ok(r) => r.linhas[0][0].clone(),
                    Err(e) => panic!("catálogo ilegível depois de recuperar: {e}"),
                };
                if n == v(1) {
                    let linhas = q(&b, "SELECT id FROM t ORDER BY id", &[]);
                    let esperado: Vec<Vec<Valor>> =
                        (0..linhas.len() as i64).map(|i| vec![v(i)]).collect();
                    assert_eq!(linhas, esperado, "recuperou confirmações fora de ordem");
                    exec(&b, "INSERT INTO t VALUES (100, 'depois')", &[]);
                }
            }
        }));
        assert!(r.is_ok(), "pânico na rodada {rodada}");
    }
}

#[test]
fn join_basico_left_alias_plano_e_erros() {
    let (_vfs, b) = banco();
    exec(
        &b,
        "CREATE TABLE clientes (id INT PRIMARY KEY, nome TEXT, cidade TEXT);
         CREATE TABLE pedidos (id SERIAL PRIMARY KEY, cliente_id INT, total DECIMAL);
         CREATE INDEX pedidos_cliente ON pedidos (cliente_id);
         INSERT INTO clientes VALUES (1, 'ANA', 'SP'), (2, 'BIA', 'RJ'), (3, 'CAIO', 'SP');
         INSERT INTO pedidos (cliente_id, total) VALUES (1, 10), (1, 5.5), (2, 7), (9, 1)",
        &[],
    );
    let dec = |s: &str| Valor::Decimal(siouxdb_catalogo::Decimal::de_texto(s).unwrap());
    // INNER: pedidos sem cliente (9) e clientes sem pedido (3) ficam de fora.
    assert_eq!(
        q(
            &b,
            "SELECT c.nome, p.total FROM pedidos p JOIN clientes c ON c.id = p.cliente_id ORDER BY p.id",
            &[]
        ),
        vec![
            vec![v("ANA"), dec("10")],
            vec![v("ANA"), dec("5.5")],
            vec![v("BIA"), dec("7")]
        ]
    );
    // LEFT: cliente sem pedido aparece com NULL.
    assert_eq!(
        q(
            &b,
            "SELECT c.nome, p.total FROM clientes c LEFT JOIN pedidos p ON p.cliente_id = c.id ORDER BY c.id, p.id",
            &[]
        ),
        vec![
            vec![v("ANA"), dec("10")],
            vec![v("ANA"), dec("5.5")],
            vec![v("BIA"), dec("7")],
            vec![v("CAIO"), Valor::Nulo]
        ]
    );
    // WHERE depois do LEFT JOIN, agregação e colunas sem prefixo quando não são ambíguas.
    assert_eq!(
        q(
            &b,
            "SELECT nome, COUNT(total) AS n, SUM(total) FROM clientes c LEFT JOIN pedidos ON cliente_id = c.id GROUP BY nome HAVING COUNT(*) >= 1 ORDER BY n DESC, nome",
            &[]
        ),
        vec![
            vec![v("ANA"), v(2), dec("15.5")],
            vec![v("BIA"), v(1), dec("7")],
            vec![v("CAIO"), v(0), Valor::Nulo]
        ]
    );
    assert_eq!(
        q(
            &b,
            "SELECT c.id FROM clientes c LEFT JOIN pedidos p ON p.cliente_id = c.id WHERE p.id IS NULL",
            &[]
        ),
        vec![vec![v(3)]]
    );
    // Vários JOINs, ON com mais de uma condição, e a tabela duas vezes com apelidos.
    assert_eq!(
        q(
            &b,
            "SELECT a.nome, b.nome FROM clientes a JOIN clientes b ON a.cidade = b.cidade AND a.id < b.id",
            &[]
        ),
        vec![vec![v("ANA"), v("CAIO")]]
    );
    assert_eq!(
        q(
            &b,
            "SELECT c.nome, p.total FROM clientes c JOIN pedidos p ON p.cliente_id = c.id JOIN clientes d ON d.id = c.id WHERE d.cidade = 'SP' AND p.total > 6",
            &[]
        ),
        vec![vec![v("ANA"), dec("10")]]
    );
    // SELECT * junta as colunas das duas.
    let r = consultar(
        &b.ler().unwrap(),
        "SELECT * FROM clientes c JOIN pedidos p ON p.cliente_id = c.id WHERE p.id = 3",
        &[],
    )
    .unwrap();
    assert_eq!(r.colunas.len(), 6);
    assert_eq!(r.linhas.len(), 1);
    // AS OF vale para todas as tabelas.
    exec(
        &b,
        "INSERT INTO pedidos (cliente_id, total) VALUES (3, 2)",
        &[],
    );
    let contar = |extra: &str| {
        q(
            &b,
            &format!(
                "SELECT COUNT(*) FROM clientes c JOIN pedidos p ON p.cliente_id = c.id{extra}"
            ),
            &[],
        )
    };
    assert_eq!(
        contar(" AS OF 1"),
        vec![vec![v(3)]],
        "exec roda tudo na confirmação 1"
    );
    assert_eq!(contar(""), vec![vec![v(4)]]);
    // Planos: busca pela chave na esquerda e pelo índice na direita.
    assert_eq!(
        plano(
            &b,
            "SELECT * FROM clientes c JOIN pedidos p ON p.cliente_id = c.id WHERE c.id = 1",
            &[]
        ),
        "c: busca pela chave primária; p: índice pedidos_cliente"
    );
    assert_eq!(
        plano(
            &b,
            "SELECT * FROM pedidos p JOIN clientes c ON c.id = p.cliente_id",
            &[]
        ),
        "p: varredura completa; c: busca pela chave primária"
    );
    assert_eq!(
        plano(
            &b,
            "SELECT * FROM pedidos p JOIN clientes c ON c.nome = 'ANA'",
            &[]
        ),
        "p: varredura completa; c: varredura completa"
    );
    // Erros.
    for (sql, trecho) in [
        (
            "SELECT id FROM clientes c JOIN pedidos p ON p.cliente_id = c.id",
            "ambígua",
        ),
        (
            "SELECT * FROM clientes JOIN clientes ON 1 = 1",
            "duas vezes",
        ),
        (
            "SELECT * FROM clientes c JOIN pedidos p ON x.y = 1",
            "não existe",
        ),
        (
            "SELECT * FROM clientes c JOIN sys_tabelas s ON 1 = 1",
            "sys_",
        ),
        ("SELECT * FROM clientes LEFT pedidos", "JOIN"),
    ] {
        let e = match consultar(&b.ler().unwrap(), sql, &[]) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("{sql} deveria falhar"),
        };
        assert!(e.contains(trecho), "{sql}: \"{e}\"");
    }
}

/// JOIN aleatório: planejador (chave, índice, varredura) x laço aninhado feito em Rust.
#[test]
fn diferencial_join_contra_laco_em_rust() {
    let (_vfs, b) = banco();
    exec(
        &b,
        "CREATE TABLE a (id INT PRIMARY KEY, k INT, t TEXT); CREATE TABLE b (id INT PRIMARY KEY, k INT, t TEXT);
         CREATE INDEX b_k ON b (k)",
        &[],
    );
    let mut s = Sorteio(31);
    let mut gerar = |tabela: &str, n: i64| -> Vec<(i64, Option<i64>, Option<String>)> {
        let mut v = Vec::new();
        let mut tx = b.escrever().unwrap();
        for id in 0..n {
            let k = (s.ate(5) != 0).then(|| s.ate(8) as i64);
            let t = (s.ate(4) != 0).then(|| ["x", "y", "z"][s.ate(3) as usize].to_string());
            executar(
                &mut tx,
                &format!("INSERT INTO {tabela} VALUES (?, ?, ?)"),
                &[
                    v_(id),
                    k.map_or(Valor::Nulo, Valor::Inteiro),
                    t.clone().map_or(Valor::Nulo, Valor::Texto),
                ],
            )
            .unwrap();
            v.push((id, k, t));
        }
        tx.confirmar().unwrap();
        v
    };
    fn v_(i: i64) -> Valor {
        Valor::Inteiro(i)
    }
    let (ta, tb) = (gerar("a", 40), gerar("b", 60));
    let mut planos = std::collections::HashSet::new();
    for (juncao, esquerda) in [("JOIN", false), ("LEFT JOIN", true)] {
        for on in [
            "a.k = b.k",
            "b.k = a.k AND a.t = b.t",
            "a.id = b.id",
            "b.id = a.k",
            "a.k < b.k",
            "a.t = b.t",
        ] {
            for filtro in [
                "",
                " WHERE b.t = 'x'",
                " WHERE a.id < 20 AND a.k = 3",
                " WHERE b.k IS NULL",
                " WHERE a.id = 7",
            ] {
                let sql =
                    format!("SELECT a.id, b.id FROM a {juncao} b ON {on}{filtro} ORDER BY 1, 2");
                let obtido: Vec<(i64, Option<i64>)> = q(&b, &sql, &[])
                    .into_iter()
                    .map(|l| match (&l[0], &l[1]) {
                        (Valor::Inteiro(x), Valor::Inteiro(y)) => (*x, Some(*y)),
                        (Valor::Inteiro(x), Valor::Nulo) => (*x, None),
                        _ => unreachable!(),
                    })
                    .collect();
                let casa =
                    |x: &(i64, Option<i64>, Option<String>),
                     y: &(i64, Option<i64>, Option<String>)| match on {
                        "a.k = b.k" => x.1.is_some() && x.1 == y.1,
                        "b.k = a.k AND a.t = b.t" => {
                            x.1.is_some() && x.1 == y.1 && x.2.is_some() && x.2 == y.2
                        }
                        "a.id = b.id" => x.0 == y.0,
                        "b.id = a.k" => x.1 == Some(y.0),
                        "a.k < b.k" => matches!((x.1, y.1), (Some(p), Some(q)) if p < q),
                        _ => x.2.is_some() && x.2 == y.2,
                    };
                let ok_where =
                    |x: &(i64, Option<i64>, Option<String>),
                     y: Option<&(i64, Option<i64>, Option<String>)>| {
                        match filtro {
                            "" => true,
                            " WHERE b.t = 'x'" => y.is_some_and(|y| y.2.as_deref() == Some("x")),
                            " WHERE a.id < 20 AND a.k = 3" => x.0 < 20 && x.1 == Some(3),
                            " WHERE a.id = 7" => x.0 == 7,
                            _ => y.is_none_or(|y| y.1.is_none()),
                        }
                    };
                let mut esperado = Vec::new();
                for x in &ta {
                    let mut achou = false;
                    for y in &tb {
                        if casa(x, y) {
                            achou = true;
                            if ok_where(x, Some(y)) {
                                esperado.push((x.0, Some(y.0)));
                            }
                        }
                    }
                    if !achou && esquerda && ok_where(x, None) {
                        esperado.push((x.0, None));
                    }
                }
                esperado.sort();
                assert_eq!(obtido, esperado, "{sql}");
                planos.insert(plano(&b, &sql, &[]));
            }
        }
    }
    let todos = planos.iter().cloned().collect::<Vec<_>>().join(" | ");
    for p in [
        "b: índice b_k",
        "b: busca pela chave primária",
        "b: varredura completa",
        "a: busca pela chave primária",
        "a: faixa da chave primária",
    ] {
        assert!(todos.contains(p), "plano não exercitado: {p}\n{todos}");
    }
}

#[test]
fn verify_confere_o_razao_e_a_ancora() {
    let (_vfs, b) = banco();
    exec(&b, "CREATE TABLE t (id INTEIRO PRIMARY KEY, v TEXTO)", &[]);
    for i in 0..30 {
        exec(&b, "INSERT INTO t VALUES (?, 'x')", &[v(i)]);
    }
    let item = |linhas: &[Vec<Valor>], nome: &str| {
        linhas
            .iter()
            .find(|l| l[0] == v(nome))
            .map(|l| l[1].clone())
            .unwrap_or_else(|| panic!("sem {nome}: {linhas:?}"))
    };
    assert!(somente_leitura("VERIFY").unwrap());
    let r = q(&b, "VERIFY", &[]);
    assert_eq!(item(&r, "resultado"), v("OK"), "{r:?}");
    assert_eq!(item(&r, "confirmacoes"), v(31));
    let Valor::Texto(ancora) = item(&r, "ancora") else {
        panic!()
    };
    exec(&b, "UPDATE t SET v = 'y' WHERE id = 3", &[]);
    let r = q(&b, &format!("VERIFY '{ancora}'"), &[]);
    assert_eq!(item(&r, "resultado"), v("OK"), "{r:?}");
    let falsa = format!("{}0", &ancora[..ancora.len() - 1]);
    let falsa = if falsa == ancora {
        format!("{}1", &ancora[..ancora.len() - 1])
    } else {
        falsa
    };
    let r = q(&b, &format!("VERIFICAR '{falsa}'"), &[]);
    assert_eq!(item(&r, "resultado"), v("FALHOU"), "{r:?}");
    assert!(matches!(erro(&b, "VERIFY 'lixo'"), ErroSql::Semantica(_)));
}
