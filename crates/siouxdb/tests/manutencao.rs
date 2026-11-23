//! Manutenção automática com arquivos de verdade.

use siouxdb::manutencao::{
    ConfigBackup, ConfigManutencao, compactar_se_preciso, fazer_backup, iniciar,
};
use siouxdb::transacoes::Config;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn pasta(nome: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("siouxdb-manut-{}-{nome}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn sql(b: &siouxdb::BancoArquivo, comando: &str) {
    let mut tx = b.escrever().unwrap();
    siouxdb::sql::executar(&mut tx, comando, &[]).unwrap();
    tx.confirmar().unwrap();
}

fn um(b: &siouxdb::BancoArquivo, comando: &str) -> siouxdb::catalogo::Valor {
    siouxdb::sql::consultar(&b.ler().unwrap(), comando, &[])
        .unwrap()
        .linhas[0][0]
        .clone()
}

#[test]
fn compacta_quando_passa_do_limite_e_preserva_historico_recente() {
    let dir = pasta("compactar");
    let b = siouxdb::abrir_ou_criar(&dir.join("b.sdb"), None, Config::default()).unwrap();
    sql(
        &b,
        "CREATE TABLE t (id INT PRIMARY KEY, n INT); CREATE TABLE quieta (id INT PRIMARY KEY)",
    );
    sql(
        &b,
        "INSERT INTO t VALUES (1, 0), (2, 0), (3, 0); INSERT INTO quieta VALUES (1)",
    );
    for i in 1..=60 {
        sql(&b, &format!("UPDATE t SET n = {i}"));
    }
    let lsn_recente = b.estatisticas().lsn_escrito - 5;
    let cfg = ConfigManutencao {
        versoes_minimas: 50,
        manter_confirmacoes: 10,
        ..Default::default()
    };
    let feitas = compactar_se_preciso(&b, &cfg).unwrap();
    assert_eq!(feitas.len(), 1, "só a tabela com muito histórico");
    assert_eq!(feitas[0].0, "t");
    assert!(
        feitas[0].1.versoes_depois < 3 * 15 && feitas[0].1.versoes_antes == 183,
        "{:?}",
        feitas[0].1
    );
    assert_eq!(um(&b, "SELECT SUM(n) FROM t"), 180.into());
    // O histórico dentro da janela continua exato.
    assert_eq!(
        um(
            &b,
            &format!("SELECT n FROM t AS OF {lsn_recente} WHERE id = 1")
        ),
        55.into()
    );
    // Rodar de novo não faz nada.
    assert!(compactar_se_preciso(&b, &cfg).unwrap().is_empty());
    drop(b);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn backup_com_rodizio_teste_e_restauracao() {
    let dir = pasta("backup");
    let senha: &[u8] = b"senha-do-teste";
    let b = siouxdb::abrir_ou_criar(&dir.join("b.sdb"), Some(senha), Config::default()).unwrap();
    sql(&b, "CREATE TABLE t (id SERIAL PRIMARY KEY, valor DECIMAL)");
    let cfg = ConfigBackup {
        manter: 2,
        senha_para_testar: Some(senha.to_vec()),
        ..ConfigBackup::em(dir.join("copias"))
    };
    let mut copias = Vec::new();
    for i in 1..=4 {
        sql(&b, &format!("INSERT INTO t (valor) VALUES ({i}.5)"));
        copias.push(fazer_backup(&b, &cfg).unwrap());
    }
    // Ficaram só as duas mais novas (com o WAL de cada uma).
    assert!(!copias[0].exists() && !copias[1].exists() && copias[2].exists() && copias[3].exists());
    let nomes: Vec<_> = std::fs::read_dir(dir.join("copias"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(nomes.len(), 4, "{nomes:?}");
    // Restaurar = abrir a cópia: a penúltima tem 3 linhas, a última 4.
    let penultima = siouxdb::abrir_ou_criar(&copias[2], Some(senha), Config::default()).unwrap();
    assert_eq!(um(&penultima, "SELECT COUNT(*) FROM t"), 3.into());
    drop(penultima);
    // Senha errada no teste: a cópia é apagada e vira erro.
    let ruim = ConfigBackup {
        senha_para_testar: Some(b"errada".to_vec()),
        ..cfg.clone()
    };
    assert!(fazer_backup(&b, &ruim).is_err());
    assert_eq!(
        std::fs::read_dir(dir.join("copias")).unwrap().count(),
        4,
        "cópia que não abre não fica"
    );
    drop(b);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn tarefa_faz_backup_compacta_e_para() {
    let dir = pasta("tarefa");
    let b = Arc::new(siouxdb::abrir_ou_criar(&dir.join("b.sdb"), None, Config::default()).unwrap());
    sql(
        &b,
        "CREATE TABLE t (id INT PRIMARY KEY, n INT); INSERT INTO t VALUES (1, 0)",
    );
    for i in 0..40 {
        sql(&b, &format!("UPDATE t SET n = {i}"));
    }
    let cfg = ConfigManutencao {
        intervalo: Duration::from_millis(30),
        versoes_minimas: 20,
        manter_confirmacoes: 0,
        backup: Some(ConfigBackup {
            intervalo: Duration::from_millis(60),
            manter: 3,
            ..ConfigBackup::em(dir.join("copias"))
        }),
        ..Default::default()
    };
    let m = iniciar(Arc::clone(&b), cfg);
    let inicio = std::time::Instant::now();
    while (m.estado().backups < 2 || m.estado().compactacoes < 1)
        && inicio.elapsed() < Duration::from_secs(20)
    {
        // O banco continua em uso durante a manutenção.
        sql(&b, "UPDATE t SET n = n + 1");
        std::thread::sleep(Duration::from_millis(5));
    }
    let e = m.estado();
    assert!(
        e.backups >= 2 && e.compactacoes >= 1 && e.ultimo_erro.is_none(),
        "{e:?}"
    );
    m.parar();
    assert!(std::fs::read_dir(dir.join("copias")).unwrap().count() <= 6);
    assert_eq!(um(&b, "SELECT COUNT(*) FROM t"), 1.into());
    drop(b);
    std::fs::remove_dir_all(dir).unwrap();
}
