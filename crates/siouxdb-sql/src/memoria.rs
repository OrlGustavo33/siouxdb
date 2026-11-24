//! Orçamento de memória das consultas.
//!
//! Tudo o que uma consulta precisa guardar ao mesmo tempo (o resultado, as linhas de um
//! `ORDER BY`, os grupos de um `GROUP BY`, a tabela da direita de um `JOIN` sem busca, as
//! chaves de um `UPDATE`/`DELETE`) conta aqui. Passou do limite: a consulta para com
//! [`ErroSql::MemoriaEsgotada`](crate::ErroSql::MemoriaEsgotada), em vez de o processo
//! ser derrubado pelo sistema por falta de memória. Linhas que só passam (filtradas,
//! contadas num agregado, puladas pelo `OFFSET`) não contam.

use crate::{ErroSql, Resultado};
use siouxdb_catalogo::Valor;
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Limite padrão por consulta: 512 MiB.
pub const LIMITE_PADRAO: usize = 512 << 20;

static LIMITE: AtomicUsize = AtomicUsize::new(LIMITE_PADRAO);

thread_local! {
    static LIMITE_DA_THREAD: Cell<usize> = const { Cell::new(0) };
}

/// Roda `f` com outro limite de memória por consulta, só nesta thread (ex.: um limite menor
/// para consultas vindas de fora).
pub fn com_limite_memoria<R>(bytes: usize, f: impl FnOnce() -> R) -> R {
    let antes = LIMITE_DA_THREAD.with(|l| l.replace(bytes));
    let r = f();
    LIMITE_DA_THREAD.with(|l| l.set(antes));
    r
}

/// Troca o limite de memória por consulta (vale para o processo todo; 0 = padrão).
pub fn definir_limite_memoria(bytes: usize) {
    let b = if bytes == 0 { LIMITE_PADRAO } else { bytes };
    LIMITE.store(b, Ordering::Relaxed);
}

/// Limite de memória por consulta em vigor.
pub fn limite_memoria() -> usize {
    match LIMITE_DA_THREAD.with(Cell::get) {
        0 => LIMITE.load(Ordering::Relaxed),
        n => n,
    }
}

/// Quanto uma consulta já guardou.
pub(crate) struct Orcamento {
    usado: Cell<usize>,
    limite: usize,
}

impl Orcamento {
    pub(crate) fn novo() -> Orcamento {
        Orcamento {
            usado: Cell::new(0),
            limite: limite_memoria(),
        }
    }

    pub(crate) fn somar(&self, bytes: usize) -> Resultado<()> {
        let usado = self.usado.get().saturating_add(bytes);
        if usado > self.limite {
            return Err(ErroSql::MemoriaEsgotada {
                limite: self.limite,
            });
        }
        self.usado.set(usado);
        Ok(())
    }

    pub(crate) fn tirar(&self, bytes: usize) {
        self.usado.set(self.usado.get().saturating_sub(bytes));
    }
}

/// Bytes (aproximados) que uma linha ocupa na memória.
pub(crate) fn tamanho_linha(l: &[Valor]) -> usize {
    24 + l
        .iter()
        .map(|v| {
            std::mem::size_of::<Valor>()
                + match v {
                    Valor::Texto(s) => s.len(),
                    Valor::Bytes(b) => b.len(),
                    _ => 0,
                }
        })
        .sum::<usize>()
}
