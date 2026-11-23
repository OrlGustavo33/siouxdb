//! Histograma log-linear: cada potência de 2 é dividida em 8 baldes, então o valor
//! informado fica no máximo 12,5 % acima do real. 496 baldes cobrem de 0 a `u64::MAX`.

use std::sync::atomic::{AtomicU64, Ordering};

const SUB_BITS: u32 = 3;
const SUB: u64 = 1 << SUB_BITS;
const BALDES: usize = ((64 - SUB_BITS as usize) + 1) * SUB as usize;

fn balde(v: u64) -> usize {
    if v < SUB {
        return v as usize;
    }
    let e = 63 - v.leading_zeros();
    let sub = (v >> (e - SUB_BITS)) & (SUB - 1);
    ((e - SUB_BITS + 1) as u64 * SUB + sub) as usize
}

/// Maior valor que cai no balde `i`.
fn teto(i: usize) -> u64 {
    let i = i as u64;
    if i < SUB {
        return i;
    }
    let e = (i / SUB) as u32 + SUB_BITS - 1;
    let sub = i % SUB;
    let largura = 1u64 << (e - SUB_BITS);
    ((SUB + sub) << (e - SUB_BITS)).saturating_add(largura - 1)
}

/// Histograma que várias threads alimentam ao mesmo tempo sem trava.
pub struct Histograma {
    baldes: Box<[AtomicU64]>,
    soma: AtomicU64,
    maximo: AtomicU64,
}

impl Default for Histograma {
    fn default() -> Self {
        Self::novo()
    }
}

impl std::fmt::Debug for Histograma {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let foto = self.foto();
        f.debug_struct("Histograma")
            .field("contagem", &foto.contagem())
            .field("maximo", &foto.maximo)
            .finish()
    }
}

impl Histograma {
    /// Histograma vazio.
    pub fn novo() -> Self {
        Histograma {
            baldes: (0..BALDES).map(|_| AtomicU64::new(0)).collect(),
            soma: AtomicU64::new(0),
            maximo: AtomicU64::new(0),
        }
    }

    /// Registra um valor (ex.: nanossegundos).
    pub fn registrar(&self, v: u64) {
        self.baldes[balde(v)].fetch_add(1, Ordering::Relaxed);
        self.soma.fetch_add(v, Ordering::Relaxed);
        self.maximo.fetch_max(v, Ordering::Relaxed);
    }

    /// Registra a duração em nanossegundos.
    pub fn registrar_duracao(&self, d: std::time::Duration) {
        self.registrar(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    }

    /// Cópia dos valores acumulados até agora.
    pub fn foto(&self) -> FotoHistograma {
        FotoHistograma {
            baldes: self
                .baldes
                .iter()
                .map(|b| b.load(Ordering::Relaxed))
                .collect(),
            soma: self.soma.load(Ordering::Relaxed),
            maximo: self.maximo.load(Ordering::Relaxed),
        }
    }
}

/// Cópia de um histograma num instante. Subtraindo duas fotos tem-se o que aconteceu
/// entre elas (ex.: o último segundo).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FotoHistograma {
    baldes: Vec<u64>,
    /// Soma de todos os valores.
    pub soma: u64,
    /// Maior valor já registrado (acumulado, não só da janela).
    pub maximo: u64,
}

impl Default for FotoHistograma {
    fn default() -> Self {
        FotoHistograma {
            baldes: vec![0; BALDES],
            soma: 0,
            maximo: 0,
        }
    }
}

impl FotoHistograma {
    /// Quantos valores.
    pub fn contagem(&self) -> u64 {
        self.baldes.iter().sum()
    }

    /// O que foi registrado depois de `anterior`.
    pub fn menos(&self, anterior: &FotoHistograma) -> FotoHistograma {
        FotoHistograma {
            baldes: self
                .baldes
                .iter()
                .zip(&anterior.baldes)
                .map(|(a, b)| a.saturating_sub(*b))
                .collect(),
            soma: self.soma.saturating_sub(anterior.soma),
            maximo: self.maximo,
        }
    }

    /// Percentil `q` (0 a 1). `None` se vazio.
    pub fn percentil(&self, q: f64) -> Option<u64> {
        let total = self.contagem();
        if total == 0 {
            return None;
        }
        let alvo = ((q.clamp(0.0, 1.0) * total as f64).ceil() as u64).max(1);
        let mut acumulado = 0;
        for (i, &n) in self.baldes.iter().enumerate() {
            acumulado += n;
            if acumulado >= alvo {
                // O máximo acumulado também é teto válido e aperta o último balde.
                return Some(teto(i).min(self.maximo));
            }
        }
        Some(self.maximo)
    }

    /// Média. `None` se vazio.
    pub fn media(&self) -> Option<f64> {
        let n = self.contagem();
        (n > 0).then(|| self.soma as f64 / n as f64)
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn baldes_sao_continuos_e_crescentes() {
        assert_eq!(balde(0), 0);
        assert_eq!(balde(7), 7);
        assert_eq!(balde(8), 8);
        assert_eq!(balde(16), 16);
        assert_eq!(balde(u64::MAX), BALDES - 1);
        assert_eq!(teto(BALDES - 1), u64::MAX);
        for v in [1u64, 9, 100, 1_000, 65_535, 1 << 40, u64::MAX / 3] {
            let i = balde(v);
            assert!(teto(i) >= v, "teto({i}) < {v}");
            if i > 0 {
                assert!(
                    teto(i - 1) < v,
                    "valor {v} deveria estar num balde anterior"
                );
            }
            // erro relativo de no máximo 12,5 %
            assert!((teto(i) - v) as f64 <= v as f64 * 0.125 + 1.0);
        }
    }

    #[test]
    fn percentis_aproximados() {
        let h = Histograma::novo();
        for v in 1..=10_000u64 {
            h.registrar(v * 1000);
        }
        let f = h.foto();
        assert_eq!(f.contagem(), 10_000);
        let p50 = f.percentil(0.5).unwrap() as f64;
        let p99 = f.percentil(0.99).unwrap() as f64;
        assert!(
            (5_000_000.0..=5_000_000.0 * 1.125).contains(&p50),
            "p50 {p50}"
        );
        assert!(
            (9_900_000.0..=9_900_000.0 * 1.125).contains(&p99),
            "p99 {p99}"
        );
        assert_eq!(f.maximo, 10_000_000);
        assert!((f.media().unwrap() - 5_000_500.0).abs() < 1.0);
        assert_eq!(FotoHistograma::default().percentil(0.5), None);
    }

    #[test]
    fn janela_por_subtracao() {
        let h = Histograma::novo();
        h.registrar(10);
        let antes = h.foto();
        h.registrar(1_000_000);
        h.registrar(1_000_000);
        let janela = h.foto().menos(&antes);
        assert_eq!(janela.contagem(), 2);
        assert!(janela.percentil(0.01).unwrap() >= 1_000_000);
    }

    #[test]
    fn varias_threads() {
        let h = std::sync::Arc::new(Histograma::novo());
        let mut ts = Vec::new();
        for t in 0..4 {
            let h = h.clone();
            ts.push(std::thread::spawn(move || {
                for i in 0..10_000 {
                    h.registrar(t * 10_000 + i);
                }
            }));
        }
        for t in ts {
            t.join().unwrap();
        }
        assert_eq!(h.foto().contagem(), 40_000);
    }
}
