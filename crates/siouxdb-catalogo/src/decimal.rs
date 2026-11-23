//! Decimal exato de ponto fixo: 10 casas depois da vírgula, guardado como inteiro de 128
//! bits. Soma e subtração são exatas; multiplicação e divisão arredondam na 10ª casa
//! (metade para longe do zero). Cabe até ±17.014.118.346.046.923.173 (1,7 × 10²⁸).
//!
//! É o tipo para dinheiro e quantidades: `0.1 + 0.2` dá exatamente `0.3`.

use std::fmt;

/// Casas decimais guardadas.
pub const CASAS: u32 = 10;
const ESCALA: i128 = 10_000_000_000;
const ESCALA_U: u128 = ESCALA as u128;

/// Número decimal exato com 10 casas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Decimal(i128);

impl Decimal {
    /// Zero.
    pub const ZERO: Decimal = Decimal(0);

    /// De um inteiro (sempre cabe).
    pub fn de_inteiro(i: i64) -> Decimal {
        Decimal(i as i128 * ESCALA)
    }

    /// Do valor interno (o número vezes 10¹⁰).
    pub fn de_bruto(b: i128) -> Decimal {
        Decimal(b)
    }

    /// O valor interno (o número vezes 10¹⁰).
    pub fn bruto(self) -> i128 {
        self.0
    }

    /// De texto como `-123.45` (ponto decimal). Casas além da 10ª são arredondadas.
    pub fn de_texto(s: &str) -> Option<Decimal> {
        let s = s.trim();
        let (negativo, s) = match s.as_bytes().first()? {
            b'-' => (true, &s[1..]),
            b'+' => (false, &s[1..]),
            _ => (false, s),
        };
        let (inteira, fracao) = s.split_once('.').unwrap_or((s, ""));
        if (inteira.is_empty() && fracao.is_empty())
            || !inteira
                .bytes()
                .chain(fracao.bytes())
                .all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let mut v: i128 = 0;
        for b in inteira.bytes() {
            v = v.checked_mul(10)?.checked_add((b - b'0') as i128)?;
        }
        v = v.checked_mul(ESCALA)?;
        let mut peso = ESCALA / 10;
        for (i, b) in fracao.bytes().enumerate() {
            let d = (b - b'0') as i128;
            if i < CASAS as usize {
                v = v.checked_add(d * peso)?;
                peso /= 10;
            } else {
                if i == CASAS as usize && d >= 5 {
                    v = v.checked_add(1)?;
                }
                break;
            }
        }
        Some(Decimal(if negativo { -v } else { v }))
    }

    /// De um número de ponto flutuante, pela representação decimal mais curta dele
    /// (`0.1` vira exatamente 0,1). `None` para infinito, NaN ou fora do limite.
    pub fn de_real(f: f64) -> Option<Decimal> {
        if !f.is_finite() {
            return None;
        }
        Decimal::de_texto(&format!("{f}"))
    }

    /// Aproximação em ponto flutuante.
    pub fn para_real(self) -> f64 {
        self.0 as f64 / ESCALA as f64
    }

    /// Soma. `None` se estourar.
    pub fn somar(self, o: Decimal) -> Option<Decimal> {
        self.0.checked_add(o.0).map(Decimal)
    }

    /// Subtração. `None` se estourar.
    pub fn subtrair(self, o: Decimal) -> Option<Decimal> {
        self.0.checked_sub(o.0).map(Decimal)
    }

    /// Troca o sinal.
    pub fn negativo(self) -> Option<Decimal> {
        self.0.checked_neg().map(Decimal)
    }

    /// Valor absoluto.
    pub fn absoluto(self) -> Option<Decimal> {
        self.0.checked_abs().map(Decimal)
    }

    fn com_sinal(magnitude: u128, negativo: bool) -> Option<Decimal> {
        let v = i128::try_from(magnitude).ok()?;
        Some(Decimal(if negativo { -v } else { v }))
    }

    /// Multiplicação, arredondada na 10ª casa. `None` se estourar.
    pub fn multiplicar(self, o: Decimal) -> Option<Decimal> {
        let (a, b) = (self.0.unsigned_abs(), o.0.unsigned_abs());
        // a × b / 10¹⁰ sem passar de 128 bits: separa a parte inteira e a fração de `a`.
        let (ai, af) = (a / ESCALA_U, a % ESCALA_U);
        let r = ai
            .checked_mul(b)?
            .checked_add(af.checked_mul(b)?.checked_add(ESCALA_U / 2)? / ESCALA_U)?;
        Decimal::com_sinal(r, (self.0 < 0) != (o.0 < 0))
    }

    /// Divisão, arredondada na 10ª casa. `None` se o divisor é zero ou se estourar.
    pub fn dividir(self, o: Decimal) -> Option<Decimal> {
        if o.0 == 0 {
            return None;
        }
        let (a, b) = (self.0.unsigned_abs(), o.0.unsigned_abs());
        let (q, r) = (a / b, a % b);
        let fracao = r.checked_mul(ESCALA_U)?.checked_add(b / 2)? / b;
        let total = q.checked_mul(ESCALA_U)?.checked_add(fracao)?;
        Decimal::com_sinal(total, (self.0 < 0) != (o.0 < 0))
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.0.unsigned_abs();
        let (inteira, fracao) = (m / ESCALA_U, m % ESCALA_U);
        if self.0 < 0 {
            write!(f, "-")?;
        }
        write!(f, "{inteira}")?;
        if fracao != 0 {
            let s = format!("{fracao:010}");
            write!(f, ".{}", s.trim_end_matches('0'))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::de_texto(s).unwrap()
    }

    #[test]
    fn exato_onde_o_ponto_flutuante_erra() {
        assert_eq!(d("0.1").somar(d("0.2")).unwrap(), d("0.3"));
        assert_eq!(d("0.1").somar(d("0.2")).unwrap().to_string(), "0.3");
        let mut soma = Decimal::ZERO;
        for _ in 0..1000 {
            soma = soma.somar(d("0.01")).unwrap();
        }
        assert_eq!(soma, d("10"));
        assert_eq!(Decimal::de_real(0.1).unwrap(), d("0.1"));
        assert_eq!(Decimal::de_real(1234.56).unwrap().to_string(), "1234.56");
        assert!(Decimal::de_real(f64::NAN).is_none());
    }

    #[test]
    fn texto_vai_e_volta_e_ordena() {
        for s in [
            "0",
            "1",
            "-1",
            "0.5",
            "-0.0000000001",
            "123456789012345678.1234567891",
            "99.99",
        ] {
            assert_eq!(d(s).to_string(), s);
        }
        assert_eq!(d("1.50").to_string(), "1.5");
        assert_eq!(d(".5"), d("0.5"));
        assert_eq!(d("5."), d("5"));
        assert_eq!(
            d("1.00000000005").to_string(),
            "1.0000000001",
            "arredonda a 11ª casa"
        );
        assert_eq!(d("-1.00000000004").to_string(), "-1");
        for ruim in ["", ".", "abc", "1.2.3", "1e5", "--1", "1,5"] {
            assert!(Decimal::de_texto(ruim).is_none(), "{ruim}");
        }
        assert!(
            d("-2") < d("-1.5")
                && d("-1.5") < d("0")
                && d("0") < d("0.0000000001")
                && d("9.9") < d("10")
        );
        assert!(
            Decimal::de_texto(&"9".repeat(40)).is_none(),
            "estouro vira None"
        );
    }

    #[test]
    fn multiplica_e_divide_com_arredondamento() {
        assert_eq!(d("19.99").multiplicar(d("3")).unwrap(), d("59.97"));
        assert_eq!(d("0.1").multiplicar(d("0.1")).unwrap(), d("0.01"));
        assert_eq!(d("-2.5").multiplicar(d("4")).unwrap(), d("-10"));
        assert_eq!(
            d("0.00001").multiplicar(d("0.00001")).unwrap(),
            d("0.0000000001")
        );
        assert_eq!(
            d("0.00001").multiplicar(d("0.000004")).unwrap(),
            Decimal::ZERO,
            "4e-11 arredonda para baixo"
        );
        assert_eq!(
            d("0.00001").multiplicar(d("0.000005")).unwrap(),
            d("0.0000000001"),
            "5e-11 arredonda para cima"
        );
        assert_eq!(d("10").dividir(d("4")).unwrap(), d("2.5"));
        assert_eq!(d("1").dividir(d("3")).unwrap().to_string(), "0.3333333333");
        assert_eq!(d("2").dividir(d("3")).unwrap().to_string(), "0.6666666667");
        assert_eq!(
            d("-1").dividir(d("3")).unwrap().to_string(),
            "-0.3333333333"
        );
        assert!(d("1").dividir(Decimal::ZERO).is_none());
        // Valores grandes: preço × quantidade de bilhões não estoura.
        assert_eq!(
            d("123456789.1234567891")
                .multiplicar(d("1000000"))
                .unwrap()
                .to_string(),
            "123456789123456.7891"
        );
        assert!(
            d("99999999999999999999")
                .multiplicar(d("99999999999999999999"))
                .is_none()
        );
        assert_eq!(
            Decimal::de_inteiro(i64::MIN).to_string(),
            i64::MIN.to_string()
        );
        // Confere com contas em inteiros: (a×b)/b = a para a com poucas casas.
        let mut x = 7u64;
        for _ in 0..2000 {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let a = Decimal::de_bruto(
                ((x >> 20) % 1_000_000_000_000) as i128 * 100_000_000 - 500_000_000_000_000,
            );
            let b = Decimal::de_inteiro(1 + ((x >> 8) % 9999) as i64);
            assert_eq!(a.multiplicar(b).unwrap().dividir(b).unwrap(), a);
        }
    }
}
