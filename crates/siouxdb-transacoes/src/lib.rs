//! Transações do SiouxDB.
//!
//! [`Banco`] junta o arquivo principal (páginas verificadas e, por padrão, cifradas) com
//! um **WAL** ao lado (`banco.sdb-wal`):
//!
//! - uma [`Transacao`] junta as páginas alteradas e, ao confirmar, grava tudo no fim do WAL
//!   de uma vez, com uma marca de confirmação. Na recuperação vale só o que tem a marca:
//!   **tudo ou nada**, inclusive para páginas já existentes sobrescritas;
//! - **um escritor por vez**, numa fila justa com tempo máximo de espera;
//! - **confirmação em grupo**: o escritor solta a vez antes do `fsync`; quem confirma junto
//!   divide o mesmo `fsync`;
//! - **leitores com foto**: uma [`Leitura`] vê o banco como estava quando começou, sem
//!   esperar e sem bloquear ninguém;
//! - **checkpoint**: copia as páginas do WAL para o arquivo principal (respeitando as fotos
//!   abertas) e reinicia o WAL;
//! - [`Sincronia`] configurável: `Total` (nada confirmado se perde) ou `Normal` (mais rápido,
//!   pode perder as últimas confirmações numa queda, nunca corromper);
//! - **livro-razão** ([`razao`]): cada confirmação entra numa cadeia de hashes que fixa o
//!   estado inteiro do banco; [`Banco::verificar_razao`] confere tudo.

mod banco;
mod erro;
mod fila;
pub mod razao;
mod wal;

pub use banco::{
    Banco, Config, EstatisticasBanco, Leitura, Lembra, Paginas, PaginasMut, Sincronia, Transacao,
    cache_automatico_bytes,
};
pub use erro::{ErroBanco, Resultado};
pub use razao::{Ancora, RelatorioRazao};

#[cfg(test)]
mod testes;
