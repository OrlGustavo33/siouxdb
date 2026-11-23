use crate::catalogo::{self, criar_tabela};
use crate::{Coluna, ErroCatalogo, Tabela, Tipo, Valor};
use siouxdb_armazenamento::{ParametrosSenha, SenhaNova, TamanhoPagina};
use siouxdb_transacoes::{Banco, Config, Sincronia};
use siouxdb_vfs::{ConfigSimulacao, Vfs, VfsSimulado};
use std::collections::BTreeMap;
use std::path::PathBuf;

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

fn clientes() -> Vec<Coluna> {
    vec![
        Coluna::nova("id", Tipo::Inteiro),
        Coluna::obrigatoria("nome", Tipo::Texto),
        Coluna::nova("saldo", Tipo::Real),
    ]
}

fn linha(id: i64, nome: &str, saldo: f64) -> Vec<Valor> {
    vec![id.into(), nome.into(), saldo.into()]
}

#[test]
fn tabela_basica_historico_e_como_estava() {
    let vfs = disco(1);
    let b = criar(&vfs, true, Config::default());
    let mut tx = b.escrever().unwrap();
    let t = criar_tabela(&mut tx, "Clientes", &clientes(), &["id"]).unwrap();
    assert_eq!(t.def().nome, "clientes");
    assert!(
        t.def().colunas[0].nao_nulo,
        "coluna da chave vira obrigatória"
    );
    t.inserir(&mut tx, &linha(2, "BIA", 10.0)).unwrap();
    t.inserir(&mut tx, &linha(-5, "ANA", 1.5)).unwrap();
    t.inserir(&mut tx, &linha(10, "CAIO", 0.0)).unwrap();
    assert!(matches!(
        t.inserir(&mut tx, &linha(2, "OUTRA", 0.0)),
        Err(ErroCatalogo::ChaveDuplicada)
    ));
    // Validação da linha.
    assert!(matches!(
        t.inserir(&mut tx, &[1.into(), "X".into()]),
        Err(ErroCatalogo::LinhaInvalida(_))
    ));
    assert!(matches!(
        t.inserir(&mut tx, &[Valor::Nulo, "X".into(), 1.0.into()]),
        Err(ErroCatalogo::LinhaInvalida(_))
    ));
    assert!(matches!(
        t.inserir(&mut tx, &[1.into(), Valor::Nulo, 1.0.into()]),
        Err(ErroCatalogo::LinhaInvalida(_))
    ));
    assert!(matches!(
        t.inserir(&mut tx, &["1".into(), "X".into(), 1.0.into()]),
        Err(ErroCatalogo::LinhaInvalida(_))
    ));
    t.inserir(&mut tx, &[7.into(), "SEM SALDO".into(), Valor::Nulo])
        .unwrap();
    // A transação já enxerga o que ela mesma gravou.
    assert_eq!(
        t.obter(&tx, &[2.into()]).unwrap(),
        Some(linha(2, "BIA", 10.0))
    );
    let lsn1 = tx.confirmar().unwrap();

    let mut tx = b.escrever().unwrap();
    assert!(t.gravar(&mut tx, &linha(2, "BIA", 99.0)).unwrap());
    assert!(!t.gravar(&mut tx, &linha(3, "DUDA", 5.0)).unwrap());
    assert!(t.remover(&mut tx, &[10.into()]).unwrap());
    assert!(!t.remover(&mut tx, &[10.into()]).unwrap());
    assert!(!t.remover(&mut tx, &[999.into()]).unwrap());
    let lsn2 = tx.confirmar().unwrap();

    let l = b.ler().unwrap();
    let t = catalogo::tabela(&l, "CLIENTES").unwrap().unwrap();
    // Ordem da chave: negativos antes.
    let ids: Vec<Valor> = t
        .varrer(&l, None, None)
        .unwrap()
        .todas()
        .unwrap()
        .into_iter()
        .map(|l| l[0].clone())
        .collect();
    assert_eq!(ids, vec![(-5).into(), 2.into(), 3.into(), 7.into()]);
    assert_eq!(
        t.obter(&l, &[2.into()]).unwrap(),
        Some(linha(2, "BIA", 99.0))
    );
    assert_eq!(t.obter(&l, &[10.into()]).unwrap(), None);
    let c = t.contagem(&l).unwrap();
    assert_eq!((c.linhas, c.versoes), (4, 7));
    // Faixa [0, 7)
    let faixa = t
        .varrer(&l, Some(&[0.into()]), Some(&[7.into()]))
        .unwrap()
        .todas()
        .unwrap();
    assert_eq!(faixa.len(), 2);

    // Como estava depois da primeira transação.
    assert_eq!(
        t.obter_em(&l, &[2.into()], lsn1).unwrap(),
        Some(linha(2, "BIA", 10.0))
    );
    assert_eq!(
        t.obter_em(&l, &[10.into()], lsn1).unwrap(),
        Some(linha(10, "CAIO", 0.0))
    );
    assert_eq!(t.obter_em(&l, &[3.into()], lsn1).unwrap(), None);
    assert_eq!(t.obter_em(&l, &[2.into()], 0).unwrap(), None);
    let antes: Vec<Valor> = t
        .varrer_em(&l, None, None, lsn1)
        .unwrap()
        .todas()
        .unwrap()
        .into_iter()
        .map(|l| l[0].clone())
        .collect();
    assert_eq!(antes, vec![(-5).into(), 2.into(), 7.into(), 10.into()]);

    let h = t.historico(&l, &[10.into()]).unwrap();
    assert_eq!(h.len(), 2);
    assert_eq!((h[0].lsn, h[0].linha.clone()), (lsn2, None));
    assert_eq!(
        (h[1].lsn, h[1].linha.clone()),
        (lsn1, Some(linha(10, "CAIO", 0.0)))
    );
    assert!(h[0].instante >= h[1].instante && h[1].instante > 1_600_000_000_000_000);
    assert!(t.historico(&l, &[12345.into()]).unwrap().is_empty());
}

#[test]
fn chave_composta_e_faixa_por_prefixo() {
    let vfs = disco(2);
    let b = criar(&vfs, false, Config::default());
    let mut tx = b.escrever().unwrap();
    let cols = [
        Coluna::nova("uf", Tipo::Texto),
        Coluna::nova("cidade", Tipo::Texto),
        Coluna::nova("populacao", Tipo::Inteiro),
    ];
    let t = criar_tabela(&mut tx, "cidades", &cols, &["uf", "cidade"]).unwrap();
    for (uf, cidade, pop) in [
        ("SP", "SANTOS", 430),
        ("PR", "CURITIBA", 1900),
        ("SP", "CAMPINAS", 1200),
        ("S", "X", 1),
        ("SPA", "Y", 2),
    ] {
        t.inserir(&mut tx, &[uf.into(), cidade.into(), (pop as i64).into()])
            .unwrap();
    }
    tx.confirmar().unwrap();
    let l = b.ler().unwrap();
    // Só as de SP: de ("SP") até ("SP", fim). "S" e "SPA" não entram.
    let sp = t
        .varrer(&l, Some(&["SP".into()]), Some(&["SP\u{1}".into()]))
        .unwrap()
        .todas()
        .unwrap();
    let nomes: Vec<_> = sp.iter().map(|l| l[1].clone()).collect();
    assert_eq!(nomes, vec![Valor::from("CAMPINAS"), Valor::from("SANTOS")]);
    assert_eq!(
        t.obter(&l, &["PR".into(), "CURITIBA".into()])
            .unwrap()
            .unwrap()[2],
        Valor::Inteiro(1900)
    );
    assert!(t.obter(&l, &["PR".into()]).is_err(), "chave incompleta");
}

#[test]
fn catalogo_regras_listagem_e_visoes() {
    let vfs = disco(3);
    let b = criar(&vfs, false, Config::default());
    assert!(catalogo::tabelas(&b.ler().unwrap()).unwrap().is_empty());
    assert!(
        catalogo::tabela(&b.ler().unwrap(), "nada")
            .unwrap()
            .is_none()
    );

    let mut tx = b.escrever().unwrap();
    for ruim in [
        "",
        "1abc",
        "com espaço",
        "ação",
        "sys_minha",
        &"x".repeat(64),
    ] {
        assert!(
            matches!(
                criar_tabela(&mut tx, ruim, &clientes(), &["id"]),
                Err(ErroCatalogo::NomeInvalido(_))
            ),
            "{ruim}"
        );
    }
    assert!(matches!(
        criar_tabela(&mut tx, "t", &[], &["id"]),
        Err(ErroCatalogo::DefinicaoInvalida(_))
    ));
    assert!(matches!(
        criar_tabela(&mut tx, "t", &clientes(), &[]),
        Err(ErroCatalogo::DefinicaoInvalida(_))
    ));
    assert!(matches!(
        criar_tabela(&mut tx, "t", &clientes(), &["nao_tem"]),
        Err(ErroCatalogo::DefinicaoInvalida(_))
    ));
    assert!(matches!(
        criar_tabela(&mut tx, "t", &clientes(), &["id", "ID"]),
        Err(ErroCatalogo::DefinicaoInvalida(_))
    ));
    let repetida = [
        Coluna::nova("a", Tipo::Inteiro),
        Coluna::nova("A", Tipo::Texto),
    ];
    assert!(matches!(
        criar_tabela(&mut tx, "t", &repetida, &["a"]),
        Err(ErroCatalogo::DefinicaoInvalida(_))
    ));

    let produtos = criar_tabela(
        &mut tx,
        "produtos",
        &[
            Coluna::nova("codigo", Tipo::Texto),
            Coluna::nova("ativo", Tipo::Booleano),
        ],
        &["codigo"],
    )
    .unwrap();
    let cli = criar_tabela(&mut tx, "clientes", &clientes(), &["id"]).unwrap();
    assert!(matches!(
        criar_tabela(&mut tx, "CLIENTES", &clientes(), &["id"]),
        Err(ErroCatalogo::TabelaJaExiste(_))
    ));
    assert_eq!((produtos.def().id, cli.def().id), (1, 2));
    cli.inserir(&mut tx, &linha(1, "ANA", 0.0)).unwrap();
    cli.gravar(&mut tx, &linha(1, "ANA", 5.0)).unwrap();
    tx.confirmar().unwrap();

    let l = b.ler().unwrap();
    let nomes: Vec<_> = catalogo::tabelas(&l)
        .unwrap()
        .iter()
        .map(|t| t.def().nome.clone())
        .collect();
    assert_eq!(nomes, ["clientes", "produtos"]);
    let st = catalogo::sys_tabelas(&l).unwrap();
    assert_eq!(
        st[0],
        vec!["clientes".into(), 2.into(), 3.into(), 1.into(), 2.into()]
    );
    assert_eq!(st[0].len(), catalogo::colunas_sys_tabelas().len());
    let sc = catalogo::sys_colunas(&l).unwrap();
    assert_eq!(sc.len(), 5);
    assert_eq!(
        sc[0],
        vec![
            "clientes".into(),
            1.into(),
            "id".into(),
            "INTEIRO".into(),
            true.into(),
            1.into()
        ]
    );
    assert_eq!(sc[2][5], Valor::Nulo);
    assert_eq!(sc[0].len(), catalogo::colunas_sys_colunas().len());
    drop(l);

    // Criar tabela e desfazer não deixa rastro; o id não é gasto.
    let mut tx = b.escrever().unwrap();
    criar_tabela(&mut tx, "temporaria", &clientes(), &["id"]).unwrap();
    tx.desfazer();
    assert!(
        catalogo::tabela(&b.ler().unwrap(), "temporaria")
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        catalogo::tabela_obrigatoria(&b.ler().unwrap(), "temporaria"),
        Err(ErroCatalogo::TabelaNaoExiste(_))
    ));
}

#[test]
fn apagar_tabela_devolve_as_paginas() {
    let vfs = disco(4);
    let b = criar(&vfs, false, Config::default());
    let encher = |nome: &str| {
        let mut tx = b.escrever().unwrap();
        let t = criar_tabela(&mut tx, nome, &clientes(), &["id"]).unwrap();
        for i in 0..3000 {
            t.inserir(&mut tx, &linha(i, "CLIENTE COM NOME RAZOAVEL", i as f64))
                .unwrap();
        }
        tx.confirmar().unwrap();
    };
    encher("a");
    let total = b.ler().unwrap().meta().total_paginas;
    assert!(total > 40);
    let mut tx = b.escrever().unwrap();
    catalogo::remover_tabela(&mut tx, "A").unwrap();
    assert!(matches!(
        catalogo::remover_tabela(&mut tx, "a"),
        Err(ErroCatalogo::TabelaNaoExiste(_))
    ));
    tx.confirmar().unwrap();
    encher("b");
    assert_eq!(
        b.ler().unwrap().meta().total_paginas,
        total,
        "as páginas liberadas foram reaproveitadas"
    );
    let l = b.ler().unwrap();
    let t = catalogo::tabela(&l, "b").unwrap().unwrap();
    assert_eq!(t.contagem(&l).unwrap().linhas, 3000);
    assert_eq!(t.verificar_indice(&l).unwrap().entradas, 3000);
}

#[test]
fn foto_nao_ve_gravacoes_novas_e_linha_grande_demais() {
    let vfs = disco(5);
    let b = criar(&vfs, true, Config::default());
    let mut tx = b.escrever().unwrap();
    let t = criar_tabela(
        &mut tx,
        "notas",
        &[
            Coluna::nova("id", Tipo::Inteiro),
            Coluna::nova("texto", Tipo::Texto),
        ],
        &["id"],
    )
    .unwrap();
    t.inserir(&mut tx, &[1.into(), "primeira".into()]).unwrap();
    assert!(matches!(
        t.inserir(&mut tx, &[2.into(), "x".repeat(5000).into()]),
        Err(ErroCatalogo::LinhaGrandeDemais { .. })
    ));
    // Linha quase do tamanho da página cabe.
    t.inserir(&mut tx, &[3.into(), "y".repeat(3900).into()])
        .unwrap();
    tx.confirmar().unwrap();
    let foto = b.ler().unwrap();
    let mut tx = b.escrever().unwrap();
    t.gravar(&mut tx, &[1.into(), "alterada".into()]).unwrap();
    t.inserir(&mut tx, &[4.into(), "nova".into()]).unwrap();
    tx.confirmar().unwrap();
    assert_eq!(
        t.obter(&foto, &[1.into()]).unwrap().unwrap()[1],
        Valor::from("primeira")
    );
    assert_eq!(
        t.varrer(&foto, None, None).unwrap().todas().unwrap().len(),
        2
    );
    assert_eq!(
        t.varrer(&b.ler().unwrap(), None, None)
            .unwrap()
            .todas()
            .unwrap()
            .len(),
        3
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

/// Estado esperado: para cada tabela existente, (id da tabela, chave → nome).
type Modelo = BTreeMap<String, (u64, BTreeMap<i64, String>)>;

fn conferir(b: &B, historico: &mut Vec<(u64, Modelo)>, sincronia: Sincronia, ctx: &str) -> Modelo {
    let l = b.ler().unwrap();
    let lsn = l.lsn();
    if sincronia == Sincronia::Total {
        assert_eq!(
            lsn,
            historico.last().unwrap().0,
            "{ctx}: confirmação perdida"
        );
    }
    let i = historico
        .iter()
        .position(|(h, _)| *h == lsn)
        .unwrap_or_else(|| panic!("{ctx}: LSN {lsn} fora do histórico"));
    historico.truncate(i + 1);
    let modelo = historico[i].1.clone();
    let existentes: Vec<String> = catalogo::tabelas(&l)
        .unwrap()
        .iter()
        .map(|t| t.def().nome.clone())
        .collect();
    assert_eq!(
        existentes,
        modelo.keys().cloned().collect::<Vec<_>>(),
        "{ctx}"
    );
    for (nome, (id, linhas)) in &modelo {
        let t = catalogo::tabela(&l, nome).unwrap().unwrap();
        assert_eq!(t.def().id, *id, "{ctx}: id de {nome}");
        let pares = |linhas: Vec<Vec<Valor>>| -> Vec<(Valor, Valor)> {
            linhas
                .into_iter()
                .map(|l| (l[0].clone(), l[1].clone()))
                .collect()
        };
        let esperar = |m: &BTreeMap<i64, String>| -> Vec<(Valor, Valor)> {
            m.iter()
                .map(|(k, v)| ((*k).into(), v.as_str().into()))
                .collect()
        };
        assert_eq!(
            pares(t.varrer(&l, None, None).unwrap().todas().unwrap()),
            esperar(linhas),
            "{ctx}: tabela {nome}"
        );
        assert_eq!(
            t.contagem(&l).unwrap().linhas as usize,
            linhas.len(),
            "{ctx}: contagem de {nome}"
        );
        t.verificar_indice(&l)
            .unwrap_or_else(|e| panic!("{ctx}: índice de {nome}: {e}"));
        // O passado também confere: enquanto a tabela é a mesma (mesmo id), cada estado
        // anterior continua legível exatamente como era.
        let mut conferidos = 0;
        for (lsn_antigo, antigo) in historico.iter().rev().skip(1) {
            match antigo.get(nome) {
                Some((id_antigo, linhas_antigas)) if id_antigo == id => {
                    let como_estava = pares(
                        t.varrer_em(&l, None, None, *lsn_antigo)
                            .unwrap()
                            .todas()
                            .unwrap(),
                    );
                    assert_eq!(
                        como_estava,
                        esperar(linhas_antigas),
                        "{ctx}: {nome} como estava no LSN {lsn_antigo}"
                    );
                    conferidos += 1;
                    if conferidos == 4 {
                        break;
                    }
                }
                _ => {
                    // Antes de a tabela existir: vazia.
                    assert!(
                        t.varrer_em(&l, None, None, *lsn_antigo)
                            .unwrap()
                            .todas()
                            .unwrap()
                            .is_empty(),
                        "{ctx}"
                    );
                    break;
                }
            }
        }
    }
    modelo
}

#[test]
fn modelo_de_tabelas_com_quedas() {
    let modos = [
        (false, Sincronia::Total),
        (true, Sincronia::Total),
        (false, Sincronia::Normal),
        (true, Sincronia::Normal),
    ];
    let nomes = ["alfa", "beta", "gama"];
    let cols = [
        Coluna::nova("id", Tipo::Inteiro),
        Coluna::nova("nome", Tipo::Texto),
    ];
    let mut quedas = 0;
    for semente in 0..25u64 {
        for (cifrado, sincronia) in modos {
            let ctx0 = format!("semente {semente}, cifrado {cifrado}, {sincronia:?}");
            let vfs = disco(semente);
            let config = Config {
                sincronia,
                checkpoint_apos_paginas: 15,
                ..Config::default()
            };
            let mut s = Sorteio(semente * 31 + u64::from(cifrado));
            let mut b = criar(&vfs, cifrado, config.clone());
            let mut historico: Vec<(u64, Modelo)> = vec![(0, Modelo::new())];
            let mut atual = Modelo::new();
            for passo in 0..40 {
                let ctx = format!("{ctx0}, passo {passo}");
                let r = s.ate(20);
                if r < 15 {
                    let mut tx = b.escrever().unwrap();
                    let mut novo = atual.clone();
                    for _ in 0..1 + s.ate(3) {
                        let nome = nomes[s.ate(3) as usize];
                        match (novo.contains_key(nome), s.ate(12)) {
                            (false, _) => {
                                let t = criar_tabela(&mut tx, nome, &cols, &["id"]).unwrap();
                                novo.insert(nome.to_string(), (t.def().id, BTreeMap::new()));
                            }
                            (true, 0) => {
                                catalogo::remover_tabela(&mut tx, nome).unwrap();
                                novo.remove(nome);
                            }
                            (true, _) => {
                                let t: Tabela = catalogo::tabela(&tx, nome).unwrap().unwrap();
                                let linhas = &mut novo.get_mut(nome).unwrap().1;
                                for i in 0..1 + s.ate(40) {
                                    let id = s.ate(300) as i64 - 150;
                                    if s.ate(4) == 0 {
                                        assert_eq!(
                                            t.remover(&mut tx, &[id.into()]).unwrap(),
                                            linhas.remove(&id).is_some(),
                                            "{ctx}"
                                        );
                                    } else {
                                        let nome_linha = format!(
                                            "s{semente}p{passo}i{i}-{}",
                                            "x".repeat(s.ate(120) as usize)
                                        );
                                        let existia = t
                                            .gravar(
                                                &mut tx,
                                                &[id.into(), nome_linha.as_str().into()],
                                            )
                                            .unwrap();
                                        assert_eq!(
                                            existia,
                                            linhas.insert(id, nome_linha).is_some(),
                                            "{ctx}"
                                        );
                                    }
                                }
                            }
                        }
                    }
                    if s.ate(6) == 0 {
                        tx.desfazer();
                    } else {
                        let lsn = tx.confirmar().unwrap();
                        historico.push((lsn, novo.clone()));
                        atual = novo;
                    }
                } else if r < 17 {
                    b.checkpoint().unwrap();
                } else {
                    drop(b);
                    vfs.queda();
                    quedas += 1;
                    b = abrir(&vfs, cifrado, config.clone());
                    atual = conferir(&b, &mut historico, sincronia, &ctx);
                }
            }
            drop(b);
            vfs.queda();
            let b = abrir(&vfs, cifrado, config.clone());
            conferir(&b, &mut historico, sincronia, &format!("{ctx0}, fim"));
        }
    }
    assert!(quedas > 100, "poucas quedas: {quedas}");
}
