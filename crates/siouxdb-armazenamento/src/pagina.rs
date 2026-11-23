//! Página: bloco de tamanho fixo com um pequeno cabeçalho próprio.
//!
//! ```text
//! 0..4    CRC32C dos bytes 4..fim
//! 4       tipo da página
//! 5..8    reservado
//! 8..16   número da página (detecta página gravada ou copiada no lugar errado)
//! 16..24  LSN da última alteração
//! 24..    conteúdo
//! ```

use crate::{ErroArmazenamento, NumeroPagina, Resultado, crc32c};

/// Bytes do cabeçalho de cada página.
pub const CABECALHO_PAGINA: usize = 24;

/// O que a página guarda.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TipoPagina {
    /// Página livre (na lista de livres).
    Livre = 0,
    /// Catálogo (tabelas, colunas, usuários).
    Catalogo = 1,
    /// Log da camada quente.
    LogQuente = 2,
    /// Nó de árvore B+.
    NoArvore = 3,
    /// Bloco frio em colunas.
    BlocoFrio = 4,
    /// Resumo de bloco.
    Resumo = 5,
    /// Livro-razão (cadeia de hashes, pontos de controle).
    Razao = 6,
    /// Cabeçalho de uma tabela (raiz do índice, pontas do log, contagens).
    CabecalhoTabela = 7,
}

impl TipoPagina {
    fn de_byte(b: u8) -> Option<TipoPagina> {
        Some(match b {
            0 => TipoPagina::Livre,
            1 => TipoPagina::Catalogo,
            2 => TipoPagina::LogQuente,
            3 => TipoPagina::NoArvore,
            4 => TipoPagina::BlocoFrio,
            5 => TipoPagina::Resumo,
            6 => TipoPagina::Razao,
            7 => TipoPagina::CabecalhoTabela,
            _ => return None,
        })
    }
}

/// Uma página em memória.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pagina {
    dados: Box<[u8]>,
}

impl Pagina {
    /// Página zerada do tipo dado, com `tamanho` bytes (o tamanho útil: num arquivo
    /// cifrado é o tamanho da página menos o que a cifra ocupa).
    pub fn nova(tamanho: usize, tipo: TipoPagina) -> Pagina {
        let mut dados = vec![0u8; tamanho.max(CABECALHO_PAGINA)].into_boxed_slice();
        dados[4] = tipo as u8;
        Pagina { dados }
    }

    /// Monta a partir dos bytes lidos do disco e confere a verificação e o número.
    pub fn de_bytes(dados: Box<[u8]>, numero_esperado: NumeroPagina) -> Resultado<Pagina> {
        if dados.len() < CABECALHO_PAGINA {
            return Err(ErroArmazenamento::PaginaCorrompida(numero_esperado));
        }
        let crc = u32::from_le_bytes(dados[0..4].try_into().expect("4 bytes"));
        if crc != crc32c(&dados[4..]) || TipoPagina::de_byte(dados[4]).is_none() {
            return Err(ErroArmazenamento::PaginaCorrompida(numero_esperado));
        }
        let p = Pagina { dados };
        if p.numero() != numero_esperado {
            return Err(ErroArmazenamento::PaginaForaDoLugar {
                esperada: numero_esperado,
                encontrada: p.numero(),
            });
        }
        Ok(p)
    }

    /// Tipo da página.
    pub fn tipo(&self) -> TipoPagina {
        TipoPagina::de_byte(self.dados[4])
            .expect("tipo conferido na leitura ou definido na criação")
    }

    /// Troca o tipo da página.
    pub fn definir_tipo(&mut self, tipo: TipoPagina) {
        self.dados[4] = tipo as u8;
    }

    /// Número gravado na página (preenchido por [`Pagina::selar`]).
    pub fn numero(&self) -> NumeroPagina {
        u64::from_le_bytes(self.dados[8..16].try_into().expect("8 bytes"))
    }

    /// LSN da última alteração.
    pub fn lsn(&self) -> u64 {
        u64::from_le_bytes(self.dados[16..24].try_into().expect("8 bytes"))
    }

    /// Define o LSN da última alteração.
    pub fn definir_lsn(&mut self, lsn: u64) {
        self.dados[16..24].copy_from_slice(&lsn.to_le_bytes());
    }

    /// Conteúdo útil (depois do cabeçalho da página).
    pub fn conteudo(&self) -> &[u8] {
        &self.dados[CABECALHO_PAGINA..]
    }

    /// Conteúdo útil para alterar.
    pub fn conteudo_mut(&mut self) -> &mut [u8] {
        &mut self.dados[CABECALHO_PAGINA..]
    }

    /// Tamanho total em bytes.
    pub fn tamanho(&self) -> usize {
        self.dados.len()
    }

    /// Prepara para gravar na posição `numero`: grava o número e calcula a verificação.
    pub fn selar(&mut self, numero: NumeroPagina) -> &[u8] {
        self.dados[8..16].copy_from_slice(&numero.to_le_bytes());
        let crc = crc32c(&self.dados[4..]);
        self.dados[0..4].copy_from_slice(&crc.to_le_bytes());
        &self.dados
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn selar_e_reler() {
        let mut p = Pagina::nova(4096, TipoPagina::NoArvore);
        p.conteudo_mut()[..5].copy_from_slice(b"sioux");
        p.definir_lsn(99);
        let bytes: Box<[u8]> = p.selar(7).into();
        let lida = Pagina::de_bytes(bytes, 7).unwrap();
        assert_eq!(
            (lida.tipo(), lida.numero(), lida.lsn()),
            (TipoPagina::NoArvore, 7, 99)
        );
        assert_eq!(&lida.conteudo()[..5], b"sioux");
        assert_eq!(lida.conteudo().len(), 4096 - CABECALHO_PAGINA);
    }

    #[test]
    fn detecta_corrupcao_e_pagina_trocada() {
        let mut p = Pagina::nova(4096, TipoPagina::Catalogo);
        let mut bytes: Box<[u8]> = p.selar(3).into();
        assert!(matches!(
            Pagina::de_bytes(bytes.clone(), 4),
            Err(ErroArmazenamento::PaginaForaDoLugar {
                esperada: 4,
                encontrada: 3
            })
        ));
        bytes[100] ^= 0x40;
        assert!(matches!(
            Pagina::de_bytes(bytes, 3),
            Err(ErroArmazenamento::PaginaCorrompida(3))
        ));
        // Página toda zerada (nunca gravada) não é aceita como válida.
        assert!(Pagina::de_bytes(vec![0u8; 4096].into_boxed_slice(), 5).is_err());
    }
}
