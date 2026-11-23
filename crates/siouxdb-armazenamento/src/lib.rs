//! Armazenamento do SiouxDB.
//!
//! - [`Pagina`]: bloco de tamanho fixo com verificação (CRC32C) e número próprio, para
//!   detectar corrupção e página fora do lugar;
//! - [`Cabecalho`]: os dados gerais do arquivo, gravados em **duas cópias alternadas** —
//!   uma queda no meio da gravação nunca deixa o arquivo sem cabeçalho válido;
//! - [`CacheSieve`]: cache com o algoritmo de descarte SIEVE;
//! - [`Paginador`]: junta tudo: cria e abre arquivos, aloca, lê, grava e confirma páginas.
//!
//! A cifra por página entra no marco M2, por cima deste formato (ver `spec/FORMATO.md`).

mod cabecalho;
mod cache;
mod crc;
mod erro;
mod pagina;
mod paginador;

pub use cabecalho::{Cabecalho, TAMANHO_CABECALHO};
pub use cache::{CacheSieve, EstatisticasCache};
pub use crc::crc32c;
pub use erro::{ErroArmazenamento, Resultado};
pub use pagina::{CABECALHO_PAGINA, Pagina, TipoPagina};
pub use paginador::{EstatisticasPaginador, Paginador};

/// Número de uma página dentro do arquivo (a página 0 começa no byte 0).
pub type NumeroPagina = u64;

/// Assinatura dos arquivos do SiouxDB.
pub const MAGICA: [u8; 8] = *b"SIOUXDB\0";

/// Versão do formato gravada por esta biblioteca. Zero = ainda instável.
pub const VERSAO_FORMATO: u32 = 0;

/// As duas primeiras páginas guardam as duas cópias do cabeçalho.
pub const PAGINAS_RESERVADAS: NumeroPagina = 2;

/// Tamanhos de página aceitos. Escolhido na criação do arquivo e fixo depois.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TamanhoPagina {
    /// 4 KiB.
    K4,
    /// 8 KiB (padrão).
    #[default]
    K8,
    /// 16 KiB.
    K16,
    /// 32 KiB.
    K32,
}

impl TamanhoPagina {
    /// Todos os tamanhos aceitos.
    pub const TODOS: [TamanhoPagina; 4] = [
        TamanhoPagina::K4,
        TamanhoPagina::K8,
        TamanhoPagina::K16,
        TamanhoPagina::K32,
    ];

    /// Tamanho em bytes.
    pub const fn bytes(self) -> usize {
        match self {
            TamanhoPagina::K4 => 4 * 1024,
            TamanhoPagina::K8 => 8 * 1024,
            TamanhoPagina::K16 => 16 * 1024,
            TamanhoPagina::K32 => 32 * 1024,
        }
    }

    /// Converte de bytes, se for um tamanho aceito.
    pub fn de_bytes(bytes: u32) -> Option<TamanhoPagina> {
        Self::TODOS
            .into_iter()
            .find(|t| t.bytes() == bytes as usize)
    }
}
