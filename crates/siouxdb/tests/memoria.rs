//! Memória limitada com carga grande: a transação derrama páginas no WAL e as consultas
//! andam em fluxo, então o pico de memória do processo não acompanha o tamanho dos dados.
//! Só no Linux (lê o pico em `/proc/self/status`). Um teste por arquivo: o pico é do processo.

#![cfg(target_os = "linux")]

use siouxdb::catalogo::{Valor, catalogo};
use siouxdb::transacoes::Config;

/// Pico de memória residente do processo, em MiB.
fn pico_mib() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    let kb: u64 = s
        .lines()
        .find(|l| l.starts_with("VmHWM:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|n| n.parse().ok())
        .unwrap();
    kb / 1024
}

#[test]
fn carga_grande_com_memoria_limitada() {
    let dir = std::env::temp_dir().join(format!("siouxdb-memoria-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = Config {
        cache_paginas: 256, // 2 MiB
        cache_wal: 64,
        paginas_sujas_max: 256, // 2 MiB por transação
        ..Config::default()
    };
    siouxdb::sql::definir_limite_memoria(8 << 20);
    let b = siouxdb::abrir_ou_criar(&dir.join("m.sdb"), None, config).unwrap();
    let n: i64 = 60_000;
    let texto = "y".repeat(1024);
    let inicio = pico_mib();

    // ~60 MiB de linhas numa transação só.
    let mut tx = b.escrever().unwrap();
    siouxdb::sql::executar(
        &mut tx,
        "CREATE TABLE m (id INTEIRO PRIMARY KEY, g INTEIRO, v INTEIRO, txt TEXTO)",
        &[],
    )
    .unwrap();
    let t = catalogo::tabela_obrigatoria(&tx, "m").unwrap();
    for i in 0..n {
        let linha = [
            Valor::Inteiro(i),
            Valor::Inteiro(i % 7),
            Valor::Inteiro((i * 7919) % n),
            Valor::Texto(texto.clone()),
        ];
        t.inserir(&mut tx, &linha).unwrap();
    }
    tx.confirmar().unwrap();
    let apos_inserir = pico_mib();

    let mut tx = b.escrever().unwrap();
    let s = siouxdb::sql::executar(&mut tx, "UPDATE m SET v = v + 1", &[]).unwrap();
    assert_eq!(s[0].afetadas, n as u64);
    let s = siouxdb::sql::executar(&mut tx, "DELETE FROM m WHERE g >= 4", &[]).unwrap();
    let restantes = n - s[0].afetadas as i64;
    tx.confirmar().unwrap();

    let mut tx = b.escrever().unwrap();
    let t = catalogo::tabela_obrigatoria(&tx, "m").unwrap();
    t.compactar(&mut tx, u64::MAX).unwrap();
    tx.confirmar().unwrap();

    let l = b.ler().unwrap();
    let um = |q: &str| siouxdb::sql::consultar(&l, q, &[]).unwrap().linhas;
    assert_eq!(
        um("SELECT COUNT(*) FROM m")[0][0],
        Valor::Inteiro(restantes)
    );
    let top = um("SELECT v FROM m ORDER BY v DESC LIMIT 3");
    assert_eq!(top.len(), 3);
    assert_eq!(um("SELECT g, COUNT(*) FROM m GROUP BY g").len(), 4);
    assert!(matches!(
        siouxdb::sql::consultar(&l, "SELECT * FROM m", &[]),
        Err(siouxdb::sql::ErroSql::MemoriaEsgotada { .. })
    ));
    assert!(siouxdb::transacoes::razao::verificar(&l, None).ok());
    drop(l);
    drop(b);
    let fim = pico_mib();
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!(
        "pico de memória: início {inicio} MiB, após inserir {apos_inserir} MiB, fim {fim} MiB"
    );
    // Os dados passam de 60 MiB; sem derrame e fluxo, só a inserção passaria de 120 MiB.
    assert!(
        fim.saturating_sub(inicio) < 48,
        "pico cresceu demais: {inicio} → {fim} MiB"
    );
}
