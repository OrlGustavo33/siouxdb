//! Cabeçalho do arquivo.
//!
//! Fica em **duas cópias**: a A no começo da página 0 e a B no começo da página 1. Cada
//! gravação incrementa a `geracao` e vai para a cópia `geracao % 2`, ou seja, sempre
//! sobrescreve a cópia **mais antiga**. Se a energia cair no meio, a outra cópia continua
//! íntegra e o arquivo abre no estado anterior. Ao abrir, vale a cópia íntegra de maior geração.
//!
//! Cada cópia ocupa 512 bytes (um setor), com CRC32C no final.

use crate::{
    ErroArmazenamento, MAGICA, NumeroPagina, PAGINAS_RESERVADAS, Resultado, TamanhoPagina,
    VERSAO_FORMATO, crc32c,
};

/// Tamanho de cada cópia do cabeçalho em bytes.
pub const TAMANHO_CABECALHO: usize = 512;

/// Dados gerais do arquivo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cabecalho {
    /// Tamanho das páginas deste arquivo.
    pub tamanho_pagina: TamanhoPagina,
    /// Contador de gravações do cabeçalho. A maior geração íntegra é a que vale.
    pub geracao: u64,
    /// Quantas páginas o arquivo tem (contando as duas reservadas).
    pub total_paginas: NumeroPagina,
    /// Primeira página da lista de páginas livres (0 = nenhuma).
    pub primeira_livre: NumeroPagina,
    /// Página raiz do catálogo (0 = ainda não existe).
    pub raiz_catalogo: NumeroPagina,
    /// Página raiz do livro-razão (0 = ainda não existe).
    pub raiz_razao: NumeroPagina,
    /// Último LSN confirmado (0 = nenhum).
    pub ultimo_lsn: u64,
}

impl Cabecalho {
    /// Cabeçalho de um arquivo recém-criado.
    pub fn novo(tamanho_pagina: TamanhoPagina) -> Self {
        Cabecalho {
            tamanho_pagina,
            geracao: 0,
            total_paginas: PAGINAS_RESERVADAS,
            primeira_livre: 0,
            raiz_catalogo: 0,
            raiz_razao: 0,
            ultimo_lsn: 0,
        }
    }

    /// Posição no arquivo da cópia em que **esta geração** deve ser gravada.
    pub fn posicao_da_copia(&self) -> u64 {
        Self::posicao(self.geracao % 2 == 1, self.tamanho_pagina)
    }

    /// Posição da cópia A (`false`) ou B (`true`).
    pub fn posicao(copia_b: bool, tamanho_pagina: TamanhoPagina) -> u64 {
        if copia_b {
            tamanho_pagina.bytes() as u64
        } else {
            0
        }
    }

    /// Serializa em 512 bytes (little-endian), com CRC32C nos 4 últimos.
    pub fn codificar(&self) -> [u8; TAMANHO_CABECALHO] {
        let mut b = [0u8; TAMANHO_CABECALHO];
        b[0..8].copy_from_slice(&MAGICA);
        b[8..12].copy_from_slice(&VERSAO_FORMATO.to_le_bytes());
        b[12..16].copy_from_slice(&(self.tamanho_pagina.bytes() as u32).to_le_bytes());
        b[16..24].copy_from_slice(&self.geracao.to_le_bytes());
        b[24..32].copy_from_slice(&self.total_paginas.to_le_bytes());
        b[32..40].copy_from_slice(&self.primeira_livre.to_le_bytes());
        b[40..48].copy_from_slice(&self.raiz_catalogo.to_le_bytes());
        b[48..56].copy_from_slice(&self.raiz_razao.to_le_bytes());
        b[56..64].copy_from_slice(&self.ultimo_lsn.to_le_bytes());
        // 64..508: reservado (parâmetros de criptografia entram no M2), zerado.
        let crc = crc32c(&b[..TAMANHO_CABECALHO - 4]);
        b[TAMANHO_CABECALHO - 4..].copy_from_slice(&crc.to_le_bytes());
        b
    }

    /// Lê uma cópia. Erra se a assinatura, a versão, o CRC ou os campos não conferirem.
    pub fn decodificar(b: &[u8]) -> Resultado<Cabecalho> {
        if b.len() < TAMANHO_CABECALHO {
            return Err(ErroArmazenamento::CabecalhoCorrompido);
        }
        let b = &b[..TAMANHO_CABECALHO];
        if b[0..8] != MAGICA {
            return Err(ErroArmazenamento::NaoEhSiouxDb);
        }
        let u32_em = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().expect("4 bytes"));
        let u64_em = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().expect("8 bytes"));
        if u32_em(TAMANHO_CABECALHO - 4) != crc32c(&b[..TAMANHO_CABECALHO - 4]) {
            return Err(ErroArmazenamento::CabecalhoCorrompido);
        }
        let versao = u32_em(8);
        if versao != VERSAO_FORMATO {
            return Err(ErroArmazenamento::VersaoNaoSuportada(versao));
        }
        let tam = u32_em(12);
        let tamanho_pagina =
            TamanhoPagina::de_bytes(tam).ok_or(ErroArmazenamento::TamanhoPaginaInvalido(tam))?;
        let c = Cabecalho {
            tamanho_pagina,
            geracao: u64_em(16),
            total_paginas: u64_em(24),
            primeira_livre: u64_em(32),
            raiz_catalogo: u64_em(40),
            raiz_razao: u64_em(48),
            ultimo_lsn: u64_em(56),
        };
        if c.total_paginas < PAGINAS_RESERVADAS {
            return Err(ErroArmazenamento::CabecalhoCorrompido);
        }
        Ok(c)
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn vai_e_volta() {
        let mut c = Cabecalho::novo(TamanhoPagina::K16);
        c.geracao = 41;
        c.total_paginas = 1000;
        c.primeira_livre = 7;
        c.raiz_catalogo = 2;
        c.raiz_razao = 3;
        c.ultimo_lsn = 123_456;
        assert_eq!(Cabecalho::decodificar(&c.codificar()).unwrap(), c);
    }

    #[test]
    fn geracoes_alternam_as_copias() {
        let mut c = Cabecalho::novo(TamanhoPagina::K8);
        c.geracao = 2;
        assert_eq!(c.posicao_da_copia(), 0);
        c.geracao = 3;
        assert_eq!(c.posicao_da_copia(), 8192);
    }

    #[test]
    fn detecta_alteracao_e_arquivo_estranho() {
        let c = Cabecalho::novo(TamanhoPagina::K8);
        let mut b = c.codificar();
        b[20] ^= 1;
        assert!(matches!(
            Cabecalho::decodificar(&b),
            Err(ErroArmazenamento::CabecalhoCorrompido)
        ));
        let mut b = c.codificar();
        b[0] = b'X';
        assert!(matches!(
            Cabecalho::decodificar(&b),
            Err(ErroArmazenamento::NaoEhSiouxDb)
        ));
        assert!(matches!(
            Cabecalho::decodificar(&[0u8; 100]),
            Err(ErroArmazenamento::CabecalhoCorrompido)
        ));
    }
}
