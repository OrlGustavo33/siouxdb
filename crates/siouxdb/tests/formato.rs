//! Compatibilidade do formato: os arquivos de `spec/exemplos/` foram gravados pela versão
//! que congelou o formato 1 e precisam abrir, com o mesmo conteúdo, em toda versão futura.
//!
//! Para gerar de novo (só ao criar uma versão nova do formato):
//! `SIOUX_GERAR_EXEMPLOS=1 cargo test -p siouxdb --test formato gerar -- --ignored`

use siouxdb::armazenamento::{ParametrosSenha, SenhaNova, TamanhoPagina};
use siouxdb::catalogo::{Decimal, Valor};
use siouxdb::transacoes::Config;
use siouxdb::vfs::VfsReal;
use std::path::{Path, PathBuf};

const SENHA: &[u8] = b"exemplo-sioux-v1";

fn exemplos() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/exemplos")
}

fn sql(b: &siouxdb::BancoArquivo, comando: &str, params: &[Valor]) -> u64 {
    let mut tx = b.escrever().unwrap();
    siouxdb::sql::executar(&mut tx, comando, params).unwrap_or_else(|e| panic!("{comando}: {e}"));
    tx.confirmar().unwrap()
}

fn q(b: &siouxdb::BancoArquivo, comando: &str) -> Vec<Vec<Valor>> {
    siouxdb::sql::consultar(&b.ler().unwrap(), comando, &[])
        .unwrap_or_else(|e| panic!("{comando}: {e}"))
        .linhas
}

fn dec(s: &str) -> Valor {
    Valor::Decimal(Decimal::de_texto(s).unwrap())
}

/// Grava o conteúdo de exemplo: todos os tipos, índice, valor grande, histórico, remoção,
/// coluna acrescentada, e transações que ficam só no WAL (sem checkpoint).
fn gravar(caminho: &Path, tamanho: TamanhoPagina, senha: Option<SenhaNova<'_>>) {
    let b = siouxdb::BancoArquivo::criar(
        &VfsReal::novo(),
        caminho,
        tamanho,
        senha,
        Config {
            checkpoint_apos_paginas: 0,
            ..Config::default()
        },
    )
    .unwrap();
    sql(
        &b,
        "CREATE TABLE produtos (
            id SERIAL PRIMARY KEY, nome TEXTO NOT NULL, preco DECIMAL, peso REAL, ativo BOOLEANO,
            foto BYTES, criado DATAHORA, estoque INTEIRO DEFAULT 0);
         CREATE UNIQUE INDEX produtos_nome ON produtos (nome);
         CREATE TABLE notas (uf TEXTO, numero INTEIRO, texto TEXTO, PRIMARY KEY (uf, numero))",
        &[],
    );
    sql(
        &b,
        "INSERT INTO produtos (nome, preco, peso, ativo, foto, criado) VALUES
            ('CANETA', 2.5, 0.01, true, NULL, '2026-11-23 10:00:00'),
            ('CADERNO', 19.99, 0.4, true, ?, '2026-11-23 11:30:00.5'),
            ('BORRACHA', 0.1, NULL, false, NULL, NULL)",
        &[Valor::Bytes(vec![0, 1, 2, 255])],
    );
    sql(
        &b,
        "INSERT INTO notas VALUES ('SP', 1, ?), ('RJ', -7, 'curta')",
        &[Valor::Texto("n".repeat(30_000))],
    );
    sql(
        &b,
        "UPDATE produtos SET preco = preco + 0.2 WHERE nome = 'BORRACHA'",
        &[],
    );
    b.checkpoint().unwrap();
    // Daqui em diante fica só no WAL.
    sql(
        &b,
        "ALTER TABLE produtos ADD COLUMN marca TEXTO DEFAULT 'SIOUX'",
        &[],
    );
    sql(
        &b,
        "INSERT INTO produtos (nome, preco, marca) VALUES ('LAPIS', 1, 'OUTRA')",
        &[],
    );
    sql(
        &b,
        "DELETE FROM produtos WHERE nome = 'CANETA'; UPDATE produtos SET estoque = 12 WHERE id = 2",
        &[],
    );
}

/// Confere o conteúdo gravado por [`gravar`].
fn conferir(b: &siouxdb::BancoArquivo) {
    let t = |s: &str| Valor::Texto(s.into());
    assert_eq!(
        q(
            b,
            "SELECT id, nome, preco, peso, ativo, foto, criado, estoque, marca FROM produtos ORDER BY id"
        ),
        vec![
            vec![
                2.into(),
                t("CADERNO"),
                dec("19.99"),
                0.4.into(),
                true.into(),
                Valor::Bytes(vec![0, 1, 2, 255]),
                Valor::DataHora(1_795_433_400_500_000),
                12.into(),
                t("SIOUX")
            ],
            vec![
                3.into(),
                t("BORRACHA"),
                dec("0.3"),
                Valor::Nulo,
                false.into(),
                Valor::Nulo,
                Valor::Nulo,
                0.into(),
                t("SIOUX")
            ],
            vec![
                4.into(),
                t("LAPIS"),
                dec("1"),
                Valor::Nulo,
                Valor::Nulo,
                Valor::Nulo,
                Valor::Nulo,
                0.into(),
                t("OUTRA")
            ],
        ]
    );
    assert_eq!(
        q(b, "SELECT uf, numero FROM notas ORDER BY uf, numero"),
        vec![vec![t("RJ"), (-7).into()], vec![t("SP"), 1.into()]]
    );
    assert_eq!(
        q(b, "SELECT texto FROM notas WHERE uf = 'SP' AND numero = 1"),
        vec![vec![t(&"n".repeat(30_000))]]
    );
    // Histórico: a caneta existia na confirmação 2, com o preço original.
    assert_eq!(
        q(b, "SELECT nome, preco FROM produtos AS OF 2 WHERE id = 1"),
        vec![vec![t("CANETA"), dec("2.5")]]
    );
    assert_eq!(
        q(b, "SELECT preco FROM produtos AS OF 3 WHERE id = 3"),
        vec![vec![dec("0.1")]]
    );
    // Índice único e catálogo.
    assert_eq!(
        q(b, "EXPLAIN SELECT * FROM produtos WHERE nome = 'LAPIS'"),
        vec![vec![t("índice produtos_nome")]]
    );
    assert_eq!(
        q(b, "SELECT id FROM produtos WHERE nome = 'LAPIS'"),
        vec![vec![4.into()]]
    );
    assert_eq!(
        q(b, "SELECT nome, linhas FROM sys_tabelas ORDER BY nome"),
        vec![vec![t("notas"), 2.into()], vec![t("produtos"), 3.into()]]
    );
    assert_eq!(
        q(
            b,
            "SELECT COUNT(*) FROM sys_colunas WHERE tabela = 'produtos'"
        ),
        vec![vec![9.into()]]
    );
    assert_eq!(b.estatisticas().lsn_escrito, 7);
    // Todas as páginas íntegras e índices coerentes.
    let l = b.ler().unwrap();
    for n in siouxdb::armazenamento::PAGINAS_RESERVADAS..l.meta().total_paginas {
        l.pagina(n).unwrap();
    }
    for tab in siouxdb::catalogo::catalogo::tabelas(&l).unwrap() {
        tab.verificar_indice(&l).unwrap();
    }
}

#[test]
#[ignore = "gera os arquivos de exemplo; rode só ao criar uma versão nova do formato"]
fn gerar() {
    if std::env::var("SIOUX_GERAR_EXEMPLOS").is_err() {
        return;
    }
    let dir = exemplos();
    std::fs::create_dir_all(&dir).unwrap();
    for nome in [
        "v1-claro.sdb",
        "v1-claro.sdb-wal",
        "v1-cifrado.sdb",
        "v1-cifrado.sdb-wal",
    ] {
        let _ = std::fs::remove_file(dir.join(nome));
    }
    gravar(&dir.join("v1-claro.sdb"), TamanhoPagina::K8, None);
    gravar(
        &dir.join("v1-cifrado.sdb"),
        TamanhoPagina::K4,
        Some(SenhaNova {
            senha: SENHA,
            parametros: ParametrosSenha::default(),
        }),
    );
}

/// Copia o exemplo para uma pasta temporária (abrir grava no WAL) e devolve o caminho.
fn copia(nome: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("siouxdb-formato-{}-{nome}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for sufixo in ["", "-wal"] {
        std::fs::copy(
            exemplos().join(format!("{nome}{sufixo}")),
            dir.join(format!("{nome}{sufixo}")),
        )
        .unwrap();
    }
    dir.join(nome)
}

#[test]
fn abre_os_arquivos_da_versao_1() {
    for (nome, senha) in [("v1-claro.sdb", None), ("v1-cifrado.sdb", Some(SENHA))] {
        let caminho = copia(nome);
        let b = siouxdb::abrir_ou_criar(&caminho, senha, Config::default())
            .unwrap_or_else(|e| panic!("{nome}: {e}"));
        conferir(&b);
        // E continua utilizável: grava, faz checkpoint, reabre e confere de novo.
        sql(
            &b,
            "INSERT INTO produtos (nome, preco) VALUES ('NOVO', 9.9)",
            &[],
        );
        b.checkpoint().unwrap();
        drop(b);
        let b = siouxdb::abrir_ou_criar(&caminho, senha, Config::default()).unwrap();
        assert_eq!(
            q(&b, "SELECT id, marca FROM produtos WHERE nome = 'NOVO'"),
            vec![vec![5.into(), Valor::Texto("SIOUX".into())]]
        );
        sql(&b, "DELETE FROM produtos WHERE nome = 'NOVO'", &[]);
        assert_eq!(q(&b, "SELECT COUNT(*) FROM produtos"), vec![vec![3.into()]]);
        drop(b);
        std::fs::remove_dir_all(caminho.parent().unwrap()).unwrap();
    }
    // Senha errada no cifrado continua sendo recusada.
    let caminho = copia("v1-cifrado.sdb");
    assert!(siouxdb::abrir_ou_criar(&caminho, Some(b"errada"), Config::default()).is_err());
    assert!(siouxdb::abrir_ou_criar(&caminho, None, Config::default()).is_err());
    std::fs::remove_dir_all(caminho.parent().unwrap()).unwrap();
}
