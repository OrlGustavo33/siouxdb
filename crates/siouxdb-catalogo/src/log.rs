//! Log da camada quente: o **livro-razão** das versões de linha.
//!
//! Toda gravação (inserir, alterar, remover) **anexa** uma versão nova no fim do log; nada é
//! sobrescrito. Cada versão aponta para a anterior da mesma chave, então o histórico de uma
//! linha é uma corrente que se percorre de trás para a frente. É a base do `AS OF`.
//!
//! ```text
//! página (tipo log quente):
//!   0..8    próxima página do log (0 = esta é a última)
//!   8..10   bytes usados pelos registros
//!   10..16  reservado
//!   16..    registros, um depois do outro
//!
//! registro (32 bytes + chave + dados):
//!   0..2    tamanho total        2      marcas (bit 0 = remoção)
//!   4..6    tamanho da chave     8..16  versão anterior da mesma chave (0 = nenhuma)
//!   16..24  LSN da transação     24..32 instante (microssegundos desde 1970)
//!   32..    chave codificada, depois a linha codificada
//! ```

use crate::{ErroCatalogo, Resultado};
use siouxdb_armazenamento::{NumeroPagina, TipoPagina};
use siouxdb_transacoes::{Paginas, PaginasMut};

const CAB_PAGINA: usize = 16;
const CAB_REGISTRO: usize = 32;
const MARCA_TRANSBORDO: u8 = 2;
const CAB_TRANSBORDO: usize = 12;

/// Maior linha aceita (16 MiB).
pub const MAXIMO_VALOR: usize = 16 << 20;
const RUIM: ErroCatalogo = ErroCatalogo::Corrompido("registro do log com forma inesperada");

/// Endereço de uma versão no log: página e posição dentro dela. Zero = nenhuma.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ponteiro(pub u64);

impl Ponteiro {
    /// Sem versão.
    pub const NENHUM: Ponteiro = Ponteiro(0);

    fn novo(pagina: NumeroPagina, posicao: usize) -> Ponteiro {
        Ponteiro(pagina << 16 | posicao as u64)
    }

    fn pagina(self) -> NumeroPagina {
        self.0 >> 16
    }

    fn posicao(self) -> usize {
        (self.0 & 0xFFFF) as usize
    }

    /// É [`Ponteiro::NENHUM`]?
    pub fn nenhum(self) -> bool {
        self.0 == 0
    }

    /// Lê dos 8 bytes guardados no índice.
    pub fn de_bytes(b: &[u8]) -> Resultado<Ponteiro> {
        Ok(Ponteiro(u64::from_le_bytes(
            b.try_into().map_err(|_| RUIM)?,
        )))
    }

    /// Os 8 bytes guardados no índice.
    pub fn bytes(self) -> [u8; 8] {
        self.0.to_le_bytes()
    }
}

/// Uma versão lida do log, sem cópia (vale dentro do fechamento de [`ler_com`]).
#[derive(Debug, Clone, Copy)]
pub struct Versao<'a> {
    /// É uma remoção?
    pub removida: bool,
    /// Versão anterior da mesma chave.
    pub anterior: Ponteiro,
    /// LSN da transação que gravou.
    pub lsn: u64,
    /// Instante da gravação (µs desde 1970).
    pub instante: u64,
    /// Chave codificada.
    pub chave: &'a [u8],
    /// Linha codificada (vazia numa remoção). Se [`Versao::transbordo`], é só o endereço:
    /// use [`dados_completos`].
    pub dados: &'a [u8],
    /// Os dados estão em páginas de transbordo?
    pub transbordo: bool,
}

/// Maior registro (chave + linha codificadas) que cabe numa página.
pub fn maximo_registro(tamanho_conteudo: usize) -> usize {
    tamanho_conteudo - CAB_PAGINA - CAB_REGISTRO
}

/// Pontas do log de uma tabela.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pontas {
    /// Primeira página (0 = log vazio).
    pub primeira: NumeroPagina,
    /// Última página, onde entram os registros novos.
    pub cauda: NumeroPagina,
}

/// Anexa uma versão (com o LSN e o instante dados) e devolve o endereço dela.
#[allow(clippy::too_many_arguments)]
pub fn anexar<P: PaginasMut + ?Sized>(
    tx: &mut P,
    pontas: &mut Pontas,
    removida: bool,
    anterior: Ponteiro,
    lsn: u64,
    instante: u64,
    chave: &[u8],
    dados: &[u8],
) -> Resultado<Ponteiro> {
    let conteudo = tx.tamanho_conteudo();
    if chave.len() > maximo_registro(conteudo) / 4 {
        return Err(ErroCatalogo::LinhaGrandeDemais {
            tamanho: chave.len(),
            maximo: maximo_registro(conteudo) / 4,
        });
    }
    if dados.len() > MAXIMO_VALOR {
        return Err(ErroCatalogo::LinhaGrandeDemais {
            tamanho: dados.len(),
            maximo: MAXIMO_VALOR,
        });
    }
    // Dados grandes vão para páginas de transbordo; o registro guarda só o endereço. Assim
    // o log continua denso (muitas versões por página) e o tamanho da linha não tem o limite
    // da página.
    let endereco;
    let (dados, marcas) = if chave.len() + dados.len() > maximo_registro(conteudo) / 2 {
        let primeira = gravar_transbordo(tx, dados)?;
        let mut e = [0u8; 16];
        e[..8].copy_from_slice(&(dados.len() as u64).to_le_bytes());
        e[8..].copy_from_slice(&primeira.to_le_bytes());
        endereco = e;
        (&endereco[..], u8::from(removida) | MARCA_TRANSBORDO)
    } else {
        (dados, u8::from(removida))
    };
    let tam = CAB_REGISTRO + chave.len() + dados.len();
    let mut usado = 0;
    if pontas.cauda != 0 {
        let p = tx.pagina(pontas.cauda)?;
        if p.tipo() != TipoPagina::LogQuente {
            return Err(RUIM);
        }
        usado = u16::from_le_bytes([p.conteudo()[8], p.conteudo()[9]]) as usize;
    }
    if pontas.cauda == 0 || CAB_PAGINA + usado + tam > conteudo {
        let nova = tx.alocar(TipoPagina::LogQuente)?;
        if pontas.cauda == 0 {
            pontas.primeira = nova;
        } else {
            tx.pagina_mut(pontas.cauda)?.conteudo_mut()[0..8].copy_from_slice(&nova.to_le_bytes());
        }
        pontas.cauda = nova;
        usado = 0;
    }
    let b = tx.pagina_mut(pontas.cauda)?.conteudo_mut();
    let pos = CAB_PAGINA + usado;
    let r = &mut b[pos..pos + tam];
    r[0..2].copy_from_slice(&(tam as u16).to_le_bytes());
    r[2] = marcas;
    r[3] = 0;
    r[4..6].copy_from_slice(&(chave.len() as u16).to_le_bytes());
    r[6..8].fill(0);
    r[8..16].copy_from_slice(&anterior.0.to_le_bytes());
    r[16..24].copy_from_slice(&lsn.to_le_bytes());
    r[24..32].copy_from_slice(&instante.to_le_bytes());
    r[32..32 + chave.len()].copy_from_slice(chave);
    r[32 + chave.len()..].copy_from_slice(dados);
    b[8..10].copy_from_slice(&((usado + tam) as u16).to_le_bytes());
    Ok(Ponteiro::novo(pontas.cauda, pos))
}

/// Lê a versão em `ponteiro` e aplica `f`, sem copiar chave nem dados.
pub fn ler_com<P: Paginas + ?Sized, T>(
    p: &P,
    ponteiro: Ponteiro,
    f: impl FnOnce(Versao<'_>) -> T,
) -> Resultado<T> {
    let pagina = p.pagina(ponteiro.pagina())?;
    if pagina.tipo() != TipoPagina::LogQuente {
        return Err(RUIM);
    }
    let b = pagina.conteudo();
    let usado = u16::from_le_bytes([b[8], b[9]]) as usize;
    let pos = ponteiro.posicao();
    let cab = b.get(pos..pos + CAB_REGISTRO).ok_or(RUIM)?;
    let tam = u16::from_le_bytes([cab[0], cab[1]]) as usize;
    let k = u16::from_le_bytes([cab[4], cab[5]]) as usize;
    if pos < CAB_PAGINA || tam < CAB_REGISTRO + k || pos + tam > CAB_PAGINA + usado {
        return Err(RUIM);
    }
    let r = b.get(pos..pos + tam).ok_or(RUIM)?;
    Ok(f(Versao {
        removida: r[2] & 1 == 1,
        transbordo: r[2] & MARCA_TRANSBORDO != 0,
        anterior: Ponteiro(u64::from_le_bytes(r[8..16].try_into().expect("8"))),
        lsn: u64::from_le_bytes(r[16..24].try_into().expect("8")),
        instante: u64::from_le_bytes(r[24..32].try_into().expect("8")),
        chave: &r[CAB_REGISTRO..CAB_REGISTRO + k],
        dados: &r[CAB_REGISTRO + k..],
    }))
}

/// Libera todas as páginas do log. Devolve quantas eram.
pub fn liberar<P: PaginasMut + ?Sized>(tx: &mut P, primeira: NumeroPagina) -> Resultado<u64> {
    let (mut atual, mut total) = (primeira, 0u64);
    let limite = tx.meta().total_paginas;
    while atual != 0 {
        let p = tx.pagina(atual)?;
        if p.tipo() != TipoPagina::LogQuente || total > limite {
            return Err(RUIM);
        }
        let b = p.conteudo();
        let proxima = u64::from_le_bytes(b[0..8].try_into().expect("8"));
        // Antes de soltar a página, solta os transbordos dos registros dela.
        let usado = u16::from_le_bytes([b[8], b[9]]) as usize;
        let mut transbordos = Vec::new();
        let mut pos = CAB_PAGINA;
        while pos < CAB_PAGINA + usado {
            let r = b.get(pos..pos + CAB_REGISTRO).ok_or(RUIM)?;
            let tam = u16::from_le_bytes([r[0], r[1]]) as usize;
            let k = u16::from_le_bytes([r[4], r[5]]) as usize;
            if tam < CAB_REGISTRO + k {
                return Err(RUIM);
            }
            if r[2] & MARCA_TRANSBORDO != 0 {
                transbordos.push(
                    b.get(pos + CAB_REGISTRO + k..pos + tam)
                        .ok_or(RUIM)?
                        .to_vec(),
                );
            }
            pos += tam;
        }
        drop(p);
        for t in transbordos {
            liberar_transbordo(tx, true, &t)?;
        }
        tx.liberar(atual)?;
        total += 1;
        atual = proxima;
    }
    Ok(total)
}

/// Grava `dados` numa corrente de páginas de transbordo e devolve a primeira.
/// Página: próxima (u64) · bytes nesta página (u32) · bytes.
fn gravar_transbordo<P: PaginasMut + ?Sized>(tx: &mut P, dados: &[u8]) -> Resultado<NumeroPagina> {
    let cabe = tx.tamanho_conteudo() - CAB_TRANSBORDO;
    let pedacos: Vec<&[u8]> = dados.chunks(cabe).collect();
    let mut proxima = 0u64;
    // De trás para a frente: cada página já nasce sabendo a próxima.
    for pedaco in pedacos.iter().rev() {
        let n = tx.alocar(TipoPagina::Transbordo)?;
        let b = tx.pagina_mut(n)?.conteudo_mut();
        b[0..8].copy_from_slice(&proxima.to_le_bytes());
        b[8..12].copy_from_slice(&(pedaco.len() as u32).to_le_bytes());
        b[CAB_TRANSBORDO..CAB_TRANSBORDO + pedaco.len()].copy_from_slice(pedaco);
        proxima = n;
    }
    Ok(proxima)
}

fn endereco_transbordo(dados: &[u8]) -> Resultado<(usize, NumeroPagina)> {
    if dados.len() != 16 {
        return Err(RUIM);
    }
    let total = u64::from_le_bytes(dados[..8].try_into().expect("8")) as usize;
    Ok((total, u64::from_le_bytes(dados[8..].try_into().expect("8"))))
}

/// Dados completos de uma versão (lê o transbordo se preciso).
pub fn dados_completos<'v, P: Paginas + ?Sized>(
    p: &P,
    v: &Versao<'v>,
) -> Resultado<std::borrow::Cow<'v, [u8]>> {
    if !v.transbordo {
        return Ok(std::borrow::Cow::Borrowed(v.dados));
    }
    let (total, mut atual) = endereco_transbordo(v.dados)?;
    if total > MAXIMO_VALOR {
        return Err(RUIM);
    }
    let mut saida = Vec::with_capacity(total);
    while saida.len() < total {
        if atual == 0 {
            return Err(RUIM);
        }
        let pagina = p.pagina(atual)?;
        if pagina.tipo() != TipoPagina::Transbordo {
            return Err(RUIM);
        }
        let b = pagina.conteudo();
        let n = u32::from_le_bytes(b[8..12].try_into().expect("4")) as usize;
        let pedaco = b.get(CAB_TRANSBORDO..CAB_TRANSBORDO + n).ok_or(RUIM)?;
        if n == 0 || saida.len() + n > total {
            return Err(RUIM);
        }
        saida.extend_from_slice(pedaco);
        atual = u64::from_le_bytes(b[0..8].try_into().expect("8"));
    }
    Ok(std::borrow::Cow::Owned(saida))
}

/// Libera a corrente de transbordo de uma versão (se tiver).
pub fn liberar_transbordo<P: PaginasMut + ?Sized>(
    tx: &mut P,
    transbordo: bool,
    dados: &[u8],
) -> Resultado<()> {
    if !transbordo {
        return Ok(());
    }
    let (_, mut atual) = endereco_transbordo(dados)?;
    let limite = tx.meta().total_paginas;
    let mut passos = 0;
    while atual != 0 {
        let p = tx.pagina(atual)?;
        if p.tipo() != TipoPagina::Transbordo || passos > limite {
            return Err(RUIM);
        }
        let proxima = u64::from_le_bytes(p.conteudo()[0..8].try_into().expect("8"));
        drop(p);
        tx.liberar(atual)?;
        atual = proxima;
        passos += 1;
    }
    Ok(())
}
