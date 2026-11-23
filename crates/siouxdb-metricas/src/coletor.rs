//! Coletor do painel: transforma leituras acumuladas (contadores que só crescem) em taxas
//! por segundo, séries dos últimos 60 s e alertas.

use crate::FotoHistograma;
use std::collections::VecDeque;
use std::time::Instant;

/// Quantos pontos cada série guarda (1 por leitura; com leitura a cada segundo = 60 s).
pub const PONTOS_SERIE: usize = 60;

/// Valores lidos da fonte (banco embutido ou servidor). Contadores são **acumulados**.
#[derive(Debug, Clone)]
pub struct Leitura {
    /// Quando a leitura foi feita.
    pub instante: Instant,
    /// Conexões (ou sessões) abertas agora.
    pub conexoes_ativas: u64,
    /// Limite de conexões (0 = sem limite / não se aplica).
    pub conexoes_limite: u64,
    /// Operações concluídas (consultas, ou operações de página enquanto não há SQL).
    pub operacoes: u64,
    /// Acertos do cache de páginas.
    pub acertos_cache: u64,
    /// Faltas do cache de páginas.
    pub faltas_cache: u64,
    /// Gravações de página / registros do WAL.
    pub gravacoes: u64,
    /// Transações confirmadas.
    pub confirmacoes: u64,
    /// Tamanho do banco em bytes.
    pub bytes_banco: u64,
    /// Transações esperando o escritor único.
    pub fila_escritor: u64,
    /// Latências acumuladas (ns) das operações.
    pub latencia: FotoHistograma,
}

impl Leitura {
    /// Leitura zerada no instante atual.
    pub fn vazia() -> Self {
        Leitura {
            instante: Instant::now(),
            conexoes_ativas: 0,
            conexoes_limite: 0,
            operacoes: 0,
            acertos_cache: 0,
            faltas_cache: 0,
            gravacoes: 0,
            confirmacoes: 0,
            bytes_banco: 0,
            fila_escritor: 0,
            latencia: FotoHistograma::default(),
        }
    }
}

/// Números do momento (o "cartão grande" do painel).
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Indicadores {
    /// Conexões abertas.
    pub conexoes_ativas: u64,
    /// Limite de conexões (0 = não se aplica).
    pub conexoes_limite: u64,
    /// Operações por segundo.
    pub operacoes_por_s: f64,
    /// Latência p50 em microssegundos (no último intervalo).
    pub latencia_p50_us: Option<f64>,
    /// Latência p99 em microssegundos (no último intervalo).
    pub latencia_p99_us: Option<f64>,
    /// Acerto do cache em % (no último intervalo; `None` se não houve leitura).
    pub acerto_cache_pct: Option<f64>,
    /// Gravações por segundo.
    pub gravacoes_por_s: f64,
    /// Confirmações por segundo.
    pub confirmacoes_por_s: f64,
    /// Tamanho do banco em bytes.
    pub bytes_banco: u64,
    /// Transações esperando o escritor.
    pub fila_escritor: u64,
}

/// Séries dos últimos [`PONTOS_SERIE`] intervalos, do mais antigo para o mais novo.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Series {
    /// Conexões abertas.
    pub conexoes: Vec<f64>,
    /// Operações por segundo.
    pub operacoes_por_s: Vec<f64>,
    /// Latência p99 em µs (0 quando não houve operação).
    pub latencia_p99_us: Vec<f64>,
    /// Acerto do cache em % (repete o anterior quando não houve leitura).
    pub acerto_cache_pct: Vec<f64>,
    /// Gravações por segundo.
    pub gravacoes_por_s: Vec<f64>,
    /// Tamanho do banco em bytes.
    pub bytes_banco: Vec<f64>,
}

/// Gravidade de um alerta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum NivelAlerta {
    /// Amarelo.
    Aviso,
    /// Vermelho.
    Critico,
}

/// Algo fora do limite configurado.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Alerta {
    /// Gravidade.
    pub nivel: NivelAlerta,
    /// Qual indicador (mesmo nome do campo em [`Indicadores`]).
    pub indicador: &'static str,
    /// Texto curto para a pessoa.
    pub mensagem: String,
}

/// Limites que disparam alertas. Todos configuráveis.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct LimitesAlerta {
    /// Aviso quando a latência p99 passa disto (µs). Crítico ao dobro.
    pub latencia_p99_us: f64,
    /// Aviso quando o acerto do cache fica abaixo disto (%). Só com tráfego relevante.
    pub acerto_cache_min_pct: f64,
    /// Leituras mínimas no intervalo para avaliar o cache (evita alarme com 2 leituras).
    pub leituras_minimas_cache: u64,
    /// Aviso com esta fila no escritor. Crítico ao dobro.
    pub fila_escritor_max: u64,
    /// Aviso quando o uso de conexões passa disto (% do limite). Crítico a partir de 100 %.
    pub conexoes_uso_max_pct: f64,
}

impl Default for LimitesAlerta {
    fn default() -> Self {
        LimitesAlerta {
            latencia_p99_us: 50_000.0,
            acerto_cache_min_pct: 90.0,
            leituras_minimas_cache: 100,
            fila_escritor_max: 5,
            conexoes_uso_max_pct: 90.0,
        }
    }
}

/// O que o painel mostra a cada atualização.
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct FotoPainel {
    /// Números do momento.
    pub indicadores: Indicadores,
    /// Últimos 60 intervalos.
    pub series: Series,
    /// Alertas ativos.
    pub alertas: Vec<Alerta>,
    /// Quantas leituras o coletor já recebeu.
    pub leituras: u64,
}

/// Junta leituras e monta a [`FotoPainel`].
#[derive(Debug)]
pub struct Coletor {
    limites: LimitesAlerta,
    anterior: Option<Leitura>,
    foto: FotoPainel,
    series: [VecDeque<f64>; 6],
}

impl Coletor {
    /// Coletor com os limites dados.
    pub fn novo(limites: LimitesAlerta) -> Self {
        Coletor {
            limites,
            anterior: None,
            foto: FotoPainel::default(),
            series: std::array::from_fn(|_| VecDeque::with_capacity(PONTOS_SERIE)),
        }
    }

    /// Troca os limites de alerta (vale a partir da próxima leitura).
    pub fn definir_limites(&mut self, limites: LimitesAlerta) {
        self.limites = limites;
    }

    /// Limites em uso.
    pub fn limites(&self) -> &LimitesAlerta {
        &self.limites
    }

    /// Última foto montada.
    pub fn foto(&self) -> &FotoPainel {
        &self.foto
    }

    /// Recebe uma leitura nova e devolve a foto atualizada.
    pub fn registrar(&mut self, atual: Leitura) -> &FotoPainel {
        let anterior = self.anterior.take().unwrap_or_else(|| Leitura {
            instante: atual.instante,
            ..atual.clone()
        });
        let segundos = atual
            .instante
            .saturating_duration_since(anterior.instante)
            .as_secs_f64();
        let taxa = |agora: u64, antes: u64| {
            if segundos > 0.0 {
                agora.saturating_sub(antes) as f64 / segundos
            } else {
                0.0
            }
        };
        let janela = atual.latencia.menos(&anterior.latencia);
        let us = |ns: u64| ns as f64 / 1000.0;
        let acertos = atual.acertos_cache.saturating_sub(anterior.acertos_cache);
        let faltas = atual.faltas_cache.saturating_sub(anterior.faltas_cache);
        let leituras_cache = acertos + faltas;

        let ind = Indicadores {
            conexoes_ativas: atual.conexoes_ativas,
            conexoes_limite: atual.conexoes_limite,
            operacoes_por_s: taxa(atual.operacoes, anterior.operacoes),
            latencia_p50_us: janela.percentil(0.50).map(us),
            latencia_p99_us: janela.percentil(0.99).map(us),
            acerto_cache_pct: (leituras_cache > 0)
                .then(|| acertos as f64 * 100.0 / leituras_cache as f64),
            gravacoes_por_s: taxa(atual.gravacoes, anterior.gravacoes),
            confirmacoes_por_s: taxa(atual.confirmacoes, anterior.confirmacoes),
            bytes_banco: atual.bytes_banco,
            fila_escritor: atual.fila_escritor,
        };

        let cache_anterior = self.series[3].back().copied().unwrap_or(100.0);
        let pontos = [
            ind.conexoes_ativas as f64,
            ind.operacoes_por_s,
            ind.latencia_p99_us.unwrap_or(0.0),
            ind.acerto_cache_pct.unwrap_or(cache_anterior),
            ind.gravacoes_por_s,
            ind.bytes_banco as f64,
        ];
        for (serie, ponto) in self.series.iter_mut().zip(pontos) {
            if serie.len() == PONTOS_SERIE {
                serie.pop_front();
            }
            serie.push_back(ponto);
        }
        let v = |i: usize| self.series[i].iter().copied().collect::<Vec<_>>();
        self.foto = FotoPainel {
            alertas: self.alertas(&ind, leituras_cache),
            indicadores: ind,
            series: Series {
                conexoes: v(0),
                operacoes_por_s: v(1),
                latencia_p99_us: v(2),
                acerto_cache_pct: v(3),
                gravacoes_por_s: v(4),
                bytes_banco: v(5),
            },
            leituras: self.foto.leituras + 1,
        };
        self.anterior = Some(atual);
        &self.foto
    }

    fn alertas(&self, ind: &Indicadores, leituras_cache: u64) -> Vec<Alerta> {
        let l = &self.limites;
        let mut a = Vec::new();
        let mut alerta = |nivel, indicador, mensagem: String| {
            a.push(Alerta {
                nivel,
                indicador,
                mensagem,
            })
        };
        if let Some(p99) = ind.latencia_p99_us {
            if p99 > l.latencia_p99_us {
                let nivel = if p99 > 2.0 * l.latencia_p99_us {
                    NivelAlerta::Critico
                } else {
                    NivelAlerta::Aviso
                };
                alerta(
                    nivel,
                    "latencia_p99_us",
                    format!(
                        "latência p99 de {:.1} ms (limite {:.1} ms)",
                        p99 / 1000.0,
                        l.latencia_p99_us / 1000.0
                    ),
                );
            }
        }
        if let Some(pct) = ind.acerto_cache_pct {
            if leituras_cache >= l.leituras_minimas_cache && pct < l.acerto_cache_min_pct {
                alerta(
                    NivelAlerta::Aviso,
                    "acerto_cache_pct",
                    format!(
                        "cache acertando {pct:.0}% (mínimo {:.0}%)",
                        l.acerto_cache_min_pct
                    ),
                );
            }
        }
        if ind.fila_escritor >= l.fila_escritor_max && l.fila_escritor_max > 0 {
            let nivel = if ind.fila_escritor >= 2 * l.fila_escritor_max {
                NivelAlerta::Critico
            } else {
                NivelAlerta::Aviso
            };
            alerta(
                nivel,
                "fila_escritor",
                format!("{} transações esperando para gravar", ind.fila_escritor),
            );
        }
        if ind.conexoes_limite > 0 {
            let uso = ind.conexoes_ativas as f64 * 100.0 / ind.conexoes_limite as f64;
            if uso >= 100.0 {
                alerta(
                    NivelAlerta::Critico,
                    "conexoes_ativas",
                    format!(
                        "limite de conexões atingido ({}/{})",
                        ind.conexoes_ativas, ind.conexoes_limite
                    ),
                );
            } else if uso >= l.conexoes_uso_max_pct {
                alerta(
                    NivelAlerta::Aviso,
                    "conexoes_ativas",
                    format!("{uso:.0}% das conexões em uso"),
                );
            }
        }
        a
    }
}

#[cfg(test)]
mod testes {
    use super::*;
    use crate::Histograma;
    use std::time::Duration;

    #[test]
    fn taxas_series_e_alertas() {
        let h = Histograma::novo();
        let mut c = Coletor::novo(LimitesAlerta::default());
        let t0 = Instant::now();
        let base = Leitura {
            instante: t0,
            conexoes_limite: 10,
            ..Leitura::vazia()
        };
        let f = c.registrar(base.clone());
        assert_eq!(f.indicadores.operacoes_por_s, 0.0);
        assert!(f.alertas.is_empty());

        for _ in 0..200 {
            h.registrar(200_000_000); // 200 ms: bem acima do limite
        }
        let f = c.registrar(Leitura {
            instante: t0 + Duration::from_secs(2),
            conexoes_ativas: 10,
            operacoes: 200,
            acertos_cache: 50,
            faltas_cache: 150,
            gravacoes: 40,
            fila_escritor: 6,
            latencia: h.foto(),
            ..base
        });
        let i = &f.indicadores;
        assert_eq!((i.operacoes_por_s, i.gravacoes_por_s), (100.0, 20.0));
        assert_eq!(i.acerto_cache_pct, Some(25.0));
        assert!(i.latencia_p99_us.unwrap() >= 200_000.0);
        let nomes: Vec<_> = f.alertas.iter().map(|a| (a.indicador, a.nivel)).collect();
        assert!(nomes.contains(&("latencia_p99_us", NivelAlerta::Critico)));
        assert!(nomes.contains(&("acerto_cache_pct", NivelAlerta::Aviso)));
        assert!(nomes.contains(&("fila_escritor", NivelAlerta::Aviso)));
        assert!(nomes.contains(&("conexoes_ativas", NivelAlerta::Critico)));
        assert_eq!(f.series.operacoes_por_s, vec![0.0, 100.0]);
        assert_eq!(f.leituras, 2);
    }

    #[test]
    fn serie_guarda_so_os_ultimos_60() {
        let mut c = Coletor::novo(LimitesAlerta::default());
        let t0 = Instant::now();
        for s in 0..100u64 {
            c.registrar(Leitura {
                instante: t0 + Duration::from_secs(s),
                operacoes: s * 10,
                bytes_banco: s,
                ..Leitura::vazia()
            });
        }
        let f = c.foto();
        assert_eq!(f.series.bytes_banco.len(), PONTOS_SERIE);
        assert_eq!(*f.series.bytes_banco.last().unwrap(), 99.0);
        assert_eq!(f.series.bytes_banco[0], 40.0);
        assert_eq!(f.indicadores.operacoes_por_s, 10.0);
        // Sem leitura de cache: série repete 100 %, indicador fica vazio.
        assert_eq!(f.indicadores.acerto_cache_pct, None);
        assert!(f.series.acerto_cache_pct.iter().all(|&v| v == 100.0));
    }

    #[cfg(feature = "serde")]
    #[test]
    fn vira_json() {
        let mut c = Coletor::novo(LimitesAlerta::default());
        let json = serde_json::to_value(c.registrar(Leitura::vazia())).unwrap();
        assert!(json["indicadores"]["operacoes_por_s"].is_number());
        assert!(json["series"]["conexoes"].is_array());
    }
}
