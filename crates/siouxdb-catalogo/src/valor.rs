//! Valores, tipos e as duas codificações:
//!
//! - **de chave**: a ordem dos bytes é a ordem dos valores (inteiros negativos antes dos
//!   positivos, textos em ordem, chaves compostas coluna a coluna). É o que entra na árvore;
//! - **de linha**: compacta, sem ordem, para guardar no log.

use crate::{ErroCatalogo, Resultado};
use std::cmp::Ordering;

/// Tipo de uma coluna.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tipo {
    /// Inteiro de 64 bits com sinal.
    Inteiro,
    /// Número de ponto flutuante de 64 bits.
    Real,
    /// Texto UTF-8.
    Texto,
    /// Bytes.
    Bytes,
    /// Verdadeiro ou falso.
    Booleano,
}

impl Tipo {
    /// Nome usado no SQL e nas tabelas do sistema.
    pub fn nome(self) -> &'static str {
        match self {
            Tipo::Inteiro => "INTEIRO",
            Tipo::Real => "REAL",
            Tipo::Texto => "TEXTO",
            Tipo::Bytes => "BYTES",
            Tipo::Booleano => "BOOLEANO",
        }
    }

    pub(crate) fn codigo(self) -> i64 {
        match self {
            Tipo::Inteiro => 1,
            Tipo::Real => 2,
            Tipo::Texto => 3,
            Tipo::Bytes => 4,
            Tipo::Booleano => 5,
        }
    }

    pub(crate) fn de_codigo(c: i64) -> Option<Tipo> {
        Some(match c {
            1 => Tipo::Inteiro,
            2 => Tipo::Real,
            3 => Tipo::Texto,
            4 => Tipo::Bytes,
            5 => Tipo::Booleano,
            _ => return None,
        })
    }
}

/// Um valor de uma coluna.
#[derive(Debug, Clone, PartialEq)]
pub enum Valor {
    /// Ausência de valor.
    Nulo,
    /// Inteiro.
    Inteiro(i64),
    /// Real.
    Real(f64),
    /// Texto.
    Texto(String),
    /// Bytes.
    Bytes(Vec<u8>),
    /// Booleano.
    Booleano(bool),
}

impl Valor {
    /// Tipo do valor (`None` para nulo).
    pub fn tipo(&self) -> Option<Tipo> {
        Some(match self {
            Valor::Nulo => return None,
            Valor::Inteiro(_) => Tipo::Inteiro,
            Valor::Real(_) => Tipo::Real,
            Valor::Texto(_) => Tipo::Texto,
            Valor::Bytes(_) => Tipo::Bytes,
            Valor::Booleano(_) => Tipo::Booleano,
        })
    }

    /// Ordem entre valores (nulo primeiro; tipos diferentes pela ordem dos tipos). É a
    /// mesma ordem dos bytes de [`codificar_chave`].
    pub fn comparar(&self, outro: &Valor) -> Ordering {
        fn posto(v: &Valor) -> u8 {
            match v {
                Valor::Nulo => 0,
                Valor::Booleano(_) => 1,
                Valor::Inteiro(_) => 2,
                Valor::Real(_) => 3,
                Valor::Texto(_) => 4,
                Valor::Bytes(_) => 5,
            }
        }
        match (self, outro) {
            (Valor::Booleano(a), Valor::Booleano(b)) => a.cmp(b),
            (Valor::Inteiro(a), Valor::Inteiro(b)) => a.cmp(b),
            (Valor::Real(a), Valor::Real(b)) => a.total_cmp(b),
            (Valor::Texto(a), Valor::Texto(b)) => a.as_bytes().cmp(b.as_bytes()),
            (Valor::Bytes(a), Valor::Bytes(b)) => a.cmp(b),
            (a, b) => posto(a).cmp(&posto(b)),
        }
    }
}

impl From<i64> for Valor {
    fn from(v: i64) -> Self {
        Valor::Inteiro(v)
    }
}
impl From<f64> for Valor {
    fn from(v: f64) -> Self {
        Valor::Real(v)
    }
}
impl From<bool> for Valor {
    fn from(v: bool) -> Self {
        Valor::Booleano(v)
    }
}
impl From<&str> for Valor {
    fn from(v: &str) -> Self {
        Valor::Texto(v.to_string())
    }
}
impl From<String> for Valor {
    fn from(v: String) -> Self {
        Valor::Texto(v)
    }
}
impl From<Vec<u8>> for Valor {
    fn from(v: Vec<u8>) -> Self {
        Valor::Bytes(v)
    }
}

fn bytes_escapados(saida: &mut Vec<u8>, b: &[u8]) {
    // 0x00 vira 0x00 0xFF e o fim é 0x00 0x00: um texto que é começo de outro vem antes,
    // e a coluna seguinte de uma chave composta não se mistura com esta.
    for &x in b {
        saida.push(x);
        if x == 0 {
            saida.push(0xFF);
        }
    }
    saida.extend_from_slice(&[0, 0]);
}

/// Codifica valores como chave: a ordem dos bytes é a ordem dos valores, coluna a coluna.
pub fn codificar_chave(valores: &[Valor]) -> Resultado<Vec<u8>> {
    let mut s = Vec::with_capacity(valores.len() * 9);
    for v in valores {
        match v {
            Valor::Nulo => s.push(0),
            Valor::Booleano(b) => s.extend_from_slice(&[1, u8::from(*b)]),
            Valor::Inteiro(i) => {
                s.push(2);
                s.extend_from_slice(&((*i as u64) ^ (1 << 63)).to_be_bytes());
            }
            Valor::Real(r) => {
                if r.is_nan() {
                    return Err(ErroCatalogo::LinhaInvalida(
                        "NaN não pode fazer parte de uma chave".into(),
                    ));
                }
                let bits = if *r == 0.0 { 0.0f64 } else { *r }.to_bits();
                let ordenavel = if bits >> 63 == 1 {
                    !bits
                } else {
                    bits ^ (1 << 63)
                };
                s.push(3);
                s.extend_from_slice(&ordenavel.to_be_bytes());
            }
            Valor::Texto(t) => {
                s.push(4);
                bytes_escapados(&mut s, t.as_bytes());
            }
            Valor::Bytes(b) => {
                s.push(5);
                bytes_escapados(&mut s, b);
            }
        }
    }
    Ok(s)
}

/// Codifica uma linha para o log.
pub fn codificar_linha(valores: &[Valor]) -> Vec<u8> {
    let mut s = Vec::with_capacity(2 + valores.len() * 9);
    s.extend_from_slice(&(valores.len() as u16).to_le_bytes());
    for v in valores {
        match v {
            Valor::Nulo => s.push(0),
            Valor::Inteiro(i) => {
                s.push(1);
                s.extend_from_slice(&i.to_le_bytes());
            }
            Valor::Real(r) => {
                s.push(2);
                s.extend_from_slice(&r.to_bits().to_le_bytes());
            }
            Valor::Texto(t) => {
                s.push(3);
                s.extend_from_slice(&(t.len() as u32).to_le_bytes());
                s.extend_from_slice(t.as_bytes());
            }
            Valor::Bytes(b) => {
                s.push(4);
                s.extend_from_slice(&(b.len() as u32).to_le_bytes());
                s.extend_from_slice(b);
            }
            Valor::Booleano(b) => s.extend_from_slice(&[5, u8::from(*b)]),
        }
    }
    s
}

/// Lê uma linha de [`codificar_linha`].
pub fn decodificar_linha(mut b: &[u8]) -> Resultado<Vec<Valor>> {
    const RUIM: ErroCatalogo = ErroCatalogo::Corrompido("linha gravada com forma inesperada");
    fn tirar<'a>(b: &mut &'a [u8], n: usize) -> Resultado<&'a [u8]> {
        if b.len() < n {
            return Err(RUIM);
        }
        let (a, resto) = b.split_at(n);
        *b = resto;
        Ok(a)
    }
    let n = u16::from_le_bytes(tirar(&mut b, 2)?.try_into().expect("2")) as usize;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        v.push(match tirar(&mut b, 1)?[0] {
            0 => Valor::Nulo,
            1 => Valor::Inteiro(i64::from_le_bytes(tirar(&mut b, 8)?.try_into().expect("8"))),
            2 => Valor::Real(f64::from_bits(u64::from_le_bytes(
                tirar(&mut b, 8)?.try_into().expect("8"),
            ))),
            t @ (3 | 4) => {
                let tam = u32::from_le_bytes(tirar(&mut b, 4)?.try_into().expect("4")) as usize;
                let dados = tirar(&mut b, tam)?.to_vec();
                if t == 3 {
                    Valor::Texto(String::from_utf8(dados).map_err(|_| RUIM)?)
                } else {
                    Valor::Bytes(dados)
                }
            }
            5 => Valor::Booleano(tirar(&mut b, 1)?[0] != 0),
            _ => return Err(RUIM),
        });
    }
    if !b.is_empty() {
        return Err(RUIM);
    }
    Ok(v)
}

#[cfg(test)]
mod testes {
    use super::*;

    struct Sorteio(u64);
    impl Sorteio {
        fn ate(&mut self, n: u64) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            (z ^ (z >> 31)) % n.max(1)
        }
        fn valor(&mut self, tipo: u64) -> Valor {
            match tipo {
                0 => {
                    let especiais = [i64::MIN, -1, 0, 1, i64::MAX, -256, 255, 256];
                    if self.ate(3) == 0 {
                        Valor::Inteiro(especiais[self.ate(8) as usize])
                    } else {
                        Valor::Inteiro(self.ate(u64::MAX) as i64 >> self.ate(60))
                    }
                }
                1 => {
                    let especiais = [
                        f64::NEG_INFINITY,
                        -1.5,
                        -0.0,
                        0.0,
                        1e-300,
                        2.5,
                        f64::INFINITY,
                        f64::MIN,
                    ];
                    Valor::Real(especiais[self.ate(8) as usize] * (1 + self.ate(3)) as f64)
                }
                2 => {
                    let n = self.ate(5);
                    let alfabeto = ["", "a", "b", "\0", "ã", "ab", "\u{1}"];
                    Valor::Texto((0..n).map(|_| alfabeto[self.ate(7) as usize]).collect())
                }
                3 => {
                    let n = self.ate(5);
                    Valor::Bytes(
                        (0..n)
                            .map(|_| [0u8, 1, 255, 0, 97][self.ate(5) as usize])
                            .collect(),
                    )
                }
                _ => Valor::Booleano(self.ate(2) == 1),
            }
        }
    }

    #[test]
    fn ordem_dos_bytes_e_a_ordem_dos_valores() {
        let mut s = Sorteio(42);
        for _ in 0..40_000 {
            // Chaves compostas de até 3 colunas, com os mesmos tipos nas duas.
            let tipos: Vec<u64> = (0..1 + s.ate(3)).map(|_| s.ate(5)).collect();
            let a: Vec<Valor> = tipos.iter().map(|&t| s.valor(t)).collect();
            let b: Vec<Valor> = tipos.iter().map(|&t| s.valor(t)).collect();
            let logica = a
                .iter()
                .zip(&b)
                .map(|(x, y)| match (x, y) {
                    // -0.0 e 0.0 são a mesma chave.
                    (Valor::Real(x), Valor::Real(y)) if x == y => Ordering::Equal,
                    _ => x.comparar(y),
                })
                .find(|o| *o != Ordering::Equal)
                .unwrap_or(Ordering::Equal);
            let (ca, cb) = (codificar_chave(&a).unwrap(), codificar_chave(&b).unwrap());
            assert_eq!(ca.cmp(&cb), logica, "{a:?} x {b:?}");
        }
        assert!(codificar_chave(&[Valor::Real(f64::NAN)]).is_err());
        // Nulo vem antes de tudo.
        assert!(
            codificar_chave(&[Valor::Nulo]).unwrap()
                < codificar_chave(&[Valor::Inteiro(i64::MIN)]).unwrap()
        );
        // Prefixo de chave composta vem antes de qualquer chave que começa com ele.
        let prefixo = codificar_chave(&["sp".into()]).unwrap();
        let cheia = codificar_chave(&["sp".into(), Valor::Inteiro(i64::MIN)]).unwrap();
        assert!(prefixo < cheia && cheia.starts_with(&prefixo));
    }

    #[test]
    fn linha_vai_e_volta() {
        let mut s = Sorteio(7);
        for _ in 0..5000 {
            let linha: Vec<Valor> = (0..s.ate(8))
                .map(|_| {
                    let t = s.ate(6);
                    if t == 5 { Valor::Nulo } else { s.valor(t) }
                })
                .collect();
            let bytes = codificar_linha(&linha);
            let volta = decodificar_linha(&bytes).unwrap();
            assert_eq!(volta.len(), linha.len());
            for (a, b) in linha.iter().zip(&volta) {
                match (a, b) {
                    (Valor::Real(x), Valor::Real(y)) => assert_eq!(x.to_bits(), y.to_bits()),
                    _ => assert_eq!(a, b),
                }
            }
            // Qualquer corte dá erro, nunca pânico.
            if !bytes.is_empty() {
                let corte = s.ate(bytes.len() as u64) as usize;
                let _ = decodificar_linha(&bytes[..corte]);
            }
        }
        assert!(decodificar_linha(&[1, 0, 9]).is_err());
        assert!(
            decodificar_linha(&[0, 0, 0]).is_err(),
            "sobra no fim é erro"
        );
    }
}
