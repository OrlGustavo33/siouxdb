//! Cache com o algoritmo de descarte **SIEVE** (NSDI 2024).
//!
//! Uma fila do mais novo para o mais antigo, um bit "visitado" por item e um ponteiro
//! (a "mão") que anda do mais antigo para o mais novo. Num acerto só se liga o bit — nada
//! muda de lugar, ao contrário do LRU. Para abrir espaço, a mão pula (e desmarca) os
//! visitados e descarta o primeiro não visitado que encontrar.

use std::collections::HashMap;
use std::hash::Hash;

/// Contadores do cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EstatisticasCache {
    /// Buscas que encontraram o item.
    pub acertos: u64,
    /// Buscas que não encontraram.
    pub faltas: u64,
    /// Itens descartados para abrir espaço.
    pub descartes: u64,
}

struct No<K, V> {
    chave: K,
    valor: V,
    visitado: bool,
    mais_novo: Option<usize>,
    mais_antigo: Option<usize>,
}

/// Cache de capacidade fixa com descarte SIEVE.
pub struct CacheSieve<K, V> {
    capacidade: usize,
    mapa: HashMap<K, usize>,
    nos: Vec<Option<No<K, V>>>,
    vagas: Vec<usize>,
    mais_novo: Option<usize>,
    mais_antigo: Option<usize>,
    mao: Option<usize>,
    estatisticas: EstatisticasCache,
}

impl<K: Hash + Eq + Clone, V: Clone> CacheSieve<K, V> {
    /// Cache para até `capacidade` itens (no mínimo 1).
    pub fn novo(capacidade: usize) -> Self {
        let capacidade = capacidade.max(1);
        CacheSieve {
            capacidade,
            mapa: HashMap::with_capacity(capacidade),
            nos: Vec::with_capacity(capacidade),
            vagas: Vec::new(),
            mais_novo: None,
            mais_antigo: None,
            mao: None,
            estatisticas: EstatisticasCache::default(),
        }
    }

    /// Quantos itens estão guardados.
    pub fn len(&self) -> usize {
        self.mapa.len()
    }

    /// Está vazio?
    pub fn is_empty(&self) -> bool {
        self.mapa.is_empty()
    }

    /// Capacidade máxima.
    pub fn capacidade(&self) -> usize {
        self.capacidade
    }

    /// Contadores acumulados.
    pub fn estatisticas(&self) -> EstatisticasCache {
        self.estatisticas
    }

    /// Busca um item e o marca como visitado.
    pub fn obter(&mut self, chave: &K) -> Option<V> {
        match self.mapa.get(chave) {
            Some(&i) => {
                self.estatisticas.acertos += 1;
                let no = self.no_mut(i);
                no.visitado = true;
                Some(no.valor.clone())
            }
            None => {
                self.estatisticas.faltas += 1;
                None
            }
        }
    }

    /// Guarda (ou troca) um item, descartando outro se o cache estiver cheio.
    pub fn inserir(&mut self, chave: K, valor: V) {
        if let Some(&i) = self.mapa.get(&chave) {
            let no = self.no_mut(i);
            no.valor = valor;
            no.visitado = true;
            return;
        }
        if self.mapa.len() >= self.capacidade {
            self.descartar();
        }
        let no = No {
            chave: chave.clone(),
            valor,
            visitado: false,
            mais_novo: None,
            mais_antigo: self.mais_novo,
        };
        let i = match self.vagas.pop() {
            Some(i) => {
                self.nos[i] = Some(no);
                i
            }
            None => {
                self.nos.push(Some(no));
                self.nos.len() - 1
            }
        };
        if let Some(antes) = self.mais_novo {
            self.no_mut(antes).mais_novo = Some(i);
        }
        self.mais_novo = Some(i);
        if self.mais_antigo.is_none() {
            self.mais_antigo = Some(i);
        }
        self.mapa.insert(chave, i);
    }

    /// Tira um item do cache, se estiver lá.
    pub fn remover(&mut self, chave: &K) -> Option<V> {
        let i = self.mapa.remove(chave)?;
        Some(self.desligar(i).valor)
    }

    /// Esvazia o cache (os contadores continuam).
    pub fn limpar(&mut self) {
        self.mapa.clear();
        self.nos.clear();
        self.vagas.clear();
        self.mais_novo = None;
        self.mais_antigo = None;
        self.mao = None;
    }

    fn no_mut(&mut self, i: usize) -> &mut No<K, V> {
        self.nos[i]
            .as_mut()
            .expect("índice do cache aponta para item existente")
    }

    fn descartar(&mut self) {
        let Some(mut i) = self.mao.or(self.mais_antigo) else {
            return;
        };
        loop {
            let antigo = self.mais_antigo;
            let no = self.no_mut(i);
            if !no.visitado {
                break;
            }
            no.visitado = false;
            i = no.mais_novo.or(antigo).expect("fila não vazia");
        }
        let no = self.desligar(i);
        self.mapa.remove(&no.chave);
        self.estatisticas.descartes += 1;
    }

    /// Tira o nó da fila e libera a vaga. A mão, se estava nele, segue para o próximo.
    fn desligar(&mut self, i: usize) -> No<K, V> {
        let no = self.nos[i]
            .take()
            .expect("índice do cache aponta para item existente");
        if self.mao == Some(i) {
            self.mao = no.mais_novo;
        }
        match no.mais_novo {
            Some(n) => self.no_mut(n).mais_antigo = no.mais_antigo,
            None => self.mais_novo = no.mais_antigo,
        }
        match no.mais_antigo {
            Some(a) => self.no_mut(a).mais_novo = no.mais_novo,
            None => self.mais_antigo = no.mais_novo,
        }
        self.vagas.push(i);
        no
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn guarda_e_devolve() {
        let mut c = CacheSieve::novo(2);
        assert!(c.is_empty());
        c.inserir(1, "um");
        c.inserir(2, "dois");
        assert_eq!(c.obter(&1), Some("um"));
        assert_eq!(c.obter(&3), None);
        c.inserir(1, "UM");
        assert_eq!(c.obter(&1), Some("UM"));
        assert_eq!(c.len(), 2);
        let e = c.estatisticas();
        assert_eq!((e.acertos, e.faltas, e.descartes), (2, 1, 0));
    }

    #[test]
    fn descarta_o_nao_visitado_e_poupa_o_visitado() {
        let mut c = CacheSieve::novo(3);
        c.inserir(1, ());
        c.inserir(2, ());
        c.inserir(3, ());
        c.obter(&1); // o mais antigo foi visitado: ganha segunda chance
        c.inserir(4, ()); // sai o 2
        assert!(c.obter(&2).is_none());
        assert!(c.obter(&1).is_some() && c.obter(&3).is_some() && c.obter(&4).is_some());
        // Agora todos visitados: a mão desmarca todos e descarta onde parou.
        c.inserir(5, ());
        assert_eq!(c.len(), 3);
        assert!(c.obter(&5).is_some());
    }

    #[test]
    fn item_quente_sobrevive_a_uma_enxurrada() {
        let mut c = CacheSieve::novo(8);
        c.inserir(0u32, ());
        for i in 1..1000u32 {
            assert!(
                c.obter(&0).is_some(),
                "o item quente foi descartado na rodada {i}"
            );
            c.inserir(i, ());
            assert!(c.len() <= 8);
        }
    }

    #[test]
    fn remover_e_limpar_mantem_a_fila_coerente() {
        let mut c = CacheSieve::novo(4);
        for i in 0..4 {
            c.inserir(i, i * 10);
        }
        assert_eq!(c.remover(&0), Some(0));
        assert_eq!(c.remover(&3), Some(30));
        assert_eq!(c.remover(&9), None);
        for i in 10..20 {
            c.inserir(i, i);
            c.obter(&(i - 1));
        }
        assert_eq!(c.len(), 4);
        c.limpar();
        assert!(c.is_empty());
        c.inserir(1, 1);
        assert_eq!(c.obter(&1), Some(1));
    }
}
