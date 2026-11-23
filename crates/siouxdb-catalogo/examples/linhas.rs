//! Mede inserir, buscar por chave e varrer N linhas num banco cifrado em disco real.
//!
//! ```text
//! cargo run --release -p siouxdb-catalogo --example linhas -- [linhas] [pasta] [cache em páginas]
//! ```

use siouxdb_armazenamento::{ParametrosSenha, SenhaNova, TamanhoPagina};
use siouxdb_catalogo::catalogo::criar_tabela;
use siouxdb_catalogo::{Coluna, Tipo, Valor};
use siouxdb_transacoes::{Banco, Config};
use siouxdb_vfs::{Vfs, VfsReal};
use std::time::Instant;

type Bd = Banco<<VfsReal as Vfs>::Arquivo>;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let n: i64 = args
        .first()
        .and_then(|a| a.parse().ok())
        .unwrap_or(1_000_000);
    let pasta = args
        .get(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let cache: usize = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(1024);
    let caminho = pasta.join(format!("siouxdb-linhas-{}.sdb", std::process::id()));
    let senha = SenhaNova {
        senha: b"senha-da-medicao",
        parametros: ParametrosSenha::default(),
    };
    let b = Bd::criar(
        &VfsReal::novo(),
        &caminho,
        TamanhoPagina::K8,
        Some(senha),
        Config {
            cache_paginas: cache,
            cache_wal: cache / 4,
            ..Config::default()
        },
    )
    .expect("criar");
    let cols = [
        Coluna::nova("id", Tipo::Inteiro),
        Coluna::nova("nome", Tipo::Texto),
        Coluna::nova("saldo", Tipo::Real),
    ];

    let t0 = Instant::now();
    let lote = 10_000;
    let mut tabela = None;
    for ini in (0..n).step_by(lote) {
        let mut tx = b.escrever().expect("tx");
        let t = match &tabela {
            Some(t) => siouxdb_catalogo::catalogo::tabela_obrigatoria(&tx, "clientes")
                .map(|_| ())
                .and(Ok(t))
                .expect("t"),
            None => {
                tabela =
                    Some(criar_tabela(&mut tx, "clientes", &cols, &["id"]).expect("criar tabela"));
                tabela.as_ref().expect("t")
            }
        }
        .clone();
        for i in ini..(ini + lote as i64).min(n) {
            // Ordem embaralhada (multiplicação por número primo) para não favorecer a árvore.
            let id = (i.wrapping_mul(2_654_435_761) % n + n) % n;
            t.inserir(
                &mut tx,
                &[
                    id.into(),
                    Valor::Texto(format!("CLIENTE NUMERO {id}")),
                    (id as f64 * 1.5).into(),
                ],
            )
            .expect("inserir");
        }
        tx.confirmar().expect("confirmar");
    }
    let insercao = t0.elapsed().as_secs_f64();
    b.checkpoint().expect("checkpoint");

    let l = b.ler().expect("ler");
    let t = siouxdb_catalogo::catalogo::tabela_obrigatoria(&l, "clientes").expect("tabela");
    let buscas = 200_000.min(n);
    let t1 = Instant::now();
    for i in 0..buscas {
        let id = (i.wrapping_mul(7919) % n + n) % n;
        assert!(t.obter(&l, &[id.into()]).expect("obter").is_some());
    }
    let busca = t1.elapsed().as_secs_f64();
    let t1b = Instant::now();
    for i in 0..buscas {
        let id = (i.wrapping_mul(7919) % n + n) % n;
        assert!(t.obter(&l, &[id.into()]).expect("obter").is_some());
    }
    let busca_quente = t1b.elapsed().as_secs_f64();
    let t2 = Instant::now();
    let total = t
        .varrer(&l, None, None)
        .expect("varrer")
        .todas()
        .expect("todas")
        .len();
    let varredura = t2.elapsed().as_secs_f64();
    assert_eq!(total as i64, n);
    let r = t.verificar_indice(&l).expect("verificar");
    let tamanho = std::fs::metadata(&caminho).map(|m| m.len()).unwrap_or(0);
    println!(
        "{n} linhas, banco cifrado, páginas de 8 KiB, lotes de {lote} por transação, cache de {cache} páginas"
    );
    println!(
        "inserir   {:.0} linhas/s ({insercao:.1} s, ordem embaralhada)",
        n as f64 / insercao
    );
    println!(
        "buscar    {:.0} buscas/s pela chave, cache frio; {:.0}/s com cache quente ({buscas} buscas)",
        buscas as f64 / busca,
        buscas as f64 / busca_quente
    );
    println!(
        "varrer    {:.0} linhas/s ({varredura:.2} s)",
        n as f64 / varredura
    );
    println!(
        "índice    altura {}, {} páginas; arquivo {:.0} MiB",
        r.altura,
        r.paginas,
        tamanho as f64 / 1048576.0
    );
    drop(l);
    drop(b);
    let _ = std::fs::remove_file(&caminho);
    let mut wal = caminho.into_os_string();
    wal.push("-wal");
    let _ = std::fs::remove_file(wal);
}
