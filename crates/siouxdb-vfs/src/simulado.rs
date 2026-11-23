//! Disco simulado em memória, determinístico, para testar o motor contra quedas de energia.
//!
//! Cada arquivo guarda duas imagens:
//! - **durável**: o que já passou por [`Arquivo::sincronizar`] e sobrevive a qualquer queda;
//! - **atual**: o que os leitores veem agora (durável + gravações pendentes).
//!
//! [`VfsSimulado::queda`] simula a energia caindo: de cada arquivo sobrevive o durável e
//! um *prefixo* sorteado das gravações pendentes, e a primeira gravação perdida pode ficar
//! **cortada no meio** (em múltiplos de um setor de 512 bytes), como num disco real.
//! Tudo é sorteado por uma semente: a mesma semente reproduz exatamente o mesmo cenário.
//!
//! Simplificações (documentadas de propósito):
//! - criar, apagar e renomear arquivo valem na hora (como se a pasta fosse sincronizada sempre);
//! - um `sincronizar` que falha (falha injetada) não grava nada e mantém tudo pendente.

use crate::{Arquivo, ErroVfs, ModoAbertura, Resultado, Vfs};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// Tamanho do setor usado para cortar gravações numa queda.
pub const SETOR: usize = 512;

/// Parâmetros do disco simulado.
#[derive(Debug, Clone)]
pub struct ConfigSimulacao {
    /// Semente do sorteio. Mesma semente, mesmo resultado.
    pub semente: u64,
    /// Capacidade total do disco em bytes (`None` = sem limite).
    pub capacidade: Option<u64>,
    /// Chance, em milésimos, de um `sincronizar` falhar (0 = nunca, 1000 = sempre).
    pub falha_sincronizar_por_mil: u32,
}

impl Default for ConfigSimulacao {
    fn default() -> Self {
        ConfigSimulacao {
            semente: 1,
            capacidade: None,
            falha_sincronizar_por_mil: 0,
        }
    }
}

#[derive(Debug, Clone)]
enum Pendente {
    Escrita { posicao: u64, dados: Vec<u8> },
    Truncar(u64),
}

#[derive(Debug, Clone, Default)]
struct ArquivoSim {
    duravel: Vec<u8>,
    atual: Vec<u8>,
    pendentes: Vec<Pendente>,
}

#[derive(Debug)]
struct Estado {
    arquivos: HashMap<PathBuf, ArquivoSim>,
    sorteio: SplitMix64,
    config: ConfigSimulacao,
    /// Aumenta a cada queda: arquivos abertos antes dela deixam de valer.
    geracao: u64,
    /// Arquivos travados com exclusividade (caminho → id da abertura).
    travados: HashMap<PathBuf, u64>,
    proxima_abertura: u64,
}

/// Disco simulado. Clonar compartilha o mesmo disco.
#[derive(Debug, Clone)]
pub struct VfsSimulado {
    estado: Arc<Mutex<Estado>>,
}

/// Arquivo aberto no disco simulado.
#[derive(Debug)]
pub struct ArquivoSimulado {
    estado: Arc<Mutex<Estado>>,
    caminho: PathBuf,
    somente_leitura: bool,
    geracao: u64,
    id: u64,
}

impl Drop for ArquivoSimulado {
    fn drop(&mut self) {
        // Fechar solta a trava, como no disco real.
        let mut e = self.estado.lock().unwrap_or_else(|e| e.into_inner());
        if e.travados.get(&self.caminho) == Some(&self.id) {
            e.travados.remove(&self.caminho);
        }
    }
}

/// Gerador pseudoaleatório pequeno e determinístico (SplitMix64).
#[derive(Debug, Clone)]
struct SplitMix64(u64);

impl SplitMix64 {
    fn proximo(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Número em `0..=maximo`.
    fn ate(&mut self, maximo: u64) -> u64 {
        if maximo == u64::MAX {
            self.proximo()
        } else {
            self.proximo() % (maximo + 1)
        }
    }
}

fn aplicar(imagem: &mut Vec<u8>, p: &Pendente) {
    match p {
        Pendente::Escrita { posicao, dados } => {
            let ini = *posicao as usize;
            let fim = ini + dados.len();
            if imagem.len() < fim {
                imagem.resize(fim, 0);
            }
            imagem[ini..fim].copy_from_slice(dados);
        }
        Pendente::Truncar(t) => imagem.resize(*t as usize, 0),
    }
}

impl VfsSimulado {
    /// Disco simulado vazio.
    pub fn novo(config: ConfigSimulacao) -> Self {
        let sorteio = SplitMix64(config.semente);
        VfsSimulado {
            estado: Arc::new(Mutex::new(Estado {
                arquivos: HashMap::new(),
                sorteio,
                config,
                geracao: 0,
                travados: HashMap::new(),
                proxima_abertura: 0,
            })),
        }
    }

    fn travar(&self) -> MutexGuard<'_, Estado> {
        // Teste que entrou em pânico com a trava não deve esconder o erro original.
        self.estado.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Simula uma queda de energia. Depois dela, arquivos abertos antes não podem mais ser
    /// usados (é como reiniciar o processo): abra de novo.
    pub fn queda(&self) {
        let mut e = self.travar();
        e.travados.clear(); // o processo "reiniciou": travas somem
        let mut caminhos: Vec<PathBuf> = e.arquivos.keys().cloned().collect();
        caminhos.sort(); // ordem fixa: o sorteio não pode depender da ordem do HashMap
        for c in caminhos {
            let pendentes = e.arquivos[&c].pendentes.len() as u64;
            let sobrevivem = e.sorteio.ate(pendentes) as usize;
            let cortar_proxima = e.sorteio.ate(1) == 1;
            let setores_cortados = e.sorteio.proximo();
            let arq = e.arquivos.get_mut(&c).expect("caminho listado acima");
            let mut imagem = std::mem::take(&mut arq.duravel);
            for p in &arq.pendentes[..sobrevivem] {
                aplicar(&mut imagem, p);
            }
            // A gravação seguinte pode ter chegado ao disco só em parte (setores iniciais).
            if cortar_proxima {
                if let Some(Pendente::Escrita { posicao, dados }) = arq.pendentes.get(sobrevivem) {
                    let setores = dados.len().div_ceil(SETOR) as u64;
                    let quantos = (setores_cortados % (setores + 1)) as usize;
                    let ate = (quantos * SETOR).min(dados.len());
                    if ate > 0 {
                        aplicar(
                            &mut imagem,
                            &Pendente::Escrita {
                                posicao: *posicao,
                                dados: dados[..ate].to_vec(),
                            },
                        );
                    }
                }
            }
            arq.atual = imagem.clone();
            arq.duravel = imagem;
            arq.pendentes.clear();
        }
        e.geracao += 1;
    }

    /// Queda em que **nenhuma** gravação pendente sobrevive (o pior caso, útil em testes).
    pub fn queda_perdendo_tudo(&self) {
        let mut e = self.travar();
        e.travados.clear(); // o processo "reiniciou": travas somem
        for arq in e.arquivos.values_mut() {
            arq.atual = arq.duravel.clone();
            arq.pendentes.clear();
        }
        e.geracao += 1;
    }

    /// Bytes ocupados agora (soma do tamanho atual de todos os arquivos).
    pub fn bytes_usados(&self) -> u64 {
        self.travar()
            .arquivos
            .values()
            .map(|a| a.atual.len() as u64)
            .sum()
    }
}

impl Vfs for VfsSimulado {
    type Arquivo = ArquivoSimulado;

    fn abrir(&self, caminho: &Path, modo: ModoAbertura) -> Resultado<ArquivoSimulado> {
        let mut e = self.travar();
        let existe = e.arquivos.contains_key(caminho);
        match modo {
            ModoAbertura::Leitura | ModoAbertura::LeituraGravacao if !existe => {
                return Err(ErroVfs::NaoEncontrado(caminho.to_path_buf()));
            }
            ModoAbertura::CriarNovo if existe => {
                return Err(ErroVfs::JaExiste(caminho.to_path_buf()));
            }
            ModoAbertura::CriarNovo | ModoAbertura::CriarSeNaoExistir if !existe => {
                e.arquivos
                    .insert(caminho.to_path_buf(), ArquivoSim::default());
            }
            _ => {}
        }
        e.proxima_abertura += 1;
        Ok(ArquivoSimulado {
            estado: Arc::clone(&self.estado),
            caminho: caminho.to_path_buf(),
            somente_leitura: modo == ModoAbertura::Leitura,
            geracao: e.geracao,
            id: e.proxima_abertura,
        })
    }

    fn existe(&self, caminho: &Path) -> Resultado<bool> {
        Ok(self.travar().arquivos.contains_key(caminho))
    }

    fn apagar(&self, caminho: &Path) -> Resultado<()> {
        self.travar()
            .arquivos
            .remove(caminho)
            .map(|_| ())
            .ok_or_else(|| ErroVfs::NaoEncontrado(caminho.to_path_buf()))
    }

    fn renomear(&self, de: &Path, para: &Path) -> Resultado<()> {
        let mut e = self.travar();
        let arq = e
            .arquivos
            .remove(de)
            .ok_or_else(|| ErroVfs::NaoEncontrado(de.to_path_buf()))?;
        e.arquivos.insert(para.to_path_buf(), arq);
        Ok(())
    }
}

impl ArquivoSimulado {
    fn com_arquivo<T>(
        &self,
        f: impl FnOnce(&mut Estado, &PathBuf) -> Resultado<T>,
    ) -> Resultado<T> {
        let mut e = self.estado.lock().unwrap_or_else(|e| e.into_inner());
        if e.geracao != self.geracao {
            return Err(ErroVfs::FalhaSimulada(
                "arquivo aberto antes de uma queda de energia",
            ));
        }
        if !e.arquivos.contains_key(&self.caminho) {
            return Err(ErroVfs::NaoEncontrado(self.caminho.clone()));
        }
        f(&mut e, &self.caminho)
    }

    fn registrar(&self, p: Pendente) -> Resultado<()> {
        if self.somente_leitura {
            return Err(ErroVfs::SomenteLeitura);
        }
        self.com_arquivo(|e, c| {
            if let (Some(cap), Pendente::Escrita { posicao, dados }) = (e.config.capacidade, &p) {
                let tamanho_atual = e.arquivos[c].atual.len() as u64;
                let novo_tamanho = tamanho_atual.max(posicao + dados.len() as u64);
                let usado: u64 = e.arquivos.values().map(|a| a.atual.len() as u64).sum();
                if usado - tamanho_atual + novo_tamanho > cap {
                    return Err(ErroVfs::DiscoCheio);
                }
            }
            let arq = e.arquivos.get_mut(c).expect("conferido em com_arquivo");
            aplicar(&mut arq.atual, &p);
            arq.pendentes.push(p);
            Ok(())
        })
    }
}

impl Arquivo for ArquivoSimulado {
    fn ler_em(&self, posicao: u64, destino: &mut [u8]) -> Resultado<usize> {
        self.com_arquivo(|e, c| {
            let atual = &e.arquivos[c].atual;
            let ini = (posicao as usize).min(atual.len());
            let n = destino.len().min(atual.len() - ini);
            destino[..n].copy_from_slice(&atual[ini..ini + n]);
            Ok(n)
        })
    }

    fn gravar_em(&self, posicao: u64, dados: &[u8]) -> Resultado<()> {
        self.registrar(Pendente::Escrita {
            posicao,
            dados: dados.to_vec(),
        })
    }

    fn travar_exclusivo(&self) -> Resultado<()> {
        let id = self.id;
        self.com_arquivo(|e, c| match e.travados.get(c) {
            Some(&dono) if dono != id => Err(ErroVfs::EmUso(c.clone())),
            _ => {
                e.travados.insert(c.clone(), id);
                Ok(())
            }
        })
    }

    fn sincronizar(&self) -> Resultado<()> {
        if self.somente_leitura {
            return Ok(());
        }
        self.com_arquivo(|e, c| {
            let chance = e.config.falha_sincronizar_por_mil as u64;
            if chance > 0 && e.sorteio.ate(999) < chance {
                return Err(ErroVfs::FalhaSimulada("sincronizar falhou"));
            }
            let arq = e.arquivos.get_mut(c).expect("conferido em com_arquivo");
            arq.duravel = arq.atual.clone();
            arq.pendentes.clear();
            Ok(())
        })
    }

    fn tamanho(&self) -> Resultado<u64> {
        self.com_arquivo(|e, c| Ok(e.arquivos[c].atual.len() as u64))
    }

    fn truncar(&self, tamanho: u64) -> Resultado<()> {
        self.registrar(Pendente::Truncar(tamanho))
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    fn conteudo(vfs: &VfsSimulado, c: &str) -> Vec<u8> {
        let a = vfs.abrir(Path::new(c), ModoAbertura::Leitura).unwrap();
        let mut buf = vec![0u8; a.tamanho().unwrap() as usize];
        a.ler_exato_em(0, &mut buf).unwrap();
        buf
    }

    #[test]
    fn trava_some_ao_fechar_e_na_queda() {
        let vfs = VfsSimulado::novo(ConfigSimulacao::default());
        let a = vfs.abrir(Path::new("t"), ModoAbertura::CriarNovo).unwrap();
        a.travar_exclusivo().unwrap();
        a.travar_exclusivo().unwrap(); // a mesma abertura pode repetir
        let b = vfs
            .abrir(Path::new("t"), ModoAbertura::LeituraGravacao)
            .unwrap();
        assert!(matches!(b.travar_exclusivo(), Err(ErroVfs::EmUso(_))));
        drop(a);
        b.travar_exclusivo().unwrap();
        vfs.queda();
        let c = vfs
            .abrir(Path::new("t"), ModoAbertura::LeituraGravacao)
            .unwrap();
        c.travar_exclusivo().unwrap();
        drop(b); // abertura de antes da queda não solta a trava de depois
        let d = vfs
            .abrir(Path::new("t"), ModoAbertura::LeituraGravacao)
            .unwrap();
        assert!(d.travar_exclusivo().is_err());
    }

    #[test]
    fn sincronizado_sobrevive_e_pendente_some() {
        let vfs = VfsSimulado::novo(ConfigSimulacao::default());
        let a = vfs.abrir(Path::new("a"), ModoAbertura::CriarNovo).unwrap();
        a.gravar_em(0, b"confirmado").unwrap();
        a.sincronizar().unwrap();
        a.gravar_em(0, b"PENDENTE!!").unwrap();
        assert_eq!(
            conteudo(&vfs, "a"),
            b"PENDENTE!!",
            "antes da queda o leitor vê o pendente"
        );
        vfs.queda_perdendo_tudo();
        assert_eq!(conteudo(&vfs, "a"), b"confirmado");
        assert!(
            matches!(a.tamanho(), Err(ErroVfs::FalhaSimulada(_))),
            "arquivo aberto antes da queda não vale mais"
        );
    }

    #[test]
    fn queda_mantem_so_prefixo_das_gravacoes_e_pode_cortar_a_seguinte() {
        // Durável: 4 setores de 'A'. Pendentes: g1 = 'B' nos 4 setores, g2 = 'C' no 1º setor.
        // Resultado válido: um prefixo de [g1, g2] aplicado, mais a próxima talvez cortada por setor.
        let mut vistos = std::collections::BTreeSet::new();
        for semente in 0..300 {
            let vfs = VfsSimulado::novo(ConfigSimulacao {
                semente,
                ..Default::default()
            });
            let a = vfs.abrir(Path::new("a"), ModoAbertura::CriarNovo).unwrap();
            a.gravar_em(0, &[b'A'; 4 * SETOR]).unwrap();
            a.sincronizar().unwrap();
            a.gravar_em(0, &[b'B'; 4 * SETOR]).unwrap();
            a.gravar_em(0, &[b'C'; SETOR]).unwrap();
            vfs.queda();
            let c = conteudo(&vfs, "a");
            assert_eq!(c.len(), 4 * SETOR);
            // Cada setor é inteiro de uma letra só (corte só na fronteira de setor).
            let setores: Vec<u8> = c
                .chunks(SETOR)
                .map(|s| {
                    assert!(
                        s.iter().all(|&b| b == s[0]),
                        "setor misturado: corte fora da fronteira"
                    );
                    s[0]
                })
                .collect();
            let valido = match setores.as_slice() {
                // nada sobreviveu, ou g1 cortado em k setores
                [b'A', b'A', b'A', b'A']
                | [b'B', b'A', b'A', b'A']
                | [b'B', b'B', b'A', b'A']
                | [b'B', b'B', b'B', b'A'] => true,
                // g1 inteiro; g2 perdido, cortado ou aplicado
                [b'B', b'B', b'B', b'B'] | [b'C', b'B', b'B', b'B'] => true,
                _ => false,
            };
            assert!(
                valido,
                "semente {semente}: estado impossível {:?}",
                String::from_utf8_lossy(&setores)
            );
            vistos.insert(setores);
        }
        assert!(
            vistos.len() >= 4,
            "o sorteio deveria produzir cenários variados, viu {}",
            vistos.len()
        );
    }

    #[test]
    fn mesma_semente_mesmo_resultado() {
        let rodar = |semente| {
            let vfs = VfsSimulado::novo(ConfigSimulacao {
                semente,
                ..Default::default()
            });
            for i in 0..5u8 {
                let a = vfs
                    .abrir(Path::new(&format!("f{i}")), ModoAbertura::CriarNovo)
                    .unwrap();
                for j in 0..20u64 {
                    a.gravar_em(j * 100, &[i.wrapping_add(j as u8); 700])
                        .unwrap();
                }
            }
            vfs.queda();
            (0..5)
                .map(|i| conteudo(&vfs, &format!("f{i}")))
                .collect::<Vec<_>>()
        };
        assert_eq!(rodar(42), rodar(42));
    }

    #[test]
    fn disco_cheio_e_falha_de_sincronizar() {
        let vfs = VfsSimulado::novo(ConfigSimulacao {
            capacidade: Some(1000),
            ..Default::default()
        });
        let a = vfs.abrir(Path::new("a"), ModoAbertura::CriarNovo).unwrap();
        a.gravar_em(0, &[1; 900]).unwrap();
        assert!(matches!(
            a.gravar_em(900, &[1; 200]),
            Err(ErroVfs::DiscoCheio)
        ));
        a.gravar_em(0, &[2; 1000]).unwrap(); // sobrescrever dentro do espaço cabe

        let vfs = VfsSimulado::novo(ConfigSimulacao {
            falha_sincronizar_por_mil: 1000,
            ..Default::default()
        });
        let a = vfs.abrir(Path::new("a"), ModoAbertura::CriarNovo).unwrap();
        a.gravar_em(0, b"x").unwrap();
        assert!(matches!(a.sincronizar(), Err(ErroVfs::FalhaSimulada(_))));
        vfs.queda_perdendo_tudo();
        assert!(
            conteudo(&vfs, "a").is_empty(),
            "sincronizar que falhou não garante nada"
        );
    }

    #[test]
    fn renomear_e_atomico() {
        let vfs = VfsSimulado::novo(ConfigSimulacao::default());
        let a = vfs
            .abrir(Path::new("tmp"), ModoAbertura::CriarNovo)
            .unwrap();
        a.gravar_em(0, b"novo").unwrap();
        a.sincronizar().unwrap();
        vfs.abrir(Path::new("final"), ModoAbertura::CriarNovo)
            .unwrap();
        vfs.renomear(Path::new("tmp"), Path::new("final")).unwrap();
        vfs.queda();
        assert!(!vfs.existe(Path::new("tmp")).unwrap());
        assert_eq!(conteudo(&vfs, "final"), b"novo");
    }
}
