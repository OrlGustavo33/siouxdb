//! Formato do WAL (registro de gravações).
//!
//! ```text
//! cabeçalho (64 bytes): "SIOUXWAL" | versão u32 | tamanho da página u32 | id do arquivo [16]
//!                       | sal u64 | sequência u64 | reservado | CRC32C dos bytes 0..60
//! quadro: [tipo u8 | reservado 7 | número da página u64 | LSN u64 | sal u64]  (32 bytes)
//!         [CRC encadeado u32 | reservado 4]                                    (8 bytes)
//!         [conteúdo]
//!   tipo 1 = página: conteúdo = a página no formato do disco (cifrada, se o banco for)
//!   tipo 2 = confirmação: conteúdo = meta (40) | quantidade de páginas u32 | reservado 4
//!            | autenticação (40, só em banco cifrado)
//! ```
//!
//! **CRC encadeado:** o CRC de cada quadro continua o do anterior. Na recuperação, a leitura
//! para no primeiro quadro que não confere (gravação cortada por queda); vale tudo até a
//! última confirmação íntegra. O **sal** muda a cada reinício do WAL: quadros velhos que
//! sobraram no fim do arquivo nunca são confundidos com novos.

use crate::{ErroBanco, Resultado};
use siouxdb_armazenamento::{Meta, NumeroPagina, Paginador, TAMANHO_ID, crc32c, crc32c_continuar};
use siouxdb_vfs::Arquivo;

pub(crate) const MAGICA_WAL: [u8; 8] = *b"SIOUXWAL";
pub(crate) const VERSAO_WAL: u32 = 1;
pub(crate) const TAMANHO_CABECALHO_WAL: u64 = 64;
pub(crate) const TAMANHO_QUADRO: usize = 40;
pub(crate) const TIPO_PAGINA: u8 = 1;
pub(crate) const TIPO_CONFIRMACAO: u8 = 2;
pub(crate) const TAMANHO_CONFIRMACAO: usize = 88;
const AUTENTICADO_CONFIRMACAO: usize = 48;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CabecalhoWal {
    pub tamanho_pagina: u32,
    pub id_arquivo: [u8; TAMANHO_ID],
    pub sal: u64,
    pub sequencia: u64,
}

impl CabecalhoWal {
    pub fn codificar(&self) -> [u8; TAMANHO_CABECALHO_WAL as usize] {
        let mut b = [0u8; TAMANHO_CABECALHO_WAL as usize];
        b[0..8].copy_from_slice(&MAGICA_WAL);
        b[8..12].copy_from_slice(&VERSAO_WAL.to_le_bytes());
        b[12..16].copy_from_slice(&self.tamanho_pagina.to_le_bytes());
        b[16..32].copy_from_slice(&self.id_arquivo);
        b[32..40].copy_from_slice(&self.sal.to_le_bytes());
        b[40..48].copy_from_slice(&self.sequencia.to_le_bytes());
        let crc = crc32c(&b[..60]);
        b[60..64].copy_from_slice(&crc.to_le_bytes());
        b
    }

    pub fn decodificar(b: &[u8]) -> Option<CabecalhoWal> {
        if b.len() < TAMANHO_CABECALHO_WAL as usize || b[0..8] != MAGICA_WAL {
            return None;
        }
        let u32_em = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().expect("4"));
        let u64_em = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().expect("8"));
        if u32_em(60) != crc32c(&b[..60]) || u32_em(8) != VERSAO_WAL {
            return None;
        }
        Some(CabecalhoWal {
            tamanho_pagina: u32_em(12),
            id_arquivo: b[16..32].try_into().expect("16"),
            sal: u64_em(32),
            sequencia: u64_em(40),
        })
    }
}

/// Os 32 primeiros bytes de um quadro (os que entram no CRC antes do conteúdo).
pub(crate) fn campos_quadro(tipo: u8, numero: NumeroPagina, lsn: u64, sal: u64) -> [u8; 32] {
    let mut h = [0u8; 32];
    h[0] = tipo;
    h[8..16].copy_from_slice(&numero.to_le_bytes());
    h[16..24].copy_from_slice(&lsn.to_le_bytes());
    h[24..32].copy_from_slice(&sal.to_le_bytes());
    h
}

/// Monta quadros em memória, encadeando o CRC.
pub(crate) struct Montador {
    pub bytes: Vec<u8>,
    pub crc: u32,
}

impl Montador {
    pub fn novo(crc: u32, capacidade: usize) -> Self {
        Montador {
            bytes: Vec::with_capacity(capacidade),
            crc,
        }
    }

    /// Acrescenta um quadro e devolve a posição dele dentro de `bytes`.
    pub fn quadro(&mut self, campos: &[u8; 32], conteudo: &[u8]) -> usize {
        let pos = self.bytes.len();
        self.crc = crc32c_continuar(crc32c_continuar(self.crc, campos), conteudo);
        self.bytes.extend_from_slice(campos);
        self.bytes.extend_from_slice(&self.crc.to_le_bytes());
        self.bytes.extend_from_slice(&[0; 4]);
        self.bytes.extend_from_slice(conteudo);
        pos
    }
}

/// Conteúdo de um quadro de confirmação. A autenticação cobre os campos do quadro, a meta,
/// a quantidade e o CRC encadeado até o quadro anterior.
pub(crate) fn conteudo_confirmacao<A: Arquivo>(
    paginador: &Paginador<A>,
    campos: &[u8; 32],
    meta: &Meta,
    quantidade: u32,
    crc_anterior: u32,
) -> Resultado<[u8; TAMANHO_CONFIRMACAO]> {
    let mut c = [0u8; TAMANHO_CONFIRMACAO];
    c[0..40].copy_from_slice(&meta.codificar());
    c[40..44].copy_from_slice(&quantidade.to_le_bytes());
    let dados = dados_autenticados(campos, &c, crc_anterior);
    if let Some(etiqueta) = paginador.autenticar(&dados)? {
        c[AUTENTICADO_CONFIRMACAO..].copy_from_slice(&etiqueta);
    }
    Ok(c)
}

fn dados_autenticados(campos: &[u8; 32], conteudo: &[u8], crc_anterior: u32) -> Vec<u8> {
    let mut d = Vec::with_capacity(32 + AUTENTICADO_CONFIRMACAO + 4);
    d.extend_from_slice(campos);
    d.extend_from_slice(&conteudo[..AUTENTICADO_CONFIRMACAO]);
    d.extend_from_slice(&crc_anterior.to_le_bytes());
    d
}

/// Um quadro de página aceito na recuperação.
pub(crate) struct QuadroPagina {
    pub numero: NumeroPagina,
    pub lsn: u64,
    pub posicao: u64,
}

/// O que a recuperação encontrou no WAL.
pub(crate) struct Varredura {
    pub paginas: Vec<QuadroPagina>,
    pub confirmacoes: Vec<Meta>,
    /// Onde a próxima gravação começa (logo depois da última confirmação íntegra).
    pub fim: u64,
    pub crc: u32,
}

/// Lê o WAL a partir do cabeçalho e devolve tudo até a última confirmação íntegra.
pub(crate) fn varrer<A: Arquivo>(
    wal: &A,
    paginador: &Paginador<A>,
    sal: u64,
) -> Resultado<Varredura> {
    let tam_pagina = paginador.tamanho_pagina().bytes();
    let tamanho = wal.tamanho()?;
    let mut v = Varredura {
        paginas: Vec::new(),
        confirmacoes: Vec::new(),
        fim: TAMANHO_CABECALHO_WAL,
        crc: 0,
    };
    let mut pos = TAMANHO_CABECALHO_WAL;
    let mut crc = 0u32;
    let mut pendentes: Vec<QuadroPagina> = Vec::new();
    let mut cab = [0u8; TAMANHO_QUADRO];
    let mut ultimo_lsn: Option<u64> = None;
    loop {
        if pos + TAMANHO_QUADRO as u64 > tamanho {
            break;
        }
        wal.ler_exato_em(pos, &mut cab)?;
        let campos: [u8; 32] = cab[..32].try_into().expect("32");
        let tipo = campos[0];
        let numero = u64::from_le_bytes(campos[8..16].try_into().expect("8"));
        let lsn = u64::from_le_bytes(campos[16..24].try_into().expect("8"));
        let sal_quadro = u64::from_le_bytes(campos[24..32].try_into().expect("8"));
        let crc_gravado = u32::from_le_bytes(cab[32..36].try_into().expect("4"));
        let tamanho_conteudo = match tipo {
            TIPO_PAGINA => tam_pagina,
            TIPO_CONFIRMACAO => TAMANHO_CONFIRMACAO,
            _ => break,
        };
        if sal_quadro != sal || pos + (TAMANHO_QUADRO + tamanho_conteudo) as u64 > tamanho {
            break;
        }
        let mut conteudo = vec![0u8; tamanho_conteudo];
        wal.ler_exato_em(pos + TAMANHO_QUADRO as u64, &mut conteudo)?;
        let crc_novo = crc32c_continuar(crc32c_continuar(crc, &campos), &conteudo);
        if crc_novo != crc_gravado {
            break; // gravação cortada: daqui para a frente não vale
        }
        if tipo == TIPO_PAGINA {
            pendentes.push(QuadroPagina {
                numero,
                lsn,
                posicao: pos,
            });
        } else {
            let quantidade = u32::from_le_bytes(conteudo[40..44].try_into().expect("4")) as usize;
            let coerente = quantidade == pendentes.len()
                && pendentes.iter().all(|q| q.lsn == lsn)
                && ultimo_lsn.is_none_or(|u| lsn == u + 1);
            let etiqueta: [u8; 40] = conteudo[AUTENTICADO_CONFIRMACAO..].try_into().expect("40");
            // CRC certo mas autenticação errada não é queda: é alteração.
            if !paginador
                .conferir_autenticacao(&dados_autenticados(&campos, &conteudo, crc), &etiqueta)
            {
                return Err(ErroBanco::WalAdulterado);
            }
            if !coerente {
                return Err(ErroBanco::WalAdulterado);
            }
            let meta = Meta::decodificar(conteudo[..40].try_into().expect("40"));
            v.paginas.append(&mut pendentes);
            v.confirmacoes.push(meta);
            ultimo_lsn = Some(lsn);
            v.fim = pos + (TAMANHO_QUADRO + tamanho_conteudo) as u64;
            v.crc = crc_novo;
        }
        crc = crc_novo;
        pos += (TAMANHO_QUADRO + tamanho_conteudo) as u64;
    }
    Ok(v)
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn cabecalho_do_wal_vai_e_volta() {
        let c = CabecalhoWal {
            tamanho_pagina: 8192,
            id_arquivo: [3; 16],
            sal: 77,
            sequencia: 2,
        };
        let b = c.codificar();
        assert_eq!(CabecalhoWal::decodificar(&b), Some(c));
        let mut ruim = b;
        ruim[33] ^= 1;
        assert_eq!(CabecalhoWal::decodificar(&ruim), None);
        assert_eq!(CabecalhoWal::decodificar(&[0; 10]), None);
    }

    #[test]
    fn montador_encadeia_o_crc() {
        let mut m = Montador::novo(0, 0);
        let campos = campos_quadro(TIPO_PAGINA, 5, 1, 9);
        assert_eq!(m.quadro(&campos, b"abc"), 0);
        let crc1 = m.crc;
        assert_eq!(crc1, crc32c(&[&campos[..], b"abc"].concat()));
        assert_eq!(m.quadro(&campos, b"abc"), TAMANHO_QUADRO + 3);
        assert_ne!(
            m.crc, crc1,
            "mesmo conteúdo, posição diferente na cadeia, CRC diferente"
        );
    }
}
