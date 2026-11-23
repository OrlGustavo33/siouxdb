//! Catálogo: quais tabelas existem e como são.
//!
//! É uma árvore B+ cuja raiz fica no cabeçalho do banco (`raiz_catalogo`). Guarda uma entrada
//! por tabela (nome → definição) e o contador de identificadores. As **tabelas do sistema**
//! `sys_tabelas` e `sys_colunas` são visões calculadas a partir dele.
//!
//! Criar e apagar tabela acontecem dentro de uma transação como qualquer outra gravação:
//! valem (ou não) junto com o resto dela.

use crate::tabela::{Coluna, DefIndice, DefTabela, Padrao, Tabela};
use crate::valor::{Tipo, Valor, codificar_linha, decodificar_linha};
use crate::{ErroCatalogo, Resultado};
use siouxdb_indices::arvore::{self, Cursor};
use siouxdb_transacoes::{Paginas, PaginasMut};

const CHAVE_PROXIMO_ID: &[u8] = b"\x00proximo_id";
const PREFIXO_TABELA: u8 = 1;
const RUIM: ErroCatalogo = ErroCatalogo::Corrompido("entrada do catálogo com forma inesperada");

/// Prefixo reservado para as tabelas do sistema.
pub const PREFIXO_SISTEMA: &str = "sys_";

/// Maior tamanho de um nome de tabela ou coluna.
pub const TAMANHO_MAXIMO_NOME: usize = 63;

/// Põe o nome na forma guardada (minúsculas) e confere as regras: de 1 a 63 caracteres,
/// letras sem acento, dígitos e `_`, sem começar por dígito.
pub fn normalizar_nome(nome: &str) -> Resultado<String> {
    let n = nome.trim().to_ascii_lowercase();
    let primeira_ok = n
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b == b'_');
    let resto_ok = n
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if !primeira_ok || !resto_ok || n.len() > TAMANHO_MAXIMO_NOME {
        return Err(ErroCatalogo::NomeInvalido(format!(
            "\"{nome}\" (use letras sem acento, dígitos e _, sem começar por dígito, até {TAMANHO_MAXIMO_NOME} caracteres)"
        )));
    }
    Ok(n)
}

fn chave_tabela(nome: &str) -> Vec<u8> {
    let mut k = vec![PREFIXO_TABELA];
    k.extend_from_slice(nome.as_bytes());
    k
}

fn codificar_def(d: &DefTabela) -> Vec<u8> {
    let mut v = vec![
        Valor::Inteiro(d.id as i64),
        Valor::Texto(d.nome.clone()),
        Valor::Inteiro(d.cabecalho as i64),
        Valor::Inteiro(d.colunas.len() as i64),
    ];
    for c in &d.colunas {
        v.extend([
            Valor::Texto(c.nome.clone()),
            Valor::Inteiro(c.tipo.codigo()),
            Valor::Booleano(c.nao_nulo),
        ]);
    }
    v.push(Valor::Inteiro(d.chave.len() as i64));
    v.extend(d.chave.iter().map(|&i| Valor::Inteiro(i as i64)));
    v.push(Valor::Inteiro(d.indices.len() as i64));
    for i in &d.indices {
        v.extend([
            Valor::Texto(i.nome.clone()),
            Valor::Booleano(i.unico),
            Valor::Inteiro(i.raiz as i64),
            Valor::Inteiro(i.colunas.len() as i64),
        ]);
        v.extend(i.colunas.iter().map(|&c| Valor::Inteiro(c as i64)));
    }
    // Extensões: numeração automática e valor padrão de cada coluna.
    v.push(Valor::Inteiro(d.auto.map_or(-1, |a| a as i64)));
    for c in &d.colunas {
        match &c.padrao {
            Padrao::Nenhum => v.push(Valor::Inteiro(0)),
            Padrao::Valor(x) => v.extend([Valor::Inteiro(1), x.clone()]),
            Padrao::Agora => v.push(Valor::Inteiro(2)),
        }
    }
    codificar_linha(&v)
}

fn decodificar_def(b: &[u8]) -> Resultado<DefTabela> {
    let v = decodificar_linha(b)?;
    let mut it = v.into_iter();
    let inteiro = |it: &mut std::vec::IntoIter<Valor>| match it.next() {
        Some(Valor::Inteiro(i)) if i >= 0 => Ok(i as u64),
        _ => Err(RUIM),
    };
    let id = inteiro(&mut it)?;
    let Some(Valor::Texto(nome)) = it.next() else {
        return Err(RUIM);
    };
    let cabecalho = inteiro(&mut it)?;
    let n = inteiro(&mut it)? as usize;
    let mut colunas = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        let Some(Valor::Texto(nome)) = it.next() else {
            return Err(RUIM);
        };
        let tipo = Tipo::de_codigo(inteiro(&mut it)? as i64).ok_or(RUIM)?;
        let Some(Valor::Booleano(nao_nulo)) = it.next() else {
            return Err(RUIM);
        };
        colunas.push(Coluna {
            nome,
            tipo,
            nao_nulo,
            padrao: Padrao::Nenhum,
        });
    }
    let k = inteiro(&mut it)? as usize;
    let mut chave = Vec::with_capacity(k.min(4096));
    for _ in 0..k {
        let i = inteiro(&mut it)? as usize;
        if i >= colunas.len() {
            return Err(RUIM);
        }
        chave.push(i);
    }
    let mut indices = Vec::new();
    if let Some(Valor::Inteiro(n)) = it.next() {
        for _ in 0..n.max(0) {
            let Some(Valor::Texto(nome)) = it.next() else {
                return Err(RUIM);
            };
            let Some(Valor::Booleano(unico)) = it.next() else {
                return Err(RUIM);
            };
            let raiz = inteiro(&mut it)?;
            let m = inteiro(&mut it)? as usize;
            let mut cols = Vec::with_capacity(m.min(64));
            for _ in 0..m {
                let c = inteiro(&mut it)? as usize;
                if c >= colunas.len() {
                    return Err(RUIM);
                }
                cols.push(c);
            }
            indices.push(DefIndice {
                nome,
                colunas: cols,
                unico,
                raiz,
            });
        }
    }
    let mut auto = None;
    if let Some(Valor::Inteiro(a)) = it.next() {
        auto = usize::try_from(a).ok().filter(|a| *a < colunas.len());
        for c in &mut colunas {
            c.padrao = match it.next() {
                Some(Valor::Inteiro(0)) => Padrao::Nenhum,
                Some(Valor::Inteiro(1)) => Padrao::Valor(it.next().ok_or(RUIM)?),
                Some(Valor::Inteiro(2)) => Padrao::Agora,
                _ => return Err(RUIM),
            };
        }
    }
    if it.next().is_some() || chave.is_empty() {
        return Err(RUIM);
    }
    Ok(DefTabela {
        id,
        nome,
        colunas,
        chave,
        cabecalho,
        indices,
        auto,
    })
}

/// Cria uma tabela. `chave` são os nomes das colunas da chave primária, na ordem.
pub fn criar_tabela<P: PaginasMut + ?Sized>(
    tx: &mut P,
    nome: &str,
    colunas: &[Coluna],
    chave: &[&str],
) -> Resultado<Tabela> {
    criar_tabela_com(tx, nome, colunas, chave, None)
}

/// O valor padrão precisa ser do tipo da coluna.
fn conferir_padrao(c: &Coluna) -> Resultado<()> {
    let ok = match &c.padrao {
        Padrao::Nenhum => true,
        Padrao::Agora => c.tipo == Tipo::DataHora,
        Padrao::Valor(v) => v.tipo().is_none_or(|t| t == c.tipo),
    };
    if ok {
        Ok(())
    } else {
        Err(ErroCatalogo::DefinicaoInvalida(format!(
            "o valor padrão da coluna \"{}\" não é {}",
            c.nome,
            c.tipo.nome()
        )))
    }
}

/// Como [`criar_tabela`], com uma coluna inteira de numeração automática (`auto`): quando a
/// gravação não informa o valor, ela recebe o maior já usado mais um.
pub fn criar_tabela_com<P: PaginasMut + ?Sized>(
    tx: &mut P,
    nome: &str,
    colunas: &[Coluna],
    chave: &[&str],
    auto: Option<&str>,
) -> Resultado<Tabela> {
    let nome = normalizar_nome(nome)?;
    if nome.starts_with(PREFIXO_SISTEMA) {
        return Err(ErroCatalogo::NomeInvalido(format!(
            "\"{nome}\": o prefixo {PREFIXO_SISTEMA} é reservado ao sistema"
        )));
    }
    if colunas.is_empty() {
        return Err(ErroCatalogo::DefinicaoInvalida(
            "a tabela precisa de ao menos uma coluna".into(),
        ));
    }
    let mut cols: Vec<Coluna> = Vec::with_capacity(colunas.len());
    for c in colunas {
        let n = normalizar_nome(&c.nome)?;
        if cols.iter().any(|x| x.nome == n) {
            return Err(ErroCatalogo::DefinicaoInvalida(format!(
                "coluna \"{n}\" repetida"
            )));
        }
        conferir_padrao(c)?;
        cols.push(Coluna {
            nome: n,
            tipo: c.tipo,
            nao_nulo: c.nao_nulo,
            padrao: c.padrao.clone(),
        });
    }
    let auto = match auto {
        None => None,
        Some(a) => {
            let n = normalizar_nome(a)?;
            let i = cols.iter().position(|c| c.nome == n).ok_or_else(|| {
                ErroCatalogo::DefinicaoInvalida(format!("a coluna automática \"{n}\" não existe"))
            })?;
            if cols[i].tipo != Tipo::Inteiro {
                return Err(ErroCatalogo::DefinicaoInvalida(format!(
                    "a coluna automática \"{n}\" precisa ser INTEIRO"
                )));
            }
            Some(i)
        }
    };
    if chave.is_empty() {
        return Err(ErroCatalogo::DefinicaoInvalida(
            "a tabela precisa de chave primária".into(),
        ));
    }
    let mut posicoes = Vec::with_capacity(chave.len());
    for k in chave {
        let n = normalizar_nome(k)?;
        let i = cols.iter().position(|c| c.nome == n).ok_or_else(|| {
            ErroCatalogo::DefinicaoInvalida(format!("a coluna de chave \"{n}\" não existe"))
        })?;
        if posicoes.contains(&i) {
            return Err(ErroCatalogo::DefinicaoInvalida(format!(
                "coluna \"{n}\" repetida na chave"
            )));
        }
        cols[i].nao_nulo = true;
        posicoes.push(i);
    }

    let mut raiz = tx.meta().raiz_catalogo;
    if raiz == 0 {
        raiz = arvore::criar(tx)?;
        tx.definir_raiz_catalogo(raiz);
    }
    let k = chave_tabela(&nome);
    if arvore::obter_com(tx, raiz, &k, |_| ())?.is_some() {
        return Err(ErroCatalogo::TabelaJaExiste(nome));
    }
    let id = match arvore::obter(tx, raiz, CHAVE_PROXIMO_ID)? {
        Some(b) => u64::from_le_bytes(b.as_slice().try_into().map_err(|_| RUIM)?),
        None => 1,
    };
    arvore::inserir(tx, raiz, CHAVE_PROXIMO_ID, &(id + 1).to_le_bytes())?;
    let cabecalho = Tabela::criar_paginas(tx)?;
    let def = DefTabela {
        id,
        nome,
        colunas: cols,
        chave: posicoes,
        cabecalho,
        indices: Vec::new(),
        auto,
    };
    arvore::inserir(tx, raiz, &k, &codificar_def(&def)).map_err(|e| {
        match ErroCatalogo::from(e) {
            ErroCatalogo::Indice(siouxdb_indices::ErroIndice::EntradaGrandeDemais { .. }) => {
                ErroCatalogo::DefinicaoInvalida(
                    "colunas demais (ou nomes longos demais) para este tamanho de página".into(),
                )
            }
            outro => outro,
        }
    })?;
    Ok(Tabela::nova(def))
}

/// Abre uma tabela pelo nome.
pub fn tabela<P: Paginas + ?Sized>(p: &P, nome: &str) -> Resultado<Option<Tabela>> {
    let raiz = p.meta().raiz_catalogo;
    if raiz == 0 {
        return Ok(None);
    }
    let Ok(nome) = normalizar_nome(nome) else {
        return Ok(None);
    };
    arvore::obter_com(p, raiz, &chave_tabela(&nome), decodificar_def)?
        .transpose()
        .map(|d| d.map(Tabela::nova))
}

/// Abre uma tabela pelo nome; erro se não existir.
pub fn tabela_obrigatoria<P: Paginas + ?Sized>(p: &P, nome: &str) -> Resultado<Tabela> {
    tabela(p, nome)?.ok_or_else(|| ErroCatalogo::TabelaNaoExiste(nome.trim().to_ascii_lowercase()))
}

/// Todas as tabelas, em ordem de nome.
pub fn tabelas<P: Paginas + ?Sized>(p: &P) -> Resultado<Vec<Tabela>> {
    let raiz = p.meta().raiz_catalogo;
    if raiz == 0 {
        return Ok(Vec::new());
    }
    let mut c = Cursor::novo(
        p,
        raiz,
        Some(&[PREFIXO_TABELA]),
        Some(&[PREFIXO_TABELA + 1]),
    )?;
    let mut v = Vec::new();
    while let Some(def) = c.proximo_com(|_, b| decodificar_def(b))? {
        v.push(Tabela::nova(def?));
    }
    Ok(v)
}

/// Apaga a tabela e devolve as páginas dela ao banco. Erro se não existir.
pub fn remover_tabela<P: PaginasMut + ?Sized>(tx: &mut P, nome: &str) -> Resultado<()> {
    let t = tabela_obrigatoria(tx, nome)?;
    t.liberar_paginas(tx)?;
    arvore::remover(tx, tx.meta().raiz_catalogo, &chave_tabela(&t.def().nome))?;
    Ok(())
}

/// Colunas da visão `sys_tabelas`.
pub fn colunas_sys_tabelas() -> Vec<Coluna> {
    vec![
        Coluna::obrigatoria("nome", Tipo::Texto),
        Coluna::obrigatoria("id", Tipo::Inteiro),
        Coluna::obrigatoria("colunas", Tipo::Inteiro),
        Coluna::obrigatoria("linhas", Tipo::Inteiro),
        Coluna::obrigatoria("versoes", Tipo::Inteiro),
    ]
}

/// Linhas da visão `sys_tabelas`: uma por tabela.
pub fn sys_tabelas<P: Paginas + ?Sized>(p: &P) -> Resultado<Vec<Vec<Valor>>> {
    tabelas(p)?
        .iter()
        .map(|t| {
            let c = t.contagem(p)?;
            Ok(vec![
                Valor::Texto(t.def().nome.clone()),
                Valor::Inteiro(t.def().id as i64),
                Valor::Inteiro(t.def().colunas.len() as i64),
                Valor::Inteiro(c.linhas as i64),
                Valor::Inteiro(c.versoes as i64),
            ])
        })
        .collect()
}

/// Colunas da visão `sys_colunas`.
pub fn colunas_sys_colunas() -> Vec<Coluna> {
    vec![
        Coluna::obrigatoria("tabela", Tipo::Texto),
        Coluna::obrigatoria("posicao", Tipo::Inteiro),
        Coluna::obrigatoria("nome", Tipo::Texto),
        Coluna::obrigatoria("tipo", Tipo::Texto),
        Coluna::obrigatoria("nao_nulo", Tipo::Booleano),
        Coluna::nova("posicao_na_chave", Tipo::Inteiro),
    ]
}

/// Linhas da visão `sys_colunas`: uma por coluna de cada tabela.
pub fn sys_colunas<P: Paginas + ?Sized>(p: &P) -> Resultado<Vec<Vec<Valor>>> {
    let mut v = Vec::new();
    for t in tabelas(p)? {
        let d = t.def();
        for (i, c) in d.colunas.iter().enumerate() {
            v.push(vec![
                Valor::Texto(d.nome.clone()),
                Valor::Inteiro(i as i64 + 1),
                Valor::Texto(c.nome.clone()),
                Valor::Texto(c.tipo.nome().to_string()),
                Valor::Booleano(c.nao_nulo),
                d.chave
                    .iter()
                    .position(|&k| k == i)
                    .map_or(Valor::Nulo, |k| Valor::Inteiro(k as i64 + 1)),
            ]);
        }
    }
    Ok(v)
}

/// Cria um índice secundário em `tabela`, sobre as colunas dadas, e o preenche com as linhas
/// atuais. `unico` recusa valores repetidos (nulos não contam).
pub fn criar_indice<P: PaginasMut + ?Sized>(
    tx: &mut P,
    nome: &str,
    tabela: &str,
    colunas: &[&str],
    unico: bool,
) -> Resultado<()> {
    let nome = normalizar_nome(nome)?;
    if tabelas(tx)?
        .iter()
        .any(|t| t.def().indices.iter().any(|i| i.nome == nome))
    {
        return Err(ErroCatalogo::IndiceJaExiste(nome));
    }
    let t = tabela_obrigatoria(tx, tabela)?;
    if colunas.is_empty() {
        return Err(ErroCatalogo::DefinicaoInvalida(
            "o índice precisa de ao menos uma coluna".into(),
        ));
    }
    let mut posicoes = Vec::new();
    for c in colunas {
        let n = normalizar_nome(c)?;
        let i = t
            .def()
            .colunas
            .iter()
            .position(|x| x.nome == n)
            .ok_or_else(|| {
                ErroCatalogo::DefinicaoInvalida(format!("a coluna \"{n}\" não existe"))
            })?;
        if posicoes.contains(&i) {
            return Err(ErroCatalogo::DefinicaoInvalida(format!(
                "coluna \"{n}\" repetida no índice"
            )));
        }
        posicoes.push(i);
    }
    let raiz = arvore::criar(tx)?;
    let idx = DefIndice {
        nome,
        colunas: posicoes,
        unico,
        raiz,
    };
    t.preencher_indice(tx, &idx)?;
    let mut def = t.def().clone();
    def.indices.push(idx);
    arvore::inserir(
        tx,
        tx.meta().raiz_catalogo,
        &chave_tabela(&def.nome),
        &codificar_def(&def),
    )?;
    Ok(())
}

/// Apaga um índice pelo nome.
pub fn remover_indice<P: PaginasMut + ?Sized>(tx: &mut P, nome: &str) -> Resultado<()> {
    let nome = normalizar_nome(nome)?;
    for t in tabelas(tx)? {
        if let Some(pos) = t.def().indices.iter().position(|i| i.nome == nome) {
            let mut def = t.def().clone();
            let idx = def.indices.remove(pos);
            arvore::liberar(tx, idx.raiz)?;
            arvore::inserir(
                tx,
                tx.meta().raiz_catalogo,
                &chave_tabela(&def.nome),
                &codificar_def(&def),
            )?;
            return Ok(());
        }
    }
    Err(ErroCatalogo::IndiceNaoExiste(nome))
}

/// Colunas da visão `sys_indices`.
pub fn colunas_sys_indices() -> Vec<Coluna> {
    vec![
        Coluna::obrigatoria("tabela", Tipo::Texto),
        Coluna::obrigatoria("nome", Tipo::Texto),
        Coluna::obrigatoria("colunas", Tipo::Texto),
        Coluna::obrigatoria("unico", Tipo::Booleano),
    ]
}

/// Linhas da visão `sys_indices`: uma por índice secundário.
pub fn sys_indices<P: Paginas + ?Sized>(p: &P) -> Resultado<Vec<Vec<Valor>>> {
    let mut v = Vec::new();
    for t in tabelas(p)? {
        let d = t.def();
        for i in &d.indices {
            let cols: Vec<&str> = i
                .colunas
                .iter()
                .map(|&c| d.colunas[c].nome.as_str())
                .collect();
            v.push(vec![
                Valor::Texto(d.nome.clone()),
                Valor::Texto(i.nome.clone()),
                Valor::Texto(cols.join(", ")),
                Valor::Booleano(i.unico),
            ]);
        }
    }
    Ok(v)
}

fn regravar_def<P: PaginasMut + ?Sized>(tx: &mut P, def: &DefTabela) -> Resultado<()> {
    arvore::inserir(
        tx,
        tx.meta().raiz_catalogo,
        &chave_tabela(&def.nome),
        &codificar_def(def),
    )
    .map_err(|e| match ErroCatalogo::from(e) {
        ErroCatalogo::Indice(siouxdb_indices::ErroIndice::EntradaGrandeDemais { .. }) => {
            ErroCatalogo::DefinicaoInvalida(
                "colunas demais (ou nomes longos demais) para este tamanho de página".into(),
            )
        }
        outro => outro,
    })?;
    Ok(())
}

/// Acrescenta uma coluna no fim da tabela. As linhas que já existem passam a mostrar o valor
/// padrão dela (ou nulo); nada é regravado. Coluna obrigatória precisa de valor padrão fixo.
pub fn adicionar_coluna<P: PaginasMut + ?Sized>(
    tx: &mut P,
    tabela: &str,
    coluna: &Coluna,
) -> Resultado<()> {
    let t = tabela_obrigatoria(tx, tabela)?;
    let nome = normalizar_nome(&coluna.nome)?;
    if t.def().colunas.iter().any(|c| c.nome == nome) {
        return Err(ErroCatalogo::DefinicaoInvalida(format!(
            "a coluna \"{nome}\" já existe"
        )));
    }
    conferir_padrao(coluna)?;
    if coluna.nao_nulo && !matches!(&coluna.padrao, Padrao::Valor(v) if !matches!(v, Valor::Nulo)) {
        return Err(ErroCatalogo::DefinicaoInvalida(format!(
            "a coluna obrigatória \"{nome}\" precisa de um valor padrão fixo para as linhas que já existem"
        )));
    }
    let mut def = t.def().clone();
    def.colunas.push(Coluna {
        nome,
        tipo: coluna.tipo,
        nao_nulo: coluna.nao_nulo,
        padrao: coluna.padrao.clone(),
    });
    regravar_def(tx, &def)
}

/// Troca o nome de uma coluna (só o catálogo muda).
pub fn renomear_coluna<P: PaginasMut + ?Sized>(
    tx: &mut P,
    tabela: &str,
    de: &str,
    para: &str,
) -> Resultado<()> {
    let t = tabela_obrigatoria(tx, tabela)?;
    let (de, para) = (normalizar_nome(de)?, normalizar_nome(para)?);
    let mut def = t.def().clone();
    if def.colunas.iter().any(|c| c.nome == para) {
        return Err(ErroCatalogo::DefinicaoInvalida(format!(
            "a coluna \"{para}\" já existe"
        )));
    }
    let c = def
        .colunas
        .iter_mut()
        .find(|c| c.nome == de)
        .ok_or_else(|| ErroCatalogo::DefinicaoInvalida(format!("a coluna \"{de}\" não existe")))?;
    c.nome = para;
    regravar_def(tx, &def)
}

/// Troca o nome de uma tabela (só o catálogo muda).
pub fn renomear_tabela<P: PaginasMut + ?Sized>(tx: &mut P, de: &str, para: &str) -> Resultado<()> {
    let t = tabela_obrigatoria(tx, de)?;
    let para = normalizar_nome(para)?;
    if para.starts_with(PREFIXO_SISTEMA) {
        return Err(ErroCatalogo::NomeInvalido(format!(
            "\"{para}\": o prefixo {PREFIXO_SISTEMA} é reservado ao sistema"
        )));
    }
    if tabela(tx, &para)?.is_some() {
        return Err(ErroCatalogo::TabelaJaExiste(para));
    }
    let mut def = t.def().clone();
    arvore::remover(tx, tx.meta().raiz_catalogo, &chave_tabela(&def.nome))?;
    def.nome = para;
    regravar_def(tx, &def)
}
