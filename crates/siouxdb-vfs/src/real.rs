//! Disco de verdade, pelo `std::fs`, com leitura e gravação por posição.

use crate::{Arquivo, ErroVfs, ModoAbertura, Resultado, Vfs};
use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::path::Path;

/// Sistema de arquivos real.
#[derive(Debug, Default, Clone, Copy)]
pub struct VfsReal;

impl VfsReal {
    /// Cria o acesso ao sistema de arquivos real.
    pub fn novo() -> Self {
        VfsReal
    }
}

/// Arquivo aberto no disco real.
#[derive(Debug)]
pub struct ArquivoReal {
    arquivo: File,
    caminho: std::path::PathBuf,
    somente_leitura: bool,
}

fn traduzir(e: std::io::Error, caminho: &Path) -> ErroVfs {
    match e.kind() {
        ErrorKind::NotFound => ErroVfs::NaoEncontrado(caminho.to_path_buf()),
        ErrorKind::AlreadyExists => ErroVfs::JaExiste(caminho.to_path_buf()),
        ErrorKind::StorageFull => ErroVfs::DiscoCheio,
        _ => ErroVfs::Io(e),
    }
}

impl Vfs for VfsReal {
    type Arquivo = ArquivoReal;

    fn abrir(&self, caminho: &Path, modo: ModoAbertura) -> Resultado<ArquivoReal> {
        let mut op = OpenOptions::new();
        op.read(true);
        match modo {
            ModoAbertura::Leitura => {}
            ModoAbertura::LeituraGravacao => {
                op.write(true);
            }
            ModoAbertura::CriarSeNaoExistir => {
                op.write(true).create(true);
            }
            ModoAbertura::CriarNovo => {
                op.write(true).create_new(true);
            }
        }
        let arquivo = op.open(caminho).map_err(|e| traduzir(e, caminho))?;
        Ok(ArquivoReal {
            arquivo,
            caminho: caminho.to_path_buf(),
            somente_leitura: modo == ModoAbertura::Leitura,
        })
    }

    fn existe(&self, caminho: &Path) -> Resultado<bool> {
        Ok(caminho.try_exists()?)
    }

    fn apagar(&self, caminho: &Path) -> Resultado<()> {
        std::fs::remove_file(caminho).map_err(|e| traduzir(e, caminho))
    }

    fn renomear(&self, de: &Path, para: &Path) -> Resultado<()> {
        std::fs::rename(de, para).map_err(|e| traduzir(e, de))?;
        // Para o novo nome sobreviver a uma queda, a pasta também precisa ir para o disco.
        sincronizar_pasta(para)
    }
}

#[cfg(unix)]
fn sincronizar_pasta(caminho: &Path) -> Resultado<()> {
    if let Some(pasta) = caminho.parent().filter(|p| !p.as_os_str().is_empty()) {
        File::open(pasta)?.sync_all()?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn sincronizar_pasta(_caminho: &Path) -> Resultado<()> {
    // No Windows o rename já é registrado pelo NTFS; não há fsync de pasta.
    Ok(())
}

#[cfg(unix)]
fn ler_posicional(f: &File, buf: &mut [u8], pos: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(f, buf, pos)
}

#[cfg(windows)]
fn ler_posicional(f: &File, buf: &mut [u8], pos: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(f, buf, pos)
}

#[cfg(unix)]
fn gravar_posicional(f: &File, buf: &[u8], pos: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::write_at(f, buf, pos)
}

#[cfg(windows)]
fn gravar_posicional(f: &File, buf: &[u8], pos: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_write(f, buf, pos)
}

impl Arquivo for ArquivoReal {
    fn ler_em(&self, posicao: u64, destino: &mut [u8]) -> Resultado<usize> {
        let mut total = 0;
        while total < destino.len() {
            match ler_posicional(&self.arquivo, &mut destino[total..], posicao + total as u64) {
                Ok(0) => break,
                Ok(n) => total += n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(total)
    }

    fn gravar_em(&self, posicao: u64, dados: &[u8]) -> Resultado<()> {
        if self.somente_leitura {
            return Err(ErroVfs::SomenteLeitura);
        }
        let mut total = 0;
        while total < dados.len() {
            match gravar_posicional(&self.arquivo, &dados[total..], posicao + total as u64) {
                Ok(0) => return Err(ErroVfs::DiscoCheio),
                Ok(n) => total += n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == ErrorKind::StorageFull => return Err(ErroVfs::DiscoCheio),
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    fn travar_exclusivo(&self) -> Resultado<()> {
        // `flock` no Unix (por abertura: vale também entre aberturas do mesmo processo) e
        // `LockFile` no Windows. Solta sozinha quando o arquivo é fechado.
        match fs4::fs_std::FileExt::try_lock_exclusive(&self.arquivo) {
            Ok(true) => Ok(()),
            Ok(false) => Err(ErroVfs::EmUso(self.caminho.clone())),
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                Err(ErroVfs::EmUso(self.caminho.clone()))
            }
            Err(e) => Err(e.into()),
        }
    }

    fn sincronizar(&self) -> Resultado<()> {
        if self.somente_leitura {
            return Ok(());
        }
        Ok(self.arquivo.sync_data()?)
    }

    fn tamanho(&self) -> Resultado<u64> {
        Ok(self.arquivo.metadata()?.len())
    }

    fn truncar(&self, tamanho: u64) -> Resultado<()> {
        if self.somente_leitura {
            return Err(ErroVfs::SomenteLeitura);
        }
        Ok(self.arquivo.set_len(tamanho)?)
    }
}

#[cfg(test)]
mod testes {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Pasta temporária própria de cada teste (sem dependências externas).
    fn pasta_teste() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "siouxdb-vfs-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn trava_exclusiva_entre_aberturas() {
        let pasta = pasta_teste();
        let vfs = VfsReal::novo();
        let c = pasta.join("trava.sdb");
        let a = vfs.abrir(&c, ModoAbertura::CriarNovo).unwrap();
        a.travar_exclusivo().unwrap();
        let b = vfs.abrir(&c, ModoAbertura::LeituraGravacao).unwrap();
        assert!(matches!(b.travar_exclusivo(), Err(ErroVfs::EmUso(_))));
        drop(a);
        b.travar_exclusivo().unwrap();
        std::fs::remove_dir_all(pasta).unwrap();
    }

    #[test]
    fn grava_le_e_sincroniza() {
        let pasta = pasta_teste();
        let vfs = VfsReal::novo();
        let c = pasta.join("a.sdb");
        let a = vfs.abrir(&c, ModoAbertura::CriarNovo).unwrap();
        a.gravar_em(0, b"siouxdb").unwrap();
        a.gravar_em(4096, b"pagina 1").unwrap();
        a.sincronizar().unwrap();
        assert_eq!(a.tamanho().unwrap(), 4096 + 8);
        let mut buf = [0u8; 7];
        a.ler_exato_em(0, &mut buf).unwrap();
        assert_eq!(&buf, b"siouxdb");
        let mut buf = [0u8; 8];
        a.ler_exato_em(4096, &mut buf).unwrap();
        assert_eq!(&buf, b"pagina 1");
        std::fs::remove_dir_all(pasta).unwrap();
    }

    #[test]
    fn erros_claros() {
        let pasta = pasta_teste();
        let vfs = VfsReal::novo();
        let c = pasta.join("nao_existe.sdb");
        assert!(matches!(
            vfs.abrir(&c, ModoAbertura::Leitura),
            Err(ErroVfs::NaoEncontrado(_))
        ));
        vfs.abrir(&c, ModoAbertura::CriarNovo).unwrap();
        assert!(matches!(
            vfs.abrir(&c, ModoAbertura::CriarNovo),
            Err(ErroVfs::JaExiste(_))
        ));
        let ro = vfs.abrir(&c, ModoAbertura::Leitura).unwrap();
        assert!(matches!(
            ro.gravar_em(0, b"x"),
            Err(ErroVfs::SomenteLeitura)
        ));
        let mut buf = [0u8; 4];
        assert!(matches!(
            ro.ler_exato_em(0, &mut buf),
            Err(ErroVfs::FimInesperado { .. })
        ));
        std::fs::remove_dir_all(pasta).unwrap();
    }

    #[test]
    fn renomear_substitui() {
        let pasta = pasta_teste();
        let vfs = VfsReal::novo();
        let (a, b) = (pasta.join("tmp"), pasta.join("final"));
        vfs.abrir(&a, ModoAbertura::CriarNovo)
            .unwrap()
            .gravar_em(0, b"novo")
            .unwrap();
        vfs.abrir(&b, ModoAbertura::CriarNovo)
            .unwrap()
            .gravar_em(0, b"velho")
            .unwrap();
        vfs.renomear(&a, &b).unwrap();
        assert!(!vfs.existe(&a).unwrap());
        let mut buf = [0u8; 4];
        vfs.abrir(&b, ModoAbertura::Leitura)
            .unwrap()
            .ler_exato_em(0, &mut buf)
            .unwrap();
        assert_eq!(&buf, b"novo");
        std::fs::remove_dir_all(pasta).unwrap();
    }
}
