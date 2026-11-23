//! Manutenção automática: compactação do histórico e backup periódico.
//!
//! ```no_run
//! # use std::sync::Arc;
//! # fn f(banco: Arc<siouxdb::BancoArquivo>) {
//! let cfg = siouxdb::manutencao::ConfigManutencao {
//!     backup: Some(siouxdb::manutencao::ConfigBackup::em("/var/backups/meu-banco")),
//!     ..Default::default()
//! };
//! let _manutencao = siouxdb::manutencao::iniciar(banco, cfg); // para ao ser solta
//! # }
//! ```
//!
//! - **Compactação:** uma tabela é compactada quando tem muitas versões por linha. O
//!   histórico das últimas `manter_confirmacoes` confirmações é preservado (leituras
//!   `AS OF` nesse intervalo continuam exatas).
//! - **Backup:** cópia a quente numa pasta, com rodízio (ficam as `manter` mais novas). Se a
//!   senha for informada, cada cópia é **aberta e lida inteira** antes de valer; cópia que
//!   não abre é apagada e vira erro no estado.

use crate::BancoArquivo;
use crate::catalogo::{ErroCatalogo, ResumoCompactacao, catalogo};
use crate::transacoes::Config;
use crate::vfs::VfsReal;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Resultado das operações de manutenção.
pub type Resultado<T> = Result<T, ErroCatalogo>;

/// Backup periódico.
#[derive(Clone)]
pub struct ConfigBackup {
    /// Pasta das cópias (criada se não existir). De preferência em outro disco.
    pub pasta: PathBuf,
    /// De quanto em quanto tempo.
    pub intervalo: Duration,
    /// Quantas cópias ficam (as mais novas).
    pub manter: usize,
    /// Senha do banco, para abrir e conferir cada cópia depois de feita. Num banco cifrado,
    /// sem ela a cópia não é testada; um banco em claro é sempre testado.
    pub senha_para_testar: Option<Vec<u8>>,
}

impl ConfigBackup {
    /// Uma cópia por dia, guardando 7, na pasta dada.
    pub fn em(pasta: impl Into<PathBuf>) -> Self {
        ConfigBackup {
            pasta: pasta.into(),
            intervalo: Duration::from_secs(24 * 3600),
            manter: 7,
            senha_para_testar: None,
        }
    }
}

impl std::fmt::Debug for ConfigBackup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigBackup")
            .field("pasta", &self.pasta)
            .field("intervalo", &self.intervalo)
            .field("manter", &self.manter)
            .field(
                "senha_para_testar",
                &self.senha_para_testar.as_ref().map(|_| "***"),
            )
            .finish()
    }
}

/// O que a manutenção faz e quando.
#[derive(Debug, Clone)]
pub struct ConfigManutencao {
    /// De quanto em quanto tempo conferir as tabelas.
    pub intervalo: Duration,
    /// Compacta a tabela quando versões ÷ linhas passa disto.
    pub versoes_por_linha: f64,
    /// Só compacta tabelas com pelo menos esta quantidade de versões.
    pub versoes_minimas: u64,
    /// Histórico preservado: as últimas N confirmações do banco.
    pub manter_confirmacoes: u64,
    /// Backup periódico (`None` = não faz).
    pub backup: Option<ConfigBackup>,
}

impl Default for ConfigManutencao {
    fn default() -> Self {
        ConfigManutencao {
            intervalo: Duration::from_secs(600),
            versoes_por_linha: 4.0,
            versoes_minimas: 10_000,
            manter_confirmacoes: 10_000,
            backup: None,
        }
    }
}

/// O que aconteceu até agora.
#[derive(Debug, Clone, Default)]
pub struct EstadoManutencao {
    /// Compactações feitas.
    pub compactacoes: u64,
    /// Versões removidas por elas.
    pub versoes_removidas: u64,
    /// Backups feitos com sucesso.
    pub backups: u64,
    /// Último backup bom.
    pub ultimo_backup: Option<PathBuf>,
    /// Último erro (compactação ou backup), se houve.
    pub ultimo_erro: Option<String>,
}

/// Compacta as tabelas que passaram do limite. Devolve o que foi feito em cada uma.
pub fn compactar_se_preciso(
    banco: &BancoArquivo,
    cfg: &ConfigManutencao,
) -> Resultado<Vec<(String, ResumoCompactacao)>> {
    let candidatas: Vec<String> = {
        let l = banco.ler()?;
        let mut v = Vec::new();
        for t in catalogo::tabelas(&l)? {
            let c = t.contagem(&l)?;
            if c.versoes >= cfg.versoes_minimas
                && c.versoes as f64 > c.linhas.max(1) as f64 * cfg.versoes_por_linha
            {
                v.push(t.def().nome.clone());
            }
        }
        v
    };
    let mut feitas = Vec::new();
    for nome in candidatas {
        // Uma transação por tabela: os escritores só esperam uma tabela de cada vez.
        let mut tx = banco.escrever()?;
        let Some(t) = catalogo::tabela(&tx, &nome)? else {
            continue;
        };
        let manter_desde = tx.lsn().saturating_sub(cfg.manter_confirmacoes);
        let r = t.compactar(&mut tx, manter_desde)?;
        tx.confirmar()?;
        feitas.push((nome, r));
    }
    if !feitas.is_empty() {
        banco.checkpoint()?;
    }
    Ok(feitas)
}

fn erro_io(e: std::io::Error) -> ErroCatalogo {
    ErroCatalogo::Banco(crate::vfs::ErroVfs::Io(e).into())
}

fn wal_de(caminho: &Path) -> PathBuf {
    let mut s = caminho.as_os_str().to_owned();
    s.push("-wal");
    PathBuf::from(s)
}

/// Abre a cópia e lê todas as páginas dela. Erro se algo não conferir.
fn testar_copia(copia: &Path, senha: Option<&[u8]>) -> Resultado<()> {
    let b = BancoArquivo::abrir(&VfsReal::novo(), copia, senha, Config::default())?;
    let l = b.ler()?;
    for n in crate::armazenamento::PAGINAS_RESERVADAS..l.meta().total_paginas {
        l.pagina(n)?;
    }
    for t in catalogo::tabelas(&l)? {
        t.verificar_indice(&l)?;
    }
    Ok(())
}

/// Faz um backup agora, testa (se der) e aplica o rodízio. Devolve o arquivo da cópia.
pub fn fazer_backup(banco: &BancoArquivo, cfg: &ConfigBackup) -> Resultado<PathBuf> {
    std::fs::create_dir_all(&cfg.pasta).map_err(erro_io)?;
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    let destino = cfg.pasta.join(format!("backup-{micros:020}.sdb"));
    banco.copiar_para(&VfsReal::novo(), &destino)?;
    // Banco em claro testa sem senha; cifrado só com a senha informada.
    if !banco.paginador().cifrado() || cfg.senha_para_testar.is_some() {
        if let Err(e) = testar_copia(&destino, cfg.senha_para_testar.as_deref()) {
            let _ = std::fs::remove_file(&destino);
            let _ = std::fs::remove_file(wal_de(&destino));
            return Err(e);
        }
    }
    // Rodízio: o nome tem a data com zeros à esquerda, então a ordem dos nomes é a do tempo.
    let mut copias: Vec<PathBuf> = std::fs::read_dir(&cfg.pasta)
        .map_err(erro_io)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("backup-") && n.ends_with(".sdb"))
        })
        .collect();
    copias.sort();
    let sobra = copias.len().saturating_sub(cfg.manter.max(1));
    for velha in &copias[..sobra] {
        std::fs::remove_file(velha).map_err(erro_io)?;
        let _ = std::fs::remove_file(wal_de(velha));
    }
    Ok(destino)
}

/// A tarefa de manutenção. Soltar (ou chamar [`Manutencao::parar`]) encerra a tarefa.
pub struct Manutencao {
    sinal: Arc<(Mutex<bool>, Condvar)>,
    estado: Arc<Mutex<EstadoManutencao>>,
    tarefa: Option<JoinHandle<()>>,
}

impl Manutencao {
    /// O que já foi feito e o último erro.
    pub fn estado(&self) -> EstadoManutencao {
        self.estado
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Para a tarefa e espera ela terminar o que estiver fazendo.
    pub fn parar(mut self) {
        self.encerrar();
    }

    fn encerrar(&mut self) {
        *self.sinal.0.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.sinal.1.notify_all();
        if let Some(t) = self.tarefa.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Manutencao {
    fn drop(&mut self) {
        self.encerrar();
    }
}

/// Começa a manutenção numa tarefa própria. O primeiro backup acontece logo no início.
pub fn iniciar(banco: Arc<BancoArquivo>, cfg: ConfigManutencao) -> Manutencao {
    let sinal = Arc::new((Mutex::new(false), Condvar::new()));
    let estado = Arc::new(Mutex::new(EstadoManutencao::default()));
    let (s, e) = (Arc::clone(&sinal), Arc::clone(&estado));
    let tarefa = std::thread::Builder::new()
        .name("sioux-manutencao".into())
        .spawn(move || {
            let registrar = |f: &dyn Fn(&mut EstadoManutencao)| {
                f(&mut e.lock().unwrap_or_else(|p| p.into_inner()))
            };
            let mut ultimo_backup: Option<Instant> = None;
            loop {
                if let Some(b) = &cfg.backup {
                    if ultimo_backup.is_none_or(|t| t.elapsed() >= b.intervalo) {
                        ultimo_backup = Some(Instant::now());
                        match fazer_backup(&banco, b) {
                            Ok(p) => registrar(&|e| {
                                e.backups += 1;
                                e.ultimo_backup = Some(p.clone());
                            }),
                            Err(erro) => {
                                registrar(&|e| e.ultimo_erro = Some(format!("backup: {erro}")))
                            }
                        }
                    }
                }
                match compactar_se_preciso(&banco, &cfg) {
                    Ok(feitas) => registrar(&|e| {
                        for (_, r) in &feitas {
                            e.compactacoes += 1;
                            e.versoes_removidas += r.versoes_antes.saturating_sub(r.versoes_depois);
                        }
                    }),
                    Err(erro) => {
                        registrar(&|e| e.ultimo_erro = Some(format!("compactação: {erro}")))
                    }
                }
                // Dorme até o próximo ciclo, ou até mandarem parar.
                let espera = cfg
                    .backup
                    .as_ref()
                    .map_or(cfg.intervalo, |b| cfg.intervalo.min(b.intervalo));
                let parar = s.0.lock().unwrap_or_else(|p| p.into_inner());
                let (parar, _) =
                    s.1.wait_timeout_while(parar, espera, |p| !*p)
                        .unwrap_or_else(|p| p.into_inner());
                if *parar {
                    return;
                }
            }
        })
        .ok();
    Manutencao {
        sinal,
        estado,
        tarefa,
    }
}
