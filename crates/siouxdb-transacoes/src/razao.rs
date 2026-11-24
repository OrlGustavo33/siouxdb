//! Livro-razão: cada confirmação deixa uma entrada numa cadeia de hashes, e cada entrada
//! fixa o **estado inteiro** do banco naquele momento.
//!
//! - **Soma das páginas (LtHash):** cada página (menos as do próprio razão) vira 1024
//!   números de 16 bits (BLAKE3 em modo XOF sobre número + tipo + conteúdo); a soma, faixa a
//!   faixa, representa o banco inteiro. Na confirmação só as páginas alteradas entram na
//!   conta (tira a versão velha, põe a nova), então o custo não cresce com o tamanho do banco.
//! - **Estado:** `BLAKE3(soma ‖ meta)` — a meta inclui total de páginas, lista de livres,
//!   raízes e LSN.
//! - **Cadeia:** `elo = BLAKE3(elo anterior ‖ lsn ‖ instante ‖ estado)`.
//!
//! O [`verificar`] recalcula a cadeia do começo, recalcula a soma lendo todas as páginas e
//! compara com a última entrada. Pega página alterada, página que voltou a uma versão antiga
//! (mesmo num banco cifrado, onde a versão antiga ainda autentica), meta mexida e entrada da
//! cadeia trocada. O que nenhum arquivo consegue provar sozinho é que **ele inteiro** não
//! voltou no tempo: para isso existe a [`Ancora`] — guarde a âncora fora do banco (no e-mail
//! do backup, num log) e passe-a para o `VERIFY`.
//!
//! Páginas: a cabeça (raiz do razão na meta) guarda a soma, o último elo e as entradas mais
//! recentes; quando enche, as entradas vão para uma página arquivada, encadeada para trás.

use crate::banco::{Paginas, Transacao};
use crate::{Banco, ErroBanco, Resultado};
use siouxdb_armazenamento::{Meta, NumeroPagina, PAGINAS_RESERVADAS, Pagina, TipoPagina};
use siouxdb_vfs::Arquivo;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

/// Bytes de um hash.
pub const TAMANHO_HASH: usize = 32;
const FAIXAS: usize = 1024;
const MAGICA_CABECA: &[u8; 4] = b"SXRZ";
const MAGICA_ARQUIVADA: &[u8; 4] = b"SXRA";
const INICIO_SOMA: usize = 64;
const INICIO_ENTRADAS_CABECA: usize = INICIO_SOMA + FAIXAS * 2;
const INICIO_ENTRADAS_ARQUIVADA: usize = 16;
const TAMANHO_ENTRADA: usize = 80;
const CHAVE_PAGINA: &str = "SiouxDB razao pagina v1";
const CHAVE_ESTADO: &str = "SiouxDB razao estado v1";
const CHAVE_CADEIA: &str = "SiouxDB razao cadeia v1";

/// Soma das páginas (LtHash de 1024 faixas de 16 bits).
#[derive(Clone, PartialEq, Eq)]
struct Soma([u16; FAIXAS]);

impl Soma {
    fn zero() -> Soma {
        Soma([0; FAIXAS])
    }

    /// Soma (ou tira) a página. As páginas do próprio razão não entram.
    fn aplicar(&mut self, numero: NumeroPagina, p: &Pagina, somar: bool) {
        if p.tipo() == TipoPagina::Razao {
            return;
        }
        let mut h = blake3::Hasher::new_derive_key(CHAVE_PAGINA);
        h.update(&numero.to_le_bytes());
        h.update(&[p.tipo() as u8]);
        h.update(p.conteudo());
        let mut bytes = [0u8; FAIXAS * 2];
        h.finalize_xof().fill(&mut bytes);
        for (faixa, par) in self.0.iter_mut().zip(bytes.chunks_exact(2)) {
            let v = u16::from_le_bytes([par[0], par[1]]);
            *faixa = if somar {
                faixa.wrapping_add(v)
            } else {
                faixa.wrapping_sub(v)
            };
        }
    }

    fn ler(b: &[u8]) -> Soma {
        let mut s = Soma::zero();
        for (faixa, par) in s.0.iter_mut().zip(b.chunks_exact(2)) {
            *faixa = u16::from_le_bytes([par[0], par[1]]);
        }
        s
    }

    fn gravar(&self, b: &mut [u8]) {
        for (faixa, par) in self.0.iter().zip(b.chunks_exact_mut(2)) {
            par.copy_from_slice(&faixa.to_le_bytes());
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let mut b = vec![0u8; FAIXAS * 2];
        self.gravar(&mut b);
        b
    }
}

fn estado(soma: &Soma, meta: &Meta) -> [u8; TAMANHO_HASH] {
    let mut h = blake3::Hasher::new_derive_key(CHAVE_ESTADO);
    h.update(&soma.bytes());
    h.update(&meta.codificar());
    *h.finalize().as_bytes()
}

fn elo(
    anterior: &[u8; TAMANHO_HASH],
    lsn: u64,
    instante: u64,
    estado: &[u8; TAMANHO_HASH],
) -> [u8; TAMANHO_HASH] {
    let mut h = blake3::Hasher::new_derive_key(CHAVE_CADEIA);
    h.update(anterior);
    h.update(&lsn.to_le_bytes());
    h.update(&instante.to_le_bytes());
    h.update(estado);
    *h.finalize().as_bytes()
}

fn agora_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

fn u32_em(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().expect("4 bytes"))
}

fn u64_em(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().expect("8 bytes"))
}

fn hash_em(b: &[u8], i: usize) -> [u8; TAMANHO_HASH] {
    b[i..i + TAMANHO_HASH].try_into().expect("32 bytes")
}

/// Uma confirmação registrada no razão.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entrada {
    /// LSN da confirmação.
    pub lsn: u64,
    /// Quando foi confirmada (µs desde 1970, UTC, pelo relógio da máquina).
    pub instante: u64,
    /// Hash do estado inteiro do banco depois dela.
    pub estado: [u8; TAMANHO_HASH],
    /// Elo da cadeia até ela.
    pub elo: [u8; TAMANHO_HASH],
}

impl Entrada {
    fn ler(b: &[u8]) -> Entrada {
        Entrada {
            lsn: u64_em(b, 0),
            instante: u64_em(b, 8),
            estado: hash_em(b, 16),
            elo: hash_em(b, 48),
        }
    }

    fn gravar(&self, b: &mut [u8]) {
        b[0..8].copy_from_slice(&self.lsn.to_le_bytes());
        b[8..16].copy_from_slice(&self.instante.to_le_bytes());
        b[16..48].copy_from_slice(&self.estado);
        b[48..80].copy_from_slice(&self.elo);
    }
}

fn ler_entradas(b: &[u8], inicio: usize, qtd: usize) -> Option<Vec<Entrada>> {
    let fim = inicio.checked_add(qtd.checked_mul(TAMANHO_ENTRADA)?)?;
    if fim > b.len() {
        return None;
    }
    Some(
        b[inicio..fim]
            .chunks_exact(TAMANHO_ENTRADA)
            .map(Entrada::ler)
            .collect(),
    )
}

fn gravar_entradas(b: &mut [u8], inicio: usize, entradas: &[Entrada]) {
    for (e, pedaco) in entradas
        .iter()
        .zip(b[inicio..].chunks_exact_mut(TAMANHO_ENTRADA))
    {
        e.gravar(pedaco);
    }
}

/// A página-cabeça do razão.
struct Cabeca {
    anterior: NumeroPagina,
    total: u64,
    elo: [u8; TAMANHO_HASH],
    soma: Soma,
    entradas: Vec<Entrada>,
}

impl Cabeca {
    fn nova() -> Cabeca {
        Cabeca {
            anterior: 0,
            total: 0,
            elo: [0; TAMANHO_HASH],
            soma: Soma::zero(),
            entradas: Vec::new(),
        }
    }

    fn ler(p: &Pagina) -> Result<Cabeca, String> {
        let b = p.conteudo();
        if p.tipo() != TipoPagina::Razao || &b[0..4] != MAGICA_CABECA {
            return Err("a raiz do razão não aponta para uma cabeça do razão".into());
        }
        let qtd = u32_em(b, 4) as usize;
        let entradas = ler_entradas(b, INICIO_ENTRADAS_CABECA, qtd)
            .ok_or("cabeça do razão com quantidade de entradas impossível")?;
        Ok(Cabeca {
            anterior: u64_em(b, 8),
            total: u64_em(b, 16),
            elo: hash_em(b, 24),
            soma: Soma::ler(&b[INICIO_SOMA..INICIO_ENTRADAS_CABECA]),
            entradas,
        })
    }

    fn gravar(&self, p: &mut Pagina) {
        p.definir_tipo(TipoPagina::Razao);
        let b = p.conteudo_mut();
        b.fill(0);
        b[0..4].copy_from_slice(MAGICA_CABECA);
        b[4..8].copy_from_slice(&(self.entradas.len() as u32).to_le_bytes());
        b[8..16].copy_from_slice(&self.anterior.to_le_bytes());
        b[16..24].copy_from_slice(&self.total.to_le_bytes());
        b[24..56].copy_from_slice(&self.elo);
        self.soma
            .gravar(&mut b[INICIO_SOMA..INICIO_ENTRADAS_CABECA]);
        gravar_entradas(b, INICIO_ENTRADAS_CABECA, &self.entradas);
    }
}

fn capacidade_cabeca(tamanho_conteudo: usize) -> usize {
    tamanho_conteudo.saturating_sub(INICIO_ENTRADAS_CABECA) / TAMANHO_ENTRADA
}

/// Registra a confirmação `tx.lsn()` no razão. Chamado por `confirmar` antes de gravar o WAL:
/// as páginas do razão vão no mesmo quadro, então a entrada existe se e só se a confirmação
/// existe.
pub(crate) fn registrar<A: Arquivo>(tx: &mut Transacao<'_, A>) -> Resultado<()> {
    let lsn = tx.lsn();
    let tamanho = tx.tamanho_conteudo();
    let (mut cab, raiz) = if tx.meta.raiz_razao == 0 {
        // Primeira confirmação com razão (banco novo ou criado antes dele): soma tudo uma vez.
        let mut cab = Cabeca::nova();
        for n in PAGINAS_RESERVADAS..tx.meta_inicial.total_paginas {
            let p = tx.banco.pagina_em(tx.base_lsn, &tx.meta_inicial, n)?;
            cab.soma.aplicar(n, &p, true);
        }
        let raiz = tx.alocar(TipoPagina::Razao)?;
        tx.meta.raiz_razao = raiz;
        (cab, raiz)
    } else {
        let raiz = tx.meta.raiz_razao;
        let cab = Cabeca::ler(&*tx.pagina(raiz)?).map_err(ErroBanco::Razao)?;
        (cab, raiz)
    };
    if cab.entradas.len() >= capacidade_cabeca(tamanho) {
        let arq = tx.alocar(TipoPagina::Razao)?;
        let p = tx.pagina_mut(arq)?;
        let b = p.conteudo_mut();
        b[0..4].copy_from_slice(MAGICA_ARQUIVADA);
        b[4..8].copy_from_slice(&(cab.entradas.len() as u32).to_le_bytes());
        b[8..16].copy_from_slice(&cab.anterior.to_le_bytes());
        gravar_entradas(b, INICIO_ENTRADAS_ARQUIVADA, &cab.entradas);
        cab.anterior = arq;
        cab.entradas.clear();
    }
    // Só as páginas alteradas mudam a soma: tira a versão de antes, põe a nova.
    let base_total = tx.meta_inicial.total_paginas;
    for (&n, nova) in &tx.sujas {
        if n < base_total {
            let velha = tx.banco.pagina_em(tx.base_lsn, &tx.meta_inicial, n)?;
            cab.soma.aplicar(n, &velha, false);
        }
        cab.soma.aplicar(n, nova, true);
    }
    let mut meta = tx.meta;
    meta.ultimo_lsn = lsn;
    let estado = estado(&cab.soma, &meta);
    let instante = agora_us();
    let novo_elo = elo(&cab.elo, lsn, instante, &estado);
    cab.entradas.push(Entrada {
        lsn,
        instante,
        estado,
        elo: novo_elo,
    });
    cab.total += 1;
    cab.elo = novo_elo;
    cab.gravar(tx.pagina_mut(raiz)?);
    Ok(())
}

/// Âncora: o LSN e o elo da cadeia naquela confirmação. Guardada fora do banco, prova depois
/// que o arquivo não voltou no tempo nem teve o passado reescrito. Texto: `lsn:hex`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ancora {
    /// LSN da confirmação.
    pub lsn: u64,
    /// Elo da cadeia nela.
    pub elo: [u8; TAMANHO_HASH],
}

impl fmt::Display for Ancora {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:", self.lsn)?;
        for b in self.elo {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl std::str::FromStr for Ancora {
    type Err = String;

    fn from_str(s: &str) -> Result<Ancora, String> {
        let erro = || "âncora deve ser \"lsn:hash\" (hash com 64 dígitos hexadecimais)".to_string();
        let (lsn, hex) = s.trim().split_once(':').ok_or_else(erro)?;
        let lsn = lsn.parse().map_err(|_| erro())?;
        if hex.len() != 2 * TAMANHO_HASH || !hex.is_ascii() {
            return Err(erro());
        }
        let mut elo = [0u8; TAMANHO_HASH];
        for (i, b) in elo.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).map_err(|_| erro())?;
        }
        Ok(Ancora { lsn, elo })
    }
}

/// Resultado do [`verificar`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RelatorioRazao {
    /// O banco tem razão? (Bancos sem nenhuma confirmação ainda não têm.)
    pub tem_razao: bool,
    /// Confirmações na cadeia.
    pub confirmacoes: u64,
    /// LSN da primeira confirmação registrada.
    pub primeiro_lsn: u64,
    /// LSN da última.
    pub ultimo_lsn: u64,
    /// Instante da última (µs desde 1970, UTC).
    pub ultimo_instante: u64,
    /// Páginas conferidas contra a soma.
    pub paginas: u64,
    /// Âncora atual (guarde fora do banco).
    pub ancora: Option<Ancora>,
    /// O que não conferiu. Vazio = tudo certo.
    pub problemas: Vec<String>,
}

impl RelatorioRazao {
    /// Tudo conferiu?
    pub fn ok(&self) -> bool {
        self.problemas.is_empty()
    }
}

/// Todas as entradas, da mais antiga para a mais nova.
fn entradas<P: Paginas + ?Sized>(p: &P, cab: &Cabeca, problemas: &mut Vec<String>) -> Vec<Entrada> {
    let mut paginas = Vec::new();
    let mut n = cab.anterior;
    let limite = p.meta().total_paginas;
    while n != 0 {
        if paginas.len() as u64 > limite || n >= limite || n < PAGINAS_RESERVADAS {
            problemas.push(format!(
                "páginas arquivadas do razão em laço ou fora do banco (página {n})"
            ));
            break;
        }
        let pg = match p.pagina(n) {
            Ok(pg) => pg,
            Err(e) => {
                problemas.push(format!("página {n} do razão ilegível: {e}"));
                break;
            }
        };
        let b = pg.conteudo();
        let lidas = (pg.tipo() == TipoPagina::Razao && &b[0..4] == MAGICA_ARQUIVADA)
            .then(|| ler_entradas(b, INICIO_ENTRADAS_ARQUIVADA, u32_em(b, 4) as usize))
            .flatten();
        let Some(lidas) = lidas else {
            problemas.push(format!(
                "página {n} deveria ser do razão (arquivada) e não é"
            ));
            break;
        };
        paginas.push(lidas);
        n = u64_em(b, 8);
    }
    let mut todas: Vec<Entrada> = paginas.into_iter().rev().flatten().collect();
    todas.extend_from_slice(&cab.entradas);
    todas
}

/// Âncora da última confirmação (`None` se o banco ainda não tem razão).
pub fn ancora_atual<P: Paginas + ?Sized>(p: &P) -> Resultado<Option<Ancora>> {
    let raiz = p.meta().raiz_razao;
    if raiz == 0 {
        return Ok(None);
    }
    let cab = Cabeca::ler(&*p.pagina(raiz)?).map_err(ErroBanco::Razao)?;
    Ok(cab.entradas.last().map(|e| Ancora {
        lsn: e.lsn,
        elo: e.elo,
    }))
}

/// Confere tudo: a cadeia do começo ao fim, todas as páginas contra a soma, a meta e (se
/// vier) a âncora guardada fora. Erros de leitura viram problemas no relatório, não erro.
pub fn verificar<P: Paginas + ?Sized>(p: &P, ancora: Option<&Ancora>) -> RelatorioRazao {
    let meta = p.meta();
    let mut r = RelatorioRazao::default();
    if meta.raiz_razao == 0 {
        if meta.ultimo_lsn > 0 && meta.total_paginas > PAGINAS_RESERVADAS {
            // Só bancos gravados por versões sem razão chegam aqui; a próxima confirmação
            // cria o razão.
            r.problemas.push(
                "banco sem razão (gravado por uma versão antiga): faça uma confirmação para criá-lo"
                    .into(),
            );
        }
        if let Some(a) = ancora {
            r.problemas.push(format!(
                "âncora da confirmação {} não confere: o banco não tem razão",
                a.lsn
            ));
        }
        return r;
    }
    r.tem_razao = true;
    let cab = match p.pagina(meta.raiz_razao) {
        Ok(pg) => Cabeca::ler(&pg),
        Err(e) => Err(format!("cabeça do razão ilegível: {e}")),
    };
    let cab = match cab {
        Ok(c) => c,
        Err(m) => {
            r.problemas.push(m);
            return r;
        }
    };
    let todas = entradas(p, &cab, &mut r.problemas);
    r.confirmacoes = todas.len() as u64;
    if todas.len() as u64 != cab.total {
        r.problemas.push(format!(
            "o razão diz ter {} confirmações e tem {}",
            cab.total,
            todas.len()
        ));
    }
    // Cadeia.
    let mut anterior = [0u8; TAMANHO_HASH];
    let mut lsn_anterior = 0u64;
    for e in &todas {
        if e.lsn <= lsn_anterior {
            r.problemas.push(format!(
                "cadeia fora de ordem: confirmação {} depois da {}",
                e.lsn, lsn_anterior
            ));
            break;
        }
        if elo(&anterior, e.lsn, e.instante, &e.estado) != e.elo {
            r.problemas.push(format!(
                "cadeia quebrada na confirmação {}: a entrada foi alterada",
                e.lsn
            ));
            break;
        }
        anterior = e.elo;
        lsn_anterior = e.lsn;
    }
    if anterior != cab.elo {
        r.problemas
            .push("o último elo guardado na cabeça não confere com a cadeia".into());
    }
    if let (Some(primeira), Some(ultima)) = (todas.first(), todas.last()) {
        r.primeiro_lsn = primeira.lsn;
        r.ultimo_lsn = ultima.lsn;
        r.ultimo_instante = ultima.instante;
        r.ancora = Some(Ancora {
            lsn: ultima.lsn,
            elo: ultima.elo,
        });
    }
    // Páginas: a soma recalculada tem de ser a guardada e dar o estado da última entrada.
    let mut soma = Soma::zero();
    for n in PAGINAS_RESERVADAS..meta.total_paginas {
        match p.pagina(n) {
            Ok(pg) => soma.aplicar(n, &pg, true),
            Err(e) => r.problemas.push(format!("página {n} ilegível: {e}")),
        }
        r.paginas += 1;
    }
    if soma != cab.soma {
        r.problemas.push(
            "as páginas não conferem com o razão: alguma foi alterada ou voltou a uma versão antiga"
                .into(),
        );
    }
    match todas.last() {
        Some(ultima) => {
            if ultima.lsn != meta.ultimo_lsn {
                r.problemas.push(format!(
                    "a última confirmação do razão é a {} e o banco está na {}",
                    ultima.lsn, meta.ultimo_lsn
                ));
            } else if estado(&soma, &meta) != ultima.estado {
                r.problemas.push(
                    "o estado do banco (páginas + meta) não confere com a última entrada do razão"
                        .into(),
                );
            }
        }
        None => r.problemas.push("razão sem nenhuma entrada".into()),
    }
    if let Some(a) = ancora {
        match todas.iter().find(|e| e.lsn == a.lsn) {
            Some(e) if e.elo == a.elo => {}
            Some(_) => r.problemas.push(format!(
                "âncora da confirmação {} não confere: o histórico foi reescrito",
                a.lsn
            )),
            None if a.lsn > r.ultimo_lsn => r.problemas.push(format!(
                "âncora da confirmação {}, mas o banco só vai até a {}: o arquivo voltou no tempo",
                a.lsn, r.ultimo_lsn
            )),
            None => r
                .problemas
                .push(format!("âncora da confirmação {} não está no razão", a.lsn)),
        }
    }
    r
}

impl<A: Arquivo> Banco<A> {
    /// Confere o razão numa foto do banco (veja [`verificar`]).
    pub fn verificar_razao(&self, ancora: Option<&Ancora>) -> Resultado<RelatorioRazao> {
        Ok(verificar(&self.ler()?, ancora))
    }

    /// Âncora da última confirmação (guarde fora do banco).
    pub fn ancora(&self) -> Resultado<Option<Ancora>> {
        ancora_atual(&self.ler()?)
    }
}
