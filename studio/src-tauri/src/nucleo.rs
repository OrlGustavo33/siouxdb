//! Núcleo do Studio, sem nada de janela: sessão aberta, coleta do painel a cada segundo,
//! verificação e a carga de demonstração. Testável com `cargo test`.

use serde::Serialize;
use siouxdb::armazenamento::{
    PAGINAS_RESERVADAS, Paginador, ParametrosSenha, SenhaNova, TamanhoPagina, TipoPagina,
};
use siouxdb::metricas::{Coletor, FotoPainel, LimitesAlerta};
use siouxdb::vfs::{Vfs, VfsReal};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type Pag = Paginador<<VfsReal as Vfs>::Arquivo>;

/// Páginas guardadas em memória pelo Studio.
const CACHE_PAGINAS: usize = 512;

/// De onde vêm os dados do painel.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Origem {
    /// Arquivo aberto ou criado pela pessoa.
    Arquivo,
    /// Arquivo temporário com carga gerada pelo Studio.
    Demonstracao,
}

/// Dados do arquivo aberto.
#[derive(Debug, Clone, Serialize)]
pub struct Resumo {
    /// De onde vêm os dados.
    pub origem: Origem,
    /// Caminho do arquivo.
    pub caminho: String,
    /// Tamanho de página em KiB.
    pub pagina_kb: u64,
    /// O arquivo é cifrado?
    pub cifrado: bool,
    /// Total de páginas.
    pub paginas: u64,
    /// Geração do cabeçalho.
    pub geracao: u64,
    /// Último LSN confirmado.
    pub ultimo_lsn: u64,
    /// Páginas liberadas aguardando a próxima confirmação.
    pub liberadas_pendentes: usize,
    /// Versão do motor.
    pub versao: &'static str,
}

/// Resultado de uma verificação completa.
#[derive(Debug, Clone, Serialize)]
pub struct Verificacao {
    /// Quando terminou (segundos desde 1970).
    pub quando: u64,
    /// Páginas lidas.
    pub paginas: u64,
    /// Problemas encontrados (vazio = tudo íntegro).
    pub problemas: Vec<String>,
    /// Quanto demorou.
    pub duracao_ms: u64,
}

/// O que o painel recebe a cada segundo.
#[derive(Debug, Clone, Serialize)]
pub struct PainelResposta {
    /// Arquivo aberto.
    pub resumo: Resumo,
    /// Indicadores, séries e alertas.
    pub foto: FotoPainel,
    /// Limites de alerta em uso.
    pub limites: LimitesAlerta,
    /// Última verificação, se houve.
    pub verificacao: Option<Verificacao>,
}

struct Carga {
    parar: Arc<AtomicBool>,
    tarefa: Option<JoinHandle<()>>,
}

impl Drop for Carga {
    fn drop(&mut self) {
        self.parar.store(true, Ordering::Relaxed);
        if let Some(t) = self.tarefa.take() {
            let _ = t.join();
        }
    }
}

struct Sessao {
    origem: Origem,
    caminho: PathBuf,
    paginador: Arc<Pag>,
    coletor: Coletor,
    verificacao: Option<Verificacao>,
    carga: Option<Carga>,
}

impl Drop for Sessao {
    fn drop(&mut self) {
        // Para a carga antes de apagar o arquivo temporário.
        self.carga.take();
        if self.origem == Origem::Demonstracao {
            let _ = std::fs::remove_file(&self.caminho);
        }
    }
}

/// Estado do Studio (um arquivo aberto por vez).
pub struct Studio {
    sessao: Mutex<Option<Sessao>>,
    limites: Mutex<LimitesAlerta>,
}

impl Studio {
    /// Studio sem arquivo aberto.
    pub fn novo() -> Arc<Self> {
        Arc::new(Studio {
            sessao: Mutex::new(None),
            limites: Mutex::new(LimitesAlerta::default()),
        })
    }

    /// Tarefa que alimenta o coletor uma vez por segundo enquanto o Studio existir.
    pub fn iniciar_coleta(studio: &Arc<Studio>) {
        let fraco = Arc::downgrade(studio);
        std::thread::Builder::new()
            .name("coleta-painel".into())
            .spawn(move || {
                while let Some(studio) = fraco.upgrade() {
                    studio.coletar();
                    drop(studio);
                    std::thread::sleep(Duration::from_secs(1));
                }
            })
            .expect("não foi possível iniciar a coleta do painel");
    }

    /// Registra uma leitura no coletor da sessão aberta.
    pub fn coletar(&self) {
        if let Some(s) = self.travar().as_mut() {
            let leitura = s.paginador.leitura_metricas();
            s.coletor.registrar(leitura);
        }
    }

    fn travar(&self) -> MutexGuard<'_, Option<Sessao>> {
        self.sessao.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn limites(&self) -> LimitesAlerta {
        self.limites
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Abre um arquivo existente.
    pub fn abrir(&self, caminho: &str, senha: Option<&str>) -> Result<Resumo, String> {
        let caminho = caminho_valido(caminho)?;
        // O anterior só é fechado se este abrir: um erro não derruba o que estava aberto.
        let senha = senha.filter(|s| !s.is_empty()).map(str::as_bytes);
        let p = Pag::abrir_com(&VfsReal::novo(), &caminho, CACHE_PAGINAS, senha)
            .map_err(|e| e.to_string())?;
        Ok(self.instalar(Origem::Arquivo, caminho, p, None))
    }

    /// Cria um arquivo novo (recusa se já existir). Com `senha`, cifrado; `None` = em claro.
    pub fn criar(&self, caminho: &str, kb: u32, senha: Option<&str>) -> Result<Resumo, String> {
        let caminho = caminho_valido(caminho)?;
        let tamanho = TamanhoPagina::de_bytes(kb.saturating_mul(1024))
            .ok_or_else(|| format!("tamanho de página inválido: {kb} KiB (use 4, 8, 16 ou 32)"))?;
        if let Some(s) = senha
            && s.chars().count() < 8
        {
            return Err("a senha precisa de pelo menos 8 caracteres".into());
        }
        let senha = senha.map(|s| SenhaNova {
            senha: s.as_bytes(),
            parametros: ParametrosSenha::default(),
        });
        let p = Pag::criar_com(&VfsReal::novo(), &caminho, tamanho, CACHE_PAGINAS, senha)
            .map_err(|e| e.to_string())?;
        Ok(self.instalar(Origem::Arquivo, caminho, p, None))
    }

    /// Cria um banco temporário e gera carga nele, para ver o painel funcionando.
    pub fn demonstracao(&self) -> Result<Resumo, String> {
        self.fechar();
        let caminho =
            std::env::temp_dir().join(format!("siouxdb-studio-demo-{}.sdb", std::process::id()));
        // O nome é só nosso (leva o PID): sobra de uma execução anterior pode ser apagada.
        let _ = std::fs::remove_file(&caminho);
        // Cifrado como um banco de verdade (mede o custo real da cifra), com senha sorteada
        // e Argon2id leve: o arquivo é temporário e apagado ao fechar.
        let senha = format!("demo-{}-{:?}", std::process::id(), Instant::now());
        let senha = SenhaNova {
            senha: senha.as_bytes(),
            parametros: ParametrosSenha::para_testes(),
        };
        let p = Pag::criar_com(
            &VfsReal::novo(),
            &caminho,
            TamanhoPagina::K4,
            256,
            Some(senha),
        )
        .map_err(|e| e.to_string())?;
        let p = Arc::new(p);
        let parar = Arc::new(AtomicBool::new(false));
        let tarefa = {
            let (p, parar) = (Arc::clone(&p), Arc::clone(&parar));
            std::thread::Builder::new()
                .name("carga-demonstracao".into())
                .spawn(move || gerar_carga(&p, &parar))
                .map_err(|e| e.to_string())?
        };
        let carga = Carga {
            parar,
            tarefa: Some(tarefa),
        };
        Ok(self.instalar_arc(Origem::Demonstracao, caminho, p, Some(carga)))
    }

    fn instalar(&self, origem: Origem, caminho: PathBuf, p: Pag, carga: Option<Carga>) -> Resumo {
        self.instalar_arc(origem, caminho, Arc::new(p), carga)
    }

    fn instalar_arc(
        &self,
        origem: Origem,
        caminho: PathBuf,
        p: Arc<Pag>,
        carga: Option<Carga>,
    ) -> Resumo {
        let mut coletor = Coletor::novo(self.limites());
        coletor.registrar(p.leitura_metricas());
        let sessao = Sessao {
            origem,
            caminho,
            paginador: p,
            coletor,
            verificacao: None,
            carga,
        };
        let resumo = resumo(&sessao);
        let anterior = self.travar().replace(sessao);
        drop(anterior); // fora da trava: pode parar a carga e apagar o temporário
        resumo
    }

    /// Fecha o arquivo aberto (e para a demonstração, apagando o arquivo temporário).
    pub fn fechar(&self) {
        let antiga = self.travar().take();
        drop(antiga);
    }

    /// Foto atual do painel, se houver arquivo aberto.
    pub fn painel(&self) -> Option<PainelResposta> {
        let limites = self.limites();
        self.travar().as_ref().map(|s| PainelResposta {
            resumo: resumo(s),
            foto: s.coletor.foto().clone(),
            limites,
            verificacao: s.verificacao.clone(),
        })
    }

    /// Lê e confere todas as páginas do arquivo aberto.
    pub fn verificar(&self) -> Result<Verificacao, String> {
        // Não segura a trava da sessão durante a leitura: o painel continua atualizando.
        let p = self
            .travar()
            .as_ref()
            .map(|s| Arc::clone(&s.paginador))
            .ok_or("nenhum arquivo aberto")?;
        let inicio = Instant::now();
        let total = p.cabecalho().total_paginas;
        let mut problemas = Vec::new();
        for n in PAGINAS_RESERVADAS..total {
            if let Err(e) = p.ler(n)
                && problemas.len() < 100
            {
                problemas.push(e.to_string());
            }
        }
        let v = Verificacao {
            quando: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            paginas: total,
            problemas,
            duracao_ms: inicio.elapsed().as_millis() as u64,
        };
        if let Some(s) = self.travar().as_mut()
            && Arc::ptr_eq(&s.paginador, &p)
        {
            s.verificacao = Some(v.clone());
        }
        Ok(v)
    }

    /// Troca os limites de alerta (para a sessão atual e as próximas).
    pub fn definir_limites(&self, limites: LimitesAlerta) {
        *self.limites.lock().unwrap_or_else(|e| e.into_inner()) = limites.clone();
        if let Some(s) = self.travar().as_mut() {
            s.coletor.definir_limites(limites);
        }
    }
}

fn caminho_valido(caminho: &str) -> Result<PathBuf, String> {
    let c = caminho.trim();
    if c.is_empty() {
        return Err("informe o caminho do arquivo".into());
    }
    Ok(Path::new(c).to_path_buf())
}

fn resumo(s: &Sessao) -> Resumo {
    let c = s.paginador.cabecalho();
    Resumo {
        origem: s.origem.clone(),
        caminho: s.caminho.display().to_string(),
        pagina_kb: c.tamanho_pagina.bytes() as u64 / 1024,
        cifrado: s.paginador.cifrado(),
        paginas: c.total_paginas,
        geracao: c.geracao,
        ultimo_lsn: c.ultimo_lsn,
        liberadas_pendentes: s.paginador.estatisticas().liberadas_pendentes,
        versao: siouxdb::VERSAO,
    }
}

/// Gerador pseudoaleatório simples (xorshift), só para a demonstração.
struct Sorteio(u64);

impl Sorteio {
    fn proximo(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn ate(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.proximo() % n }
    }
}

/// Carga de demonstração: ondas de leitura e gravação com picos de vez em quando, sobre
/// um arquivo que cresce até ~16 MiB e depois passa a reaproveitar páginas.
fn gerar_carga(p: &Pag, parar: &AtomicBool) {
    const LIMITE_PAGINAS: usize = 4000;
    let mut sorteio = Sorteio(0x9E37_79B9_7F4A_7C15 ^ u64::from(std::process::id()));
    let mut vivas: Vec<u64> = Vec::new();
    let inicio = Instant::now();
    let mut desde_confirmar = 0u32;
    while !parar.load(Ordering::Relaxed) {
        let t = inicio.elapsed().as_secs_f64();
        // Onda de ~40 s entre 300 e 3.000 operações por fatia de 50 ms, com pico raro.
        let onda = 0.5 + 0.5 * (t * std::f64::consts::TAU / 40.0).sin();
        let pico = if sorteio.ate(200) == 0 { 4.0 } else { 1.0 };
        let ops = ((15.0 + 135.0 * onda) * pico) as u32;
        let fatia = Instant::now();
        for _ in 0..ops {
            let r = sorteio.ate(100);
            let resultado = if r < 25 || vivas.len() < 64 {
                if vivas.len() >= LIMITE_PAGINAS {
                    let i = sorteio.ate(vivas.len() as u64) as usize;
                    let n = vivas.swap_remove(i);
                    p.liberar(n).map(|_| ())
                } else {
                    p.alocar(TipoPagina::NoArvore).and_then(|n| {
                        let mut pag = p.pagina_nova(TipoPagina::NoArvore);
                        let marca = sorteio.proximo().to_le_bytes();
                        pag.conteudo_mut()[..8].copy_from_slice(&marca);
                        vivas.push(n);
                        p.gravar(n, pag)
                    })
                }
            } else if r < 97 {
                // Leituras concentradas nas páginas mais novas (cache acerta a maioria).
                let tam = vivas.len() as u64;
                let quente = sorteio.ate(10) < 8;
                let i = if quente {
                    tam - 1 - sorteio.ate(tam.min(200))
                } else {
                    sorteio.ate(tam)
                };
                p.ler(vivas[i as usize]).map(|_| ())
            } else {
                let i = sorteio.ate(vivas.len() as u64) as usize;
                let n = vivas.swap_remove(i);
                p.liberar(n)
            };
            if resultado.is_err() {
                return; // erro de disco: a demonstração para (o painel mostra o que houve até aqui)
            }
            desde_confirmar += 1;
            if desde_confirmar >= 400 {
                desde_confirmar = 0;
                if p.confirmar().is_err() {
                    return;
                }
            }
        }
        if let Some(resto) = Duration::from_millis(50).checked_sub(fatia.elapsed()) {
            std::thread::sleep(resto);
        }
    }
    let _ = p.confirmar();
}

#[cfg(test)]
mod testes {
    use super::*;

    fn pasta() -> PathBuf {
        let d = std::env::temp_dir().join(format!("siouxdb-studio-teste-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn criar_abrir_verificar_e_painel() {
        let studio = Studio::novo();
        assert!(studio.painel().is_none());
        assert!(studio.verificar().is_err());
        let arq = pasta().join("criar.sdb");
        let _ = std::fs::remove_file(&arq);
        let caminho = arq.display().to_string();
        assert!(studio.criar(&caminho, 3, None).is_err());
        assert!(studio.criar(&caminho, 16, Some("curta")).is_err());
        let r = studio.criar(&caminho, 16, Some("senha-de-teste")).unwrap();
        assert!(r.cifrado);
        assert_eq!((r.pagina_kb, r.paginas), (16, 2));
        assert!(
            studio.criar(&caminho, 16, None).is_err(),
            "não pode criar por cima"
        );
        studio.coletar();
        let v = studio.verificar().unwrap();
        assert!(v.problemas.is_empty());
        let painel = studio.painel().unwrap();
        assert_eq!(painel.resumo.origem, Origem::Arquivo);
        assert!(painel.verificacao.is_some());
        assert_eq!(painel.foto.leituras, 2);
        let json = serde_json::to_value(&painel).unwrap();
        assert_eq!(json["resumo"]["origem"], "arquivo");
        assert!(json["foto"]["series"]["operacoes_por_s"].is_array());

        studio.fechar();
        assert!(studio.painel().is_none());
        let sem = studio.abrir(&caminho, None).unwrap_err();
        assert!(sem.contains("senha"), "{sem}");
        assert!(studio.abrir(&caminho, Some("errada-123")).is_err());
        assert_eq!(
            studio
                .abrir(&caminho, Some("senha-de-teste"))
                .unwrap()
                .paginas,
            2
        );
        assert!(studio.abrir("  ", None).is_err());
        studio.fechar();
        std::fs::remove_file(&arq).unwrap();
    }

    #[test]
    fn demonstracao_gera_carga_e_limpa_ao_fechar() {
        let studio = Studio::novo();
        let r = studio.demonstracao().unwrap();
        assert_eq!(r.origem, Origem::Demonstracao);
        assert!(r.cifrado);
        std::thread::sleep(Duration::from_millis(600));
        studio.coletar();
        let foto = studio.painel().unwrap().foto;
        assert!(
            foto.indicadores.operacoes_por_s > 0.0,
            "{:?}",
            foto.indicadores
        );
        assert!(foto.indicadores.latencia_p99_us.is_some());
        let v = studio.verificar().unwrap();
        assert!(v.problemas.is_empty(), "{:?}", v.problemas);
        studio.definir_limites(LimitesAlerta {
            latencia_p99_us: 0.001,
            ..LimitesAlerta::default()
        });
        std::thread::sleep(Duration::from_millis(200));
        studio.coletar();
        assert!(
            studio
                .painel()
                .unwrap()
                .foto
                .alertas
                .iter()
                .any(|a| a.indicador == "latencia_p99_us")
        );
        studio.fechar();
        assert!(
            !Path::new(&r.caminho).exists(),
            "arquivo temporário deveria ter sido apagado"
        );
    }
}
