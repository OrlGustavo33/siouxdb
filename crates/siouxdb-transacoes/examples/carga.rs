//! Carga de teste com disco real.
//!
//! ```text
//! cargo run --release -p siouxdb-transacoes --example carga -- [escritores] [segundos] [total|normal] [pasta]
//! ```
//!
//! Cada escritor confirma transações de 1 página alterada; dois leitores leem o tempo todo.
//! Mostra confirmações por segundo, quantas confirmações cada `fsync` atendeu (confirmação
//! em grupo) e a latência da confirmação. Banco cifrado, páginas de 8 KiB.

use siouxdb_armazenamento::{ParametrosSenha, SenhaNova, TamanhoPagina, TipoPagina};
use siouxdb_metricas::Histograma;
use siouxdb_transacoes::{Banco, Config, Sincronia};
use siouxdb_vfs::{Vfs, VfsReal};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

type Bd = Banco<<VfsReal as Vfs>::Arquivo>;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let escritores: u64 = args.first().and_then(|a| a.parse().ok()).unwrap_or(8);
    let segundos: u64 = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(5);
    let sincronia = match args.get(2).map(String::as_str) {
        Some("normal") => Sincronia::Normal,
        _ => Sincronia::Total,
    };
    let pasta = args
        .get(3)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let caminho = pasta.join(format!("siouxdb-carga-{}.sdb", std::process::id()));
    let wal = pasta.join(format!("siouxdb-carga-{}.sdb-wal", std::process::id()));

    let senha = SenhaNova {
        senha: b"senha-da-carga",
        parametros: ParametrosSenha::default(),
    };
    let config = Config {
        sincronia,
        ..Config::default()
    };
    let b = Bd::criar(
        &VfsReal::novo(),
        &caminho,
        TamanhoPagina::K8,
        Some(senha),
        config,
    )
    .expect("criar");
    let paginas: Vec<u64> = {
        let mut tx = b.escrever().expect("tx");
        let v = (0..escritores)
            .map(|_| tx.alocar(TipoPagina::NoArvore).expect("alocar"))
            .collect();
        tx.confirmar().expect("confirmar");
        v
    };
    let parar = AtomicBool::new(false);
    let leituras = AtomicU64::new(0);
    let latencia = Histograma::novo();
    let inicio = Instant::now();
    std::thread::scope(|s| {
        for &n in &paginas {
            let (b, parar, latencia) = (&b, &parar, &latencia);
            s.spawn(move || {
                let mut i = 0u64;
                while !parar.load(Ordering::Relaxed) {
                    let t = Instant::now();
                    let mut tx = b.escrever().expect("tx");
                    tx.pagina_mut(n).expect("pagina").conteudo_mut()[..8]
                        .copy_from_slice(&i.to_le_bytes());
                    tx.confirmar().expect("confirmar");
                    latencia.registrar_duracao(t.elapsed());
                    i += 1;
                }
            });
        }
        for _ in 0..2 {
            let (b, parar, leituras, paginas) = (&b, &parar, &leituras, &paginas);
            s.spawn(move || {
                while !parar.load(Ordering::Relaxed) {
                    let l = b.ler().expect("ler");
                    for &n in paginas {
                        l.pagina(n).expect("pagina");
                        leituras.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
        std::thread::sleep(Duration::from_secs(segundos));
        parar.store(true, Ordering::Relaxed);
    });
    let dt = inicio.elapsed().as_secs_f64();
    let e = b.estatisticas();
    let f = latencia.foto();
    let ms = |v: Option<u64>| v.map(|n| n as f64 / 1e6).unwrap_or(0.0);
    println!(
        "sincronia {sincronia:?}, {escritores} escritores + 2 leitores, {dt:.1} s, banco cifrado"
    );
    println!(
        "confirmações   {} ({:.0}/s)",
        e.confirmacoes,
        e.confirmacoes as f64 / dt
    );
    println!(
        "fsyncs do WAL  {} ({:.1} confirmações por fsync)",
        e.sincronizacoes,
        e.confirmacoes as f64 / e.sincronizacoes.max(1) as f64
    );
    println!(
        "checkpoints    {} (WAL no fim: {} páginas, {:.1} MiB)",
        e.checkpoints,
        e.paginas_no_wal,
        e.bytes_wal as f64 / 1048576.0
    );
    println!(
        "leituras       {:.0}/s",
        leituras.load(Ordering::Relaxed) as f64 / dt
    );
    println!(
        "latência da confirmação: p50 {:.2} ms · p99 {:.2} ms · máx {:.2} ms",
        ms(f.percentil(0.5)),
        ms(f.percentil(0.99)),
        f.maximo as f64 / 1e6
    );
    drop(b);
    let _ = std::fs::remove_file(caminho);
    let _ = std::fs::remove_file(wal);
}
