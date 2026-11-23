//! Cabeçalho do arquivo.
//!
//! Fica em **duas cópias**: a A no começo da página 0 e a B no começo da página 1. Cada
//! gravação incrementa a `geracao` e vai para a cópia `geracao % 2`, ou seja, sempre
//! sobrescreve a cópia **mais antiga**. Se a energia cair no meio, a outra cópia continua
//! íntegra e o arquivo abre no estado anterior. Ao abrir, vale a cópia íntegra de maior geração.
//!
//! O cabeçalho fica **em claro** (é preciso lê-lo para derivar a chave), mas num arquivo
//! cifrado ele é **autenticado** com a chave do arquivo: alterar raízes, total de páginas ou
//! parâmetros da senha é detectado.
//!
//! Cada cópia ocupa 512 bytes (um setor), com CRC32C no final.
//! Bytes: 0..64 dados gerais, 64..168 proteção por senha, 168..184 identificador do
//! arquivo, 184..224 autenticação, 508..512 CRC32C.

use crate::{
    ErroArmazenamento, MAGICA, NumeroPagina, PAGINAS_RESERVADAS, Resultado, TamanhoPagina,
    VERSAO_FORMATO, crc32c,
};
use siouxdb_cripto::{CifraPagina, ParametrosSenha, SOBRA_CIFRA, TAMANHO_EMBRULHADA, TAMANHO_SAL};

/// Tamanho de cada cópia do cabeçalho em bytes.
pub const TAMANHO_CABECALHO: usize = 512;

const CIFRA_NENHUMA: u8 = 0;
const CIFRA_XCHACHA: u8 = 1;
const POS_CIFRA: usize = 64;
const POS_PARAMETROS: usize = 68;
const POS_SAL: usize = 80;
const POS_EMBRULHADA: usize = POS_SAL + TAMANHO_SAL; // 96
const POS_ID: usize = POS_EMBRULHADA + TAMANHO_EMBRULHADA; // 168
/// Bytes do identificador do arquivo.
pub const TAMANHO_ID: usize = 16;
const FIM_AUTENTICADO: usize = POS_ID + TAMANHO_ID; // 184
const POS_AUTENTICACAO: usize = FIM_AUTENTICADO;

/// Dados da proteção por senha guardados no cabeçalho.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Protecao {
    /// Custo do Argon2id.
    pub parametros: ParametrosSenha,
    /// Sal da senha.
    pub sal: [u8; TAMANHO_SAL],
    /// Chave do arquivo embrulhada pela chave da senha.
    pub embrulhada: [u8; TAMANHO_EMBRULHADA],
}

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
    /// Proteção por senha (`None` = arquivo em claro).
    pub protecao: Option<Protecao>,
    /// Identificador sorteado na criação. O WAL guarda o mesmo: um WAL de outro banco
    /// nunca é aplicado.
    pub id_arquivo: [u8; TAMANHO_ID],
    /// Etiqueta de autenticação lida do disco (só arquivos cifrados).
    autenticacao: [u8; SOBRA_CIFRA],
}

impl Cabecalho {
    /// Cabeçalho de um arquivo recém-criado.
    pub fn novo(tamanho_pagina: TamanhoPagina, protecao: Option<Protecao>) -> Self {
        Cabecalho {
            tamanho_pagina,
            geracao: 0,
            total_paginas: PAGINAS_RESERVADAS,
            primeira_livre: 0,
            raiz_catalogo: 0,
            raiz_razao: 0,
            ultimo_lsn: 0,
            protecao,
            id_arquivo: [0; TAMANHO_ID],
            autenticacao: [0; SOBRA_CIFRA],
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

    /// Bytes que identificam a proteção e nunca mudam entre gerações. A chave embrulhada é
    /// presa a eles: alterar parâmetros ou sal impede desembrulhar.
    pub fn contexto_da_chave(
        tamanho_pagina: TamanhoPagina,
        parametros: ParametrosSenha,
        sal: &[u8; TAMANHO_SAL],
    ) -> Vec<u8> {
        let mut c = Vec::with_capacity(48);
        c.extend_from_slice(&MAGICA);
        c.extend_from_slice(&VERSAO_FORMATO.to_le_bytes());
        c.extend_from_slice(&(tamanho_pagina.bytes() as u32).to_le_bytes());
        c.push(CIFRA_XCHACHA);
        c.extend_from_slice(&parametros.memoria_kib.to_le_bytes());
        c.extend_from_slice(&parametros.iteracoes.to_le_bytes());
        c.extend_from_slice(&parametros.paralelismo.to_le_bytes());
        c.extend_from_slice(sal);
        c
    }

    /// Os campos cobertos pela autenticação (bytes 0..168).
    fn campos(&self) -> [u8; FIM_AUTENTICADO] {
        let mut b = [0u8; FIM_AUTENTICADO];
        b[0..8].copy_from_slice(&MAGICA);
        b[8..12].copy_from_slice(&VERSAO_FORMATO.to_le_bytes());
        b[12..16].copy_from_slice(&(self.tamanho_pagina.bytes() as u32).to_le_bytes());
        b[16..24].copy_from_slice(&self.geracao.to_le_bytes());
        b[24..32].copy_from_slice(&self.total_paginas.to_le_bytes());
        b[32..40].copy_from_slice(&self.primeira_livre.to_le_bytes());
        b[40..48].copy_from_slice(&self.raiz_catalogo.to_le_bytes());
        b[48..56].copy_from_slice(&self.raiz_razao.to_le_bytes());
        b[56..64].copy_from_slice(&self.ultimo_lsn.to_le_bytes());
        if let Some(p) = &self.protecao {
            b[POS_CIFRA] = CIFRA_XCHACHA;
            b[POS_PARAMETROS..POS_PARAMETROS + 4]
                .copy_from_slice(&p.parametros.memoria_kib.to_le_bytes());
            b[POS_PARAMETROS + 4..POS_PARAMETROS + 8]
                .copy_from_slice(&p.parametros.iteracoes.to_le_bytes());
            b[POS_PARAMETROS + 8..POS_SAL].copy_from_slice(&p.parametros.paralelismo.to_le_bytes());
            b[POS_SAL..POS_EMBRULHADA].copy_from_slice(&p.sal);
            b[POS_EMBRULHADA..POS_ID].copy_from_slice(&p.embrulhada);
        }
        b[POS_ID..FIM_AUTENTICADO].copy_from_slice(&self.id_arquivo);
        b
    }

    /// Serializa em 512 bytes (little-endian), com CRC32C nos 4 últimos. Arquivo cifrado
    /// precisa da `cifra` para autenticar.
    pub fn codificar(&self, cifra: Option<&CifraPagina>) -> Resultado<[u8; TAMANHO_CABECALHO]> {
        let mut b = [0u8; TAMANHO_CABECALHO];
        let campos = self.campos();
        b[..FIM_AUTENTICADO].copy_from_slice(&campos);
        if self.protecao.is_some() {
            let cifra = cifra.ok_or(ErroArmazenamento::SenhaNecessaria)?;
            let etiqueta = cifra.autenticar(&campos)?;
            b[POS_AUTENTICACAO..POS_AUTENTICACAO + SOBRA_CIFRA].copy_from_slice(&etiqueta);
        }
        let crc = crc32c(&b[..TAMANHO_CABECALHO - 4]);
        b[TAMANHO_CABECALHO - 4..].copy_from_slice(&crc.to_le_bytes());
        Ok(b)
    }

    /// Lê uma cópia. Erra se a assinatura, a versão, o CRC ou os campos não conferirem.
    /// A autenticação (arquivo cifrado) é conferida à parte, com [`Cabecalho::autentico`].
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
        let protecao = match b[POS_CIFRA] {
            CIFRA_NENHUMA => None,
            CIFRA_XCHACHA => Some(Protecao {
                parametros: ParametrosSenha {
                    memoria_kib: u32_em(POS_PARAMETROS),
                    iteracoes: u32_em(POS_PARAMETROS + 4),
                    paralelismo: u32_em(POS_PARAMETROS + 8),
                },
                sal: b[POS_SAL..POS_EMBRULHADA]
                    .try_into()
                    .expect("tamanho do sal"),
                embrulhada: b[POS_EMBRULHADA..POS_ID]
                    .try_into()
                    .expect("tamanho da chave"),
            }),
            _ => return Err(ErroArmazenamento::CabecalhoCorrompido),
        };
        let c = Cabecalho {
            tamanho_pagina,
            geracao: u64_em(16),
            total_paginas: u64_em(24),
            primeira_livre: u64_em(32),
            raiz_catalogo: u64_em(40),
            raiz_razao: u64_em(48),
            ultimo_lsn: u64_em(56),
            protecao,
            id_arquivo: b[POS_ID..FIM_AUTENTICADO].try_into().expect("16 bytes"),
            autenticacao: b[POS_AUTENTICACAO..POS_AUTENTICACAO + SOBRA_CIFRA]
                .try_into()
                .expect("40 bytes"),
        };
        if c.total_paginas < PAGINAS_RESERVADAS {
            return Err(ErroArmazenamento::CabecalhoCorrompido);
        }
        Ok(c)
    }

    /// Num arquivo cifrado, confere se o cabeçalho foi escrito por quem tem a chave.
    pub fn autentico(&self, cifra: &CifraPagina) -> bool {
        self.protecao.is_some() && cifra.conferir(&self.campos(), &self.autenticacao)
    }
}

#[cfg(test)]
mod testes {
    use super::*;
    use siouxdb_cripto::Chave;

    #[test]
    fn vai_e_volta() {
        let mut c = Cabecalho::novo(TamanhoPagina::K16, None);
        c.geracao = 41;
        c.total_paginas = 1000;
        c.primeira_livre = 7;
        c.raiz_catalogo = 2;
        c.raiz_razao = 3;
        c.ultimo_lsn = 123_456;
        assert_eq!(
            Cabecalho::decodificar(&c.codificar(None).unwrap()).unwrap(),
            c
        );
    }

    #[test]
    fn cifrado_vai_volta_e_detecta_alteracao() {
        let cifra = CifraPagina::nova(Chave::aleatoria().unwrap());
        let protecao = Protecao {
            parametros: ParametrosSenha::para_testes(),
            sal: [7; TAMANHO_SAL],
            embrulhada: [9; TAMANHO_EMBRULHADA],
        };
        let mut c = Cabecalho::novo(TamanhoPagina::K8, Some(protecao));
        c.total_paginas = 10;
        assert!(matches!(
            c.codificar(None),
            Err(ErroArmazenamento::SenhaNecessaria)
        ));
        let b = c.codificar(Some(&cifra)).unwrap();
        let lido = Cabecalho::decodificar(&b).unwrap();
        assert_eq!(lido.protecao, c.protecao);
        assert!(lido.autentico(&cifra));
        assert!(!lido.autentico(&CifraPagina::nova(Chave::aleatoria().unwrap())));

        // Alguém troca o total de páginas e recalcula o CRC: o CRC passa, a autenticação não.
        let mut adulterado = b;
        adulterado[24] = 3;
        let crc = crc32c(&adulterado[..TAMANHO_CABECALHO - 4]);
        adulterado[TAMANHO_CABECALHO - 4..].copy_from_slice(&crc.to_le_bytes());
        let lido = Cabecalho::decodificar(&adulterado).unwrap();
        assert!(!lido.autentico(&cifra));
    }

    #[test]
    fn geracoes_alternam_as_copias() {
        let mut c = Cabecalho::novo(TamanhoPagina::K8, None);
        c.geracao = 2;
        assert_eq!(c.posicao_da_copia(), 0);
        c.geracao = 3;
        assert_eq!(c.posicao_da_copia(), 8192);
    }

    #[test]
    fn detecta_alteracao_e_arquivo_estranho() {
        let c = Cabecalho::novo(TamanhoPagina::K8, None);
        let mut b = c.codificar(None).unwrap();
        b[20] ^= 1;
        assert!(matches!(
            Cabecalho::decodificar(&b),
            Err(ErroArmazenamento::CabecalhoCorrompido)
        ));
        let mut b = c.codificar(None).unwrap();
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
