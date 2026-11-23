//! Métricas do SiouxDB.
//!
//! - [`Contador`] e [`Medidor`]: atômicos, custo de uma instrução;
//! - [`Histograma`]: latências em baldes logarítmicos (erro máximo de 12,5 %), sem trava;
//! - [`Coletor`]: recebe uma [`Leitura`] por segundo e monta a [`FotoPainel`] — números
//!   atuais, séries dos últimos 60 s e alertas — que o painel do Studio mostra.
//!
//! O mesmo coletor serve o banco embutido e o modo servidor.

mod coletor;
mod histograma;

pub use coletor::{
    Alerta, Coletor, FotoPainel, Indicadores, Leitura, LimitesAlerta, NivelAlerta, PONTOS_SERIE,
    Series,
};
pub use histograma::{FotoHistograma, Histograma};

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// Contador que só cresce (ex.: consultas executadas).
#[derive(Debug, Default)]
pub struct Contador(AtomicU64);

impl Contador {
    /// Contador zerado.
    pub const fn novo() -> Self {
        Contador(AtomicU64::new(0))
    }

    /// Soma 1.
    pub fn incrementar(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    /// Soma `n`.
    pub fn somar(&self, n: u64) {
        self.0.fetch_add(n, Ordering::Relaxed);
    }

    /// Valor atual.
    pub fn valor(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Valor que sobe e desce (ex.: conexões abertas).
#[derive(Debug, Default)]
pub struct Medidor(AtomicI64);

impl Medidor {
    /// Medidor zerado.
    pub const fn novo() -> Self {
        Medidor(AtomicI64::new(0))
    }

    /// Soma 1.
    pub fn subir(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    /// Tira 1.
    pub fn descer(&self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }

    /// Define o valor.
    pub fn definir(&self, v: i64) {
        self.0.store(v, Ordering::Relaxed);
    }

    /// Valor atual.
    pub fn valor(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
}
