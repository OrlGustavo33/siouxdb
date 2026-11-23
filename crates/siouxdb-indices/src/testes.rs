use crate::ErroIndice;
use crate::arvore::{self, Cursor};
use siouxdb_armazenamento::{CABECALHO_PAGINA, Meta, NumeroPagina, Pagina, TipoPagina};
use siouxdb_transacoes::{ErroBanco, Paginas, PaginasMut, Resultado as ResultadoBanco};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Páginas em memória, para exercitar a árvore com páginas pequenas e muitas operações.
/// Página liberada some: usar depois de liberar vira erro (pega ponteiro pendurado).
struct Mem {
    paginas: HashMap<NumeroPagina, Arc<Pagina>>,
    proxima: NumeroPagina,
    conteudo: usize,
    alocadas: u64,
}

impl Mem {
    fn nova(conteudo: usize) -> Self {
        Mem {
            paginas: HashMap::new(),
            proxima: 2,
            conteudo,
            alocadas: 0,
        }
    }
}

impl Paginas for Mem {
    fn pagina(&self, n: NumeroPagina) -> ResultadoBanco<Arc<Pagina>> {
        self.paginas
            .get(&n)
            .cloned()
            .ok_or(ErroBanco::PaginaInexistente(n))
    }
    fn meta(&self) -> Meta {
        Meta {
            total_paginas: self.proxima,
            ..Meta::default()
        }
    }
    fn tamanho_conteudo(&self) -> usize {
        self.conteudo
    }
}

impl PaginasMut for Mem {
    fn pagina_mut(&mut self, n: NumeroPagina) -> ResultadoBanco<&mut Pagina> {
        self.paginas
            .get_mut(&n)
            .map(Arc::make_mut)
            .ok_or(ErroBanco::PaginaInexistente(n))
    }
    fn alocar(&mut self, tipo: TipoPagina) -> ResultadoBanco<NumeroPagina> {
        let n = self.proxima;
        self.proxima += 1;
        self.alocadas += 1;
        self.paginas.insert(
            n,
            Arc::new(Pagina::nova(self.conteudo + CABECALHO_PAGINA, tipo)),
        );
        Ok(n)
    }
    fn liberar(&mut self, n: NumeroPagina) -> ResultadoBanco<()> {
        self.paginas
            .remove(&n)
            .map(|_| ())
            .ok_or(ErroBanco::PaginaJaLivre(n))
    }
    fn lsn(&self) -> u64 {
        1
    }
    fn definir_raiz_catalogo(&mut self, _: NumeroPagina) {}
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
    fn bytes(&mut self, minimo: u64, maximo: u64, alfabeto: u64) -> Vec<u8> {
        let n = minimo + self.ate(maximo - minimo + 1);
        (0..n).map(|_| b'a' + self.ate(alfabeto) as u8).collect()
    }
}

fn tudo(m: &Mem, raiz: NumeroPagina) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut c = Cursor::novo(m, raiz, None, None).unwrap();
    let mut v = Vec::new();
    while let Some(par) = c.proximo().unwrap() {
        v.push(par);
    }
    v
}

#[test]
fn vazia_uma_entrada_e_limites() {
    let mut m = Mem::nova(256);
    let raiz = arvore::criar(&mut m).unwrap();
    assert_eq!(arvore::obter(&m, raiz, b"x").unwrap(), None);
    assert!(tudo(&m, raiz).is_empty());
    assert_eq!(arvore::remover(&mut m, raiz, b"x").unwrap(), None);
    assert_eq!(arvore::inserir(&mut m, raiz, b"x", b"1").unwrap(), None);
    assert_eq!(
        arvore::inserir(&mut m, raiz, b"x", b"2").unwrap(),
        Some(b"1".to_vec())
    );
    assert_eq!(
        arvore::inserir(&mut m, raiz, b"x", b"maior").unwrap(),
        Some(b"2".to_vec())
    );
    assert_eq!(
        arvore::obter(&m, raiz, b"x").unwrap(),
        Some(b"maior".to_vec())
    );
    assert_eq!(
        arvore::inserir(&mut m, raiz, b"", b"chave vazia").unwrap(),
        None
    );
    assert_eq!(tudo(&m, raiz).len(), 2);

    let maximo = arvore::maximo_entrada(256);
    assert!(arvore::inserir(&mut m, raiz, &vec![b'k'; maximo - 3], b"abc").is_ok());
    assert!(matches!(
        arvore::inserir(&mut m, raiz, &vec![b'k'; maximo - 2], b"abc"),
        Err(ErroIndice::EntradaGrandeDemais { .. })
    ));
    arvore::verificar(&m, raiz).unwrap();
}

#[test]
fn modelo_aleatorio_contra_btreemap() {
    for (semente, conteudo, alfabeto) in [
        (1u64, 256usize, 4u64),
        (2, 256, 26),
        (3, 512, 3),
        (4, 1024, 26),
        (5, 256, 2),
    ] {
        let mut s = Sorteio(semente);
        let mut m = Mem::nova(conteudo);
        let raiz = arvore::criar(&mut m).unwrap();
        let mut modelo: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        let maximo = arvore::maximo_entrada(conteudo) as u64;
        let mut maior_altura = 0;
        for passo in 0..6000 {
            let ctx = format!("semente {semente}, passo {passo}");
            // Fases: cresce, mistura, esvazia quase tudo, cresce de novo.
            let fase = passo / 1500;
            let r = s.ate(10);
            let remover = match fase {
                0 | 3 => r < 2,
                1 => r < 5,
                _ => r < 8,
            };
            if remover && !modelo.is_empty() {
                let k = if s.ate(4) == 0 {
                    s.bytes(0, 12, alfabeto)
                } else {
                    let i = s.ate(modelo.len() as u64) as usize;
                    modelo.keys().nth(i).unwrap().clone()
                };
                assert_eq!(
                    arvore::remover(&mut m, raiz, &k).unwrap(),
                    modelo.remove(&k),
                    "{ctx}"
                );
            } else {
                let k = s.bytes(0, (maximo / 2).min(14), alfabeto);
                let v = s.bytes(0, maximo - k.len() as u64, 26);
                assert_eq!(
                    arvore::inserir(&mut m, raiz, &k, &v).unwrap(),
                    modelo.insert(k, v),
                    "{ctx}"
                );
            }
            if passo % 250 == 0 || passo == 5999 {
                let resumo = arvore::verificar(&m, raiz).unwrap_or_else(|e| panic!("{ctx}: {e}"));
                maior_altura = maior_altura.max(resumo.altura);
                assert_eq!(resumo.entradas as usize, modelo.len(), "{ctx}");
                assert_eq!(
                    resumo.paginas as usize,
                    m.paginas.len(),
                    "{ctx}: página vazada ou perdida"
                );
                let esperado: Vec<_> = modelo.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                assert_eq!(tudo(&m, raiz), esperado, "{ctx}");
                for _ in 0..20 {
                    let k = s.bytes(0, 6, alfabeto);
                    assert_eq!(
                        arvore::obter(&m, raiz, &k).unwrap().as_ref(),
                        modelo.get(&k),
                        "{ctx}"
                    );
                    // Faixa [a, b)
                    let (a, b) = (s.bytes(0, 4, alfabeto), s.bytes(0, 4, alfabeto));
                    let (a, b) = if a <= b { (a, b) } else { (b, a) };
                    let mut c = Cursor::novo(&m, raiz, Some(&a), Some(&b)).unwrap();
                    let mut vistos = Vec::new();
                    while let Some((k, _)) = c.proximo().unwrap() {
                        vistos.push(k);
                    }
                    let certos: Vec<_> = modelo
                        .range(a.clone()..b.clone())
                        .map(|(k, _)| k.clone())
                        .collect();
                    assert_eq!(vistos, certos, "{ctx}: faixa {a:?}..{b:?}");
                }
            }
        }
        assert!(
            maior_altura >= 3,
            "semente {semente}: o teste precisa de árvore com vários níveis ({maior_altura})"
        );
        // Remove tudo: sobra só a raiz, vazia.
        for k in modelo.keys() {
            assert!(arvore::remover(&mut m, raiz, k).unwrap().is_some());
        }
        let resumo = arvore::verificar(&m, raiz).unwrap();
        assert_eq!((resumo.entradas, resumo.paginas, resumo.altura), (0, 1, 1));
        assert_eq!(
            m.paginas.len(),
            1,
            "semente {semente}: páginas não devolvidas"
        );
    }
}

#[test]
fn chaves_em_ordem_enchem_as_folhas() {
    let mut m = Mem::nova(4072);
    let raiz = arvore::criar(&mut m).unwrap();
    for i in 0..50_000u64 {
        arvore::inserir(&mut m, raiz, &i.to_be_bytes(), &(i * 2).to_le_bytes()).unwrap();
    }
    let r = arvore::verificar(&m, raiz).unwrap();
    assert_eq!(r.entradas, 50_000);
    let ocupacao = r.bytes_nas_folhas as f64 / (r.folhas as f64 * (4072.0 - 16.0));
    assert!(ocupacao > 0.95, "folhas {ocupacao:.2} cheias: {r:?}");
    assert!(r.altura <= 3, "{r:?}");
    assert_eq!(
        arvore::obter(&m, raiz, &31_337u64.to_be_bytes()).unwrap(),
        Some(62_674u64.to_le_bytes().to_vec())
    );
    // Faixa no meio.
    let mut c = Cursor::novo(
        &m,
        raiz,
        Some(&1000u64.to_be_bytes()),
        Some(&1010u64.to_be_bytes()),
    )
    .unwrap();
    let mut n = 0;
    while let Some(k) = c
        .proximo_com(|k, _| u64::from_be_bytes(k.try_into().unwrap()))
        .unwrap()
    {
        assert_eq!(k, 1000 + n);
        n += 1;
    }
    assert_eq!(n, 10);
}

#[test]
fn separadora_curta_mantem_os_nos_de_cima_pequenos() {
    let mut m = Mem::nova(1024);
    let raiz = arvore::criar(&mut m).unwrap();
    // Chaves longas com prefixos bem diferentes: o que sobe é só o começo.
    for i in 0..3000u32 {
        let k = format!(
            "{:05}-cliente-com-um-nome-bem-comprido-{i}",
            i.wrapping_mul(7919) % 100_000
        );
        arvore::inserir(&mut m, raiz, k.as_bytes(), b"v").unwrap();
    }
    let r = arvore::verificar(&m, raiz).unwrap();
    let internos = r.paginas - r.folhas;
    assert!(internos * 12 <= r.folhas, "nós internos demais: {r:?}");
}

#[test]
fn liberar_devolve_todas_as_paginas() {
    let mut m = Mem::nova(256);
    let raiz = arvore::criar(&mut m).unwrap();
    for i in 0..2000u32 {
        arvore::inserir(&mut m, raiz, &i.to_be_bytes(), b"valor").unwrap();
    }
    let paginas = arvore::verificar(&m, raiz).unwrap().paginas;
    assert!(paginas > 50);
    assert_eq!(arvore::liberar(&mut m, raiz).unwrap(), paginas);
    assert!(m.paginas.is_empty());
}

#[test]
fn pagina_que_nao_e_no_da_erro_e_nao_panico() {
    let mut m = Mem::nova(256);
    let outra = m.alocar(TipoPagina::LogQuente).unwrap();
    assert!(matches!(
        arvore::obter(&m, outra, b"x"),
        Err(ErroIndice::Corrompido { .. })
    ));
    assert!(matches!(
        arvore::inserir(&mut m, outra, b"x", b"y"),
        Err(ErroIndice::Corrompido { .. })
    ));
    // Nó interno apontando para si mesmo: erro, não laço infinito.
    let raiz = arvore::criar(&mut m).unwrap();
    let b = m.pagina_mut(raiz).unwrap().conteudo_mut();
    b[0] = 1;
    b[8..16].copy_from_slice(&raiz.to_le_bytes());
    assert!(matches!(
        arvore::obter(&m, raiz, b"x"),
        Err(ErroIndice::Corrompido { .. })
    ));
    assert!(arvore::verificar(&m, raiz).is_err());
    assert!(Cursor::novo(&m, raiz, None, None).is_err());
}
