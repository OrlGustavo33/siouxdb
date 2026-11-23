use crate::{Banco, Config, ErroBanco, Sincronia};
use siouxdb_armazenamento::{
    ErroArmazenamento, NumeroPagina, ParametrosSenha, SenhaNova, TamanhoPagina, TipoPagina,
    crc32c_continuar,
};
use siouxdb_vfs::{Arquivo, ConfigSimulacao, ErroVfs, ModoAbertura, Vfs, VfsSimulado};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

type B = Banco<<VfsSimulado as Vfs>::Arquivo>;
const SENHA: &[u8] = b"senha de teste";

fn disco(semente: u64) -> VfsSimulado {
    VfsSimulado::novo(ConfigSimulacao {
        semente,
        ..ConfigSimulacao::default()
    })
}

fn caminho() -> PathBuf {
    PathBuf::from("banco.sdb")
}

fn criar(vfs: &VfsSimulado, cifrado: bool, config: Config) -> B {
    let senha = cifrado.then_some(SenhaNova {
        senha: SENHA,
        parametros: ParametrosSenha::para_testes(),
    });
    B::criar(vfs, &caminho(), TamanhoPagina::K4, senha, config).unwrap()
}

fn abrir(vfs: &VfsSimulado, cifrado: bool, config: Config) -> B {
    B::abrir(vfs, &caminho(), cifrado.then_some(SENHA), config).unwrap()
}

fn marca(texto: &str) -> [u8; 16] {
    let mut m = [b'.'; 16];
    let b = texto.as_bytes();
    m[..b.len().min(16)].copy_from_slice(&b[..b.len().min(16)]);
    m
}

fn escrever(
    tx: &mut crate::Transacao<'_, <VfsSimulado as Vfs>::Arquivo>,
    n: NumeroPagina,
    m: [u8; 16],
) {
    let mut p = tx.pagina_nova(TipoPagina::NoArvore);
    p.conteudo_mut()[..16].copy_from_slice(&m);
    tx.gravar(n, p).unwrap();
}

fn conteudo(b: &B, n: NumeroPagina) -> [u8; 16] {
    b.ler().unwrap().pagina(n).unwrap().conteudo()[..16]
        .try_into()
        .unwrap()
}

#[test]
fn confirma_desfaz_e_reabre() {
    for cifrado in [false, true] {
        let vfs = disco(1);
        let b = criar(&vfs, cifrado, Config::default());
        let mut tx = b.escrever().unwrap();
        let a = tx.alocar(TipoPagina::NoArvore).unwrap();
        escrever(&mut tx, a, marca("primeira"));
        tx.definir_raiz_catalogo(a);
        assert_eq!(tx.confirmar().unwrap(), 1);
        assert_eq!(conteudo(&b, a), marca("primeira"));

        // Desfazer não deixa rastro, nem a página alocada.
        let mut tx = b.escrever().unwrap();
        escrever(&mut tx, a, marca("descartada"));
        let x = tx.alocar(TipoPagina::NoArvore).unwrap();
        tx.desfazer();
        assert_eq!(conteudo(&b, a), marca("primeira"));
        assert!(matches!(
            b.ler().unwrap().pagina(x),
            Err(ErroBanco::PaginaInexistente(_))
        ));

        // Reabre sem checkpoint: vem do WAL.
        drop(b);
        vfs.queda();
        let b = abrir(&vfs, cifrado, Config::default());
        assert_eq!(conteudo(&b, a), marca("primeira"));
        assert_eq!(b.ler().unwrap().meta().raiz_catalogo, a);
        assert!(b.estatisticas().paginas_no_wal > 0);

        // Checkpoint esvazia o WAL; o dado continua.
        b.checkpoint().unwrap();
        let e = b.estatisticas();
        assert_eq!((e.paginas_no_wal, e.lsn_checkpoint), (0, 1));
        drop(b);
        vfs.queda();
        let b = abrir(&vfs, cifrado, Config::default());
        assert_eq!(conteudo(&b, a), marca("primeira"));
        assert_eq!(b.estatisticas().lsn_escrito, 1);
    }
}

#[test]
fn foto_nao_muda_e_checkpoint_respeita_a_foto() {
    let vfs = disco(2);
    let b = criar(
        &vfs,
        false,
        Config {
            checkpoint_apos_paginas: 0,
            ..Config::default()
        },
    );
    let mut tx = b.escrever().unwrap();
    let n = tx.alocar(TipoPagina::NoArvore).unwrap();
    escrever(&mut tx, n, marca("v1"));
    tx.confirmar().unwrap();

    let foto = b.ler().unwrap();
    for v in ["v2", "v3"] {
        let mut tx = b.escrever().unwrap();
        escrever(&mut tx, n, marca(v));
        tx.confirmar().unwrap();
    }
    b.checkpoint().unwrap();
    // A foto antiga segura o checkpoint: o arquivo principal não pode receber v3 ainda.
    assert_eq!(b.estatisticas().lsn_checkpoint, 1);
    assert_eq!(foto.pagina(n).unwrap().conteudo()[..16], marca("v1"));
    assert_eq!(conteudo(&b, n), marca("v3"));
    drop(foto);
    b.checkpoint().unwrap();
    let e = b.estatisticas();
    assert_eq!(
        (e.lsn_checkpoint, e.paginas_no_wal, e.leituras_abertas),
        (3, 0, 0)
    );
    assert_eq!(conteudo(&b, n), marca("v3"));
}

#[test]
fn wal_reinicia_com_foto_aberta_em_dia_e_ela_continua_certa() {
    let vfs = disco(8);
    let b = criar(
        &vfs,
        true,
        Config {
            checkpoint_apos_paginas: 0,
            ..Config::default()
        },
    );
    let mut tx = b.escrever().unwrap();
    let n = tx.alocar(TipoPagina::NoArvore).unwrap();
    escrever(&mut tx, n, marca("v1"));
    tx.confirmar().unwrap();
    let foto = b.ler().unwrap();
    assert_eq!(foto.pagina(n).unwrap().conteudo()[..16], marca("v1")); // vem do WAL (e do cache)
    b.checkpoint().unwrap();
    // A foto está no LSN do checkpoint: não precisa do WAL, e ele reinicia.
    assert_eq!(b.estatisticas().paginas_no_wal, 0);
    let mut tx = b.escrever().unwrap();
    escrever(&mut tx, n, marca("v2"));
    tx.confirmar().unwrap();
    // O quadro novo pode ocupar a mesma posição do antigo; a foto continua vendo v1.
    assert_eq!(foto.pagina(n).unwrap().conteudo()[..16], marca("v1"));
    assert_eq!(conteudo(&b, n), marca("v2"));
}

#[test]
fn checkpoint_automatico_mantem_o_wal_limitado_sob_carga() {
    let vfs = disco(9);
    let b = criar(
        &vfs,
        false,
        Config {
            checkpoint_apos_paginas: 20,
            ..Config::default()
        },
    );
    let mut tx = b.escrever().unwrap();
    let paginas: Vec<_> = (0..4)
        .map(|_| tx.alocar(TipoPagina::NoArvore).unwrap())
        .collect();
    tx.confirmar().unwrap();
    std::thread::scope(|s| {
        for &n in &paginas {
            let b = &b;
            s.spawn(move || {
                for i in 0..150 {
                    let mut tx = b.escrever().unwrap();
                    escrever(&mut tx, n, marca(&format!("{n}-{i}")));
                    tx.confirmar().unwrap();
                    let _foto = b.ler().unwrap(); // leitor sempre aparecendo
                }
            });
        }
    });
    let e = b.estatisticas();
    assert!(e.checkpoints >= 600 / 20 / 2, "{e:?}");
    assert!(e.checkpoints <= 600 / 20 + 2, "checkpoint demais: {e:?}");
    assert!(e.paginas_no_wal <= 200, "WAL cresceu demais: {e:?}");
    for &n in &paginas {
        assert_eq!(conteudo(&b, n), marca(&format!("{n}-149")));
    }
}

#[test]
fn liberar_e_reaproveitar() {
    let vfs = disco(3);
    let b = criar(&vfs, true, Config::default());
    let mut tx = b.escrever().unwrap();
    let a = tx.alocar(TipoPagina::NoArvore).unwrap();
    let c = tx.alocar(TipoPagina::NoArvore).unwrap();
    tx.confirmar().unwrap();
    let mut tx = b.escrever().unwrap();
    tx.liberar(a).unwrap();
    assert!(matches!(tx.liberar(a), Err(ErroBanco::PaginaJaLivre(_))));
    tx.confirmar().unwrap();
    let mut tx = b.escrever().unwrap();
    assert_eq!(
        tx.alocar(TipoPagina::Catalogo).unwrap(),
        a,
        "a livre é reaproveitada"
    );
    assert_eq!(
        tx.alocar(TipoPagina::Catalogo).unwrap(),
        c + 1,
        "sem livres, cresce"
    );
}

#[test]
fn segundo_dono_recebe_em_uso_e_fila_tem_limite() {
    let vfs = disco(4);
    let b = criar(&vfs, false, Config::default());
    assert!(matches!(
        B::abrir(&vfs, &caminho(), None, Config::default()),
        Err(ErroBanco::Armazenamento(ErroArmazenamento::Vfs(
            ErroVfs::EmUso(_)
        )))
    ));
    let tx = b.escrever().unwrap();
    // Leitura não espera o escritor.
    assert!(b.ler().is_ok());
    assert!(matches!(
        b.escrever_esperando(Duration::from_millis(20)),
        Err(ErroBanco::EsperaEsgotada(_))
    ));
    drop(tx);
    assert!(b.escrever_esperando(Duration::from_millis(20)).is_ok());
}

#[test]
fn confirmacao_em_grupo_com_varias_threads() {
    let vfs = disco(5);
    let b = criar(
        &vfs,
        false,
        Config {
            checkpoint_apos_paginas: 50,
            ..Config::default()
        },
    );
    let paginas: Vec<NumeroPagina> = {
        let mut tx = b.escrever().unwrap();
        let v = (0..8)
            .map(|_| tx.alocar(TipoPagina::NoArvore).unwrap())
            .collect();
        tx.confirmar().unwrap();
        v
    };
    std::thread::scope(|s| {
        for (t, &n) in paginas.iter().enumerate() {
            let b = &b;
            s.spawn(move || {
                for i in 0..40 {
                    let mut tx = b.escrever().unwrap();
                    escrever(&mut tx, n, marca(&format!("t{t}-{i}")));
                    tx.confirmar().unwrap();
                    // leitores no meio da carga
                    let l = b.ler().unwrap();
                    l.pagina(n).unwrap();
                }
            });
        }
    });
    let e = b.estatisticas();
    assert_eq!(e.confirmacoes, 1 + 8 * 40);
    assert!(e.sincronizacoes <= e.confirmacoes, "{e:?}");
    for (t, &n) in paginas.iter().enumerate() {
        assert_eq!(conteudo(&b, n), marca(&format!("t{t}-39")));
    }
    drop(b);
    vfs.queda();
    let b = abrir(&vfs, false, Config::default());
    for (t, &n) in paginas.iter().enumerate() {
        assert_eq!(
            conteudo(&b, n),
            marca(&format!("t{t}-39")),
            "confirmado sumiu na queda"
        );
    }
}

#[test]
fn wal_alterado_sem_a_chave_nao_abre() {
    let vfs = disco(6);
    let b = criar(
        &vfs,
        true,
        Config {
            checkpoint_apos_paginas: 0,
            ..Config::default()
        },
    );
    let mut tx = b.escrever().unwrap();
    let n = tx.alocar(TipoPagina::NoArvore).unwrap();
    escrever(&mut tx, n, marca("ok"));
    tx.confirmar().unwrap();
    drop(b);

    // Muda a raiz do catálogo na confirmação e refaz o CRC (que qualquer um sabe calcular).
    let wal = vfs
        .abrir(Path::new("banco.sdb-wal"), ModoAbertura::LeituraGravacao)
        .unwrap();
    let mut bytes = vec![0u8; wal.tamanho().unwrap() as usize];
    wal.ler_exato_em(0, &mut bytes).unwrap();
    let pos_conf = bytes.len() - (40 + 88);
    let crc_anterior = u32::from_le_bytes(
        bytes[pos_conf - 4096 - 8..pos_conf - 4096 - 4]
            .try_into()
            .unwrap(),
    );
    bytes[pos_conf + 40 + 16] = 7; // raiz_catalogo
    let crc = crc32c_continuar(
        crc32c_continuar(crc_anterior, &bytes[pos_conf..pos_conf + 32]),
        &bytes[pos_conf + 40..],
    );
    bytes[pos_conf + 32..pos_conf + 36].copy_from_slice(&crc.to_le_bytes());
    wal.gravar_em(0, &bytes).unwrap();
    wal.sincronizar().unwrap();
    drop(wal);
    assert!(matches!(
        B::abrir(&vfs, &caminho(), Some(SENHA), Config::default()),
        Err(ErroBanco::WalAdulterado)
    ));
}

#[test]
fn wal_de_outro_banco_e_ignorado() {
    let vfs = disco(7);
    let b = criar(
        &vfs,
        false,
        Config {
            checkpoint_apos_paginas: 0,
            ..Config::default()
        },
    );
    let mut tx = b.escrever().unwrap();
    let n = tx.alocar(TipoPagina::NoArvore).unwrap();
    escrever(&mut tx, n, marca("do outro"));
    tx.confirmar().unwrap();
    drop(b);
    // Apaga o banco e cria outro com o mesmo nome; o WAL velho continua lá.
    vfs.apagar(&caminho()).unwrap();
    let b = criar(&vfs, false, Config::default());
    assert_eq!(b.ler().unwrap().meta().total_paginas, 2);
    assert_eq!(b.estatisticas().lsn_escrito, 0);
}

/// Gerador pequeno e determinístico para o teste de modelo.
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

type Modelo = BTreeMap<NumeroPagina, [u8; 16]>;

/// Depois de reabrir: o banco precisa estar exatamente numa confirmação do histórico
/// (a última, com sincronia Total) e a lista de livres não pode entregar página em uso.
fn conferir_apos_queda(
    b: &B,
    historico: &mut Vec<(u64, Modelo)>,
    sincronia: Sincronia,
    minimo: u64,
    ctx: &str,
) -> Modelo {
    let l = b.ler().unwrap();
    let lsn = l.lsn();
    match sincronia {
        Sincronia::Total => assert_eq!(
            lsn,
            historico.last().unwrap().0,
            "{ctx}: confirmação perdida"
        ),
        Sincronia::Normal => assert!(
            lsn >= minimo,
            "{ctx}: voltou para antes do checkpoint ({lsn} < {minimo})"
        ),
    }
    let i = historico
        .iter()
        .position(|(h, _)| *h == lsn)
        .unwrap_or_else(|| panic!("{ctx}: LSN {lsn} não está no histórico"));
    historico.truncate(i + 1);
    let modelo = historico[i].1.clone();
    for (n, m) in &modelo {
        let p = l
            .pagina(*n)
            .unwrap_or_else(|e| panic!("{ctx}: página {n}: {e}"));
        assert_eq!(&p.conteudo()[..16], m, "{ctx}: página {n}");
    }
    drop(l);
    let mut tx = b.escrever().unwrap();
    for _ in 0..4 {
        let n = tx.alocar(TipoPagina::NoArvore).unwrap();
        assert!(
            !modelo.contains_key(&n),
            "{ctx}: página {n} em uso entregue como livre"
        );
    }
    tx.desfazer();
    modelo
}

#[test]
fn modelo_com_quedas_em_qualquer_ponto() {
    let modos = [
        (false, Sincronia::Total),
        (true, Sincronia::Total),
        (false, Sincronia::Normal),
        (true, Sincronia::Normal),
    ];
    let mut quedas = 0;
    for semente in 0..60 * fator_simulacao() {
        for (cifrado, sincronia) in modos {
            let ctx0 = format!("semente {semente}, cifrado {cifrado}, {sincronia:?}");
            let vfs = disco(semente);
            let config = Config {
                sincronia,
                checkpoint_apos_paginas: 12,
                ..Config::default()
            };
            let mut sorteio = Sorteio(semente.wrapping_mul(7919) + u64::from(cifrado));
            let mut b = criar(&vfs, cifrado, config.clone());
            let mut historico: Vec<(u64, Modelo)> = vec![(0, Modelo::new())];
            let mut atual = Modelo::new();
            for passo in 0..30 {
                let ctx = format!("{ctx0}, passo {passo}");
                let r = sorteio.ate(20);
                if r < 14 {
                    let mut tx = b.escrever().unwrap();
                    let mut novo = atual.clone();
                    for i in 0..1 + sorteio.ate(4) {
                        let m = marca(&format!("s{semente}p{passo}i{i}"));
                        let op = sorteio.ate(3);
                        if op == 0 || novo.is_empty() {
                            let n = tx.alocar(TipoPagina::NoArvore).unwrap();
                            escrever(&mut tx, n, m);
                            novo.insert(n, m);
                        } else {
                            let k = sorteio.ate(novo.len() as u64) as usize;
                            let n = *novo.keys().nth(k).unwrap();
                            if op == 1 {
                                escrever(&mut tx, n, m);
                                novo.insert(n, m);
                            } else {
                                tx.liberar(n).unwrap();
                                novo.remove(&n);
                            }
                        }
                    }
                    if sorteio.ate(5) == 0 {
                        tx.desfazer();
                    } else {
                        let lsn = tx.confirmar().unwrap();
                        historico.push((lsn, novo.clone()));
                        atual = novo;
                    }
                } else if r < 16 {
                    b.checkpoint().unwrap();
                } else {
                    if r >= 18 {
                        b.checkpoint_pela_metade().unwrap();
                    }
                    let minimo = b.estatisticas().lsn_checkpoint;
                    drop(b);
                    vfs.queda();
                    quedas += 1;
                    b = abrir(&vfs, cifrado, config.clone());
                    atual = conferir_apos_queda(&b, &mut historico, sincronia, minimo, &ctx);
                }
            }
            let minimo = b.estatisticas().lsn_checkpoint;
            drop(b);
            vfs.queda();
            let b = abrir(&vfs, cifrado, config.clone());
            conferir_apos_queda(
                &b,
                &mut historico,
                sincronia,
                minimo,
                &format!("{ctx0}, fim"),
            );
        }
    }
    assert!(quedas > 100, "poucas quedas exercitadas: {quedas}");
}

#[test]
fn copia_a_quente_abre_com_tudo_confirmado() {
    for cifrado in [false, true] {
        let vfs = disco(11);
        let b = criar(&vfs, cifrado, Config::default());
        let mut tx = b.escrever().unwrap();
        let n = tx.alocar(TipoPagina::NoArvore).unwrap();
        escrever(&mut tx, n, marca("antes"));
        tx.confirmar().unwrap();
        // Uma foto antiga aberta segura o checkpoint: a cópia precisa levar o WAL junto.
        let foto = b.ler().unwrap();
        let mut tx = b.escrever().unwrap();
        escrever(&mut tx, n, marca("depois"));
        tx.confirmar().unwrap();
        let destino = Path::new("copia.sdb");
        assert!(b.copiar_para(&vfs, destino).unwrap() > 0);
        assert!(
            b.copiar_para(&vfs, destino).is_err(),
            "não sobrescreve cópia existente"
        );
        drop(foto);
        drop(b);
        let copia = B::abrir(&vfs, destino, cifrado.then_some(SENHA), Config::default()).unwrap();
        assert_eq!(conteudo(&copia, n), marca("depois"));
        assert!(!vfs.existe(Path::new("copia.sdb.parcial")).unwrap());
    }
}

/// Multiplicador das rodadas de simulação (`SIOUX_SIM_FATOR`, usado na rodada noturna do CI).
fn fator_simulacao() -> u64 {
    std::env::var("SIOUX_SIM_FATOR")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
}
