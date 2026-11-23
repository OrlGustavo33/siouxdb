//! Fila justa do escritor único: quem pede primeiro escreve primeiro, com tempo máximo
//! de espera. (O `Mutex` da biblioteca padrão não promete ordem.)

use crate::{ErroBanco, Resultado};
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Estado {
    ocupado: bool,
    fila: VecDeque<u64>,
    proximo: u64,
}

#[derive(Default)]
pub(crate) struct FilaEscritor {
    estado: Mutex<Estado>,
    cv: Condvar,
}

/// A vez de escrever. Soltar (drop) libera para o próximo da fila.
pub(crate) struct Vez<'a> {
    fila: &'a FilaEscritor,
}

impl Drop for Vez<'_> {
    fn drop(&mut self) {
        self.fila.travar().ocupado = false;
        self.fila.cv.notify_all();
    }
}

impl FilaEscritor {
    fn travar(&self) -> MutexGuard<'_, Estado> {
        self.estado.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Espera a vez, por ordem de chegada, até `espera`.
    pub(crate) fn adquirir(&self, espera: Duration) -> Resultado<Vez<'_>> {
        let limite = Instant::now() + espera;
        let mut e = self.travar();
        let id = e.proximo;
        e.proximo += 1;
        e.fila.push_back(id);
        loop {
            if !e.ocupado && e.fila.front() == Some(&id) {
                e.fila.pop_front();
                e.ocupado = true;
                return Ok(Vez { fila: self });
            }
            let agora = Instant::now();
            if agora >= limite {
                e.fila.retain(|&x| x != id);
                drop(e);
                self.cv.notify_all(); // quem estava atrás pode ter virado o primeiro
                return Err(ErroBanco::EsperaEsgotada(espera));
            }
            e = self
                .cv
                .wait_timeout(e, limite - agora)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    /// Quantos estão esperando.
    pub(crate) fn esperando(&self) -> usize {
        self.travar().fila.len()
    }

    /// Alguém está escrevendo agora?
    pub(crate) fn ocupado(&self) -> bool {
        self.travar().ocupado
    }
}

#[cfg(test)]
mod testes {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn ordem_de_chegada_e_espera_esgotada() {
        let fila = Arc::new(FilaEscritor::default());
        let vez = fila.adquirir(Duration::from_secs(1)).unwrap();
        assert!(matches!(
            fila.adquirir(Duration::from_millis(30)),
            Err(ErroBanco::EsperaEsgotada(_))
        ));
        assert_eq!(fila.esperando(), 0);

        let ordem = Arc::new(Mutex::new(Vec::new()));
        let mut ts = Vec::new();
        for i in 0..5 {
            let (f, o) = (fila.clone(), ordem.clone());
            ts.push(std::thread::spawn(move || {
                let _v = f.adquirir(Duration::from_secs(10)).unwrap();
                o.lock().unwrap().push(i);
            }));
            // garante a ordem de chegada
            while fila.esperando() < i + 1 {
                std::thread::yield_now();
            }
        }
        drop(vez);
        for t in ts {
            t.join().unwrap();
        }
        assert_eq!(*ordem.lock().unwrap(), vec![0, 1, 2, 3, 4]);
        assert!(!fila.ocupado());
    }
}
