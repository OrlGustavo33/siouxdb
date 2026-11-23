//! Planejador simples e execução.

use crate::avaliar::{
    Ctx, avaliar, conferir_colunas, decimal_de, ordem_total, passa, resolver_agregados,
    tem_agregado, tem_coluna,
};
use crate::erro::semantica;
use crate::parser::{ColDef, Comando, Conflito, Expr, Item, Op, Selecao, analisar};
use crate::{Resultado, Saida};
use siouxdb_catalogo::catalogo;
use siouxdb_catalogo::valor::codificar_chave;
use siouxdb_catalogo::{
    Coluna, Decimal, ErroCatalogo, Padrao, Tabela, Tipo, Valor, datahora, fim_do_prefixo,
};
use siouxdb_transacoes::{Paginas, PaginasMut};

/// Como as linhas da tabela vão ser lidas.
enum Acesso {
    /// Uma linha, pela chave primária completa.
    Ponto(Vec<u8>),
    /// Faixa da chave primária.
    FaixaChave(Option<Vec<u8>>, Option<Vec<u8>>),
    /// Faixa de um índice secundário (posição do índice).
    Indice(usize, Option<Vec<u8>>, Option<Vec<u8>>),
    /// A tabela toda.
    Tudo,
}

fn descrever(a: &Acesso, t: &Tabela) -> String {
    match a {
        Acesso::Ponto(_) => "busca pela chave primária".into(),
        Acesso::FaixaChave(..) => "faixa da chave primária".into(),
        Acesso::Indice(i, ..) => format!("índice {}", t.def().indices[*i].nome),
        Acesso::Tudo => "varredura completa".into(),
    }
}

fn conjuncoes(e: &Expr) -> Vec<&Expr> {
    match e {
        Expr::Bin(Op::E, a, b) => {
            let mut v = conjuncoes(a);
            v.extend(conjuncoes(b));
            v
        }
        outro => vec![outro],
    }
}

/// Valor de uma expressão sem colunas (literal, parâmetro, conta).
fn constante(e: &Expr, params: &[Valor]) -> Option<Valor> {
    if tem_coluna(e) || tem_agregado(e) {
        return None;
    }
    avaliar(
        e,
        &Ctx {
            nomes: &[],
            linha: &[],
            params,
        },
    )
    .ok()
}

/// O valor no tipo da coluna, para entrar na chave codificada (só se não mudar a ordem).
fn no_tipo(v: Valor, tipo: Tipo) -> Option<Valor> {
    match (v, tipo) {
        (Valor::Inteiro(i), Tipo::Real) => Some(Valor::Real(i as f64)),
        (v @ (Valor::Inteiro(_) | Valor::Real(_)), Tipo::Decimal) => {
            decimal_de(&v).map(Valor::Decimal)
        }
        (Valor::Texto(t), Tipo::DataHora) => datahora::de_texto(&t).map(Valor::DataHora),
        (v, t) if v.tipo() == Some(t) => Some(v),
        _ => None,
    }
}

/// Predicados simples "coluna op constante" das conjunções do WHERE.
fn predicados(t: &Tabela, onde: Option<&Expr>, params: &[Valor]) -> Vec<(usize, Op, Valor)> {
    let mut v = Vec::new();
    let Some(onde) = onde else { return v };
    let posicao = |c: &str| t.def().colunas.iter().position(|x| x.nome == c);
    let inverter = |op: Op| match op {
        Op::Menor => Op::Maior,
        Op::MenorIgual => Op::MaiorIgual,
        Op::Maior => Op::Menor,
        Op::MaiorIgual => Op::MenorIgual,
        o => o,
    };
    for c in conjuncoes(onde) {
        let mut empurrar = |col: &str, op: Op, e: &Expr| {
            if let (Some(i), Some(k)) = (posicao(col), constante(e, params)) {
                if let Some(k) = no_tipo(k, t.def().colunas[i].tipo) {
                    v.push((i, op, k));
                }
            }
        };
        match c {
            Expr::Bin(
                op @ (Op::Igual | Op::Menor | Op::MenorIgual | Op::Maior | Op::MaiorIgual),
                a,
                b,
            ) => match (&**a, &**b) {
                (Expr::Coluna(col), k) => empurrar(col, *op, k),
                (k, Expr::Coluna(col)) => empurrar(col, inverter(*op), k),
                _ => {}
            },
            Expr::Entre(a, de, ate, false) => {
                if let Expr::Coluna(col) = &**a {
                    empurrar(col, Op::MaiorIgual, de);
                    empurrar(col, Op::MenorIgual, ate);
                }
            }
            _ => {}
        }
    }
    v
}

/// Limites de bytes para as colunas `cols` (de um índice ou da chave): prefixo de
/// igualdades + faixa na coluna seguinte. Devolve (quantas igualdades, tem faixa, início, fim).
#[allow(clippy::type_complexity)]
fn limites(
    cols: &[usize],
    preds: &[(usize, Op, Valor)],
) -> Option<(usize, bool, Option<Vec<u8>>, Option<Vec<u8>>)> {
    let mut iguais = Vec::new();
    for c in cols {
        match preds.iter().find(|(p, op, _)| p == c && *op == Op::Igual) {
            Some((_, _, v)) => iguais.push(v.clone()),
            None => break,
        }
    }
    let prefixo = codificar_chave(&iguais).ok()?;
    let mut inicio = (!iguais.is_empty()).then(|| prefixo.clone());
    let mut fim = (!iguais.is_empty()).then(|| fim_do_prefixo(&prefixo));
    let mut faixa = false;
    if let Some(prox) = cols.get(iguais.len()) {
        for (_, op, v) in preds.iter().filter(|(p, ..)| p == prox) {
            let mut k = prefixo.clone();
            k.extend(codificar_chave(std::slice::from_ref(v)).ok()?);
            match op {
                Op::MaiorIgual if inicio.as_ref().is_none_or(|i| *i <= k) => inicio = Some(k),
                Op::Maior => inicio = Some(fim_do_prefixo(&k)),
                Op::Menor => fim = Some(k),
                Op::MenorIgual => fim = Some(fim_do_prefixo(&k)),
                _ => continue,
            }
            faixa = true;
        }
    }
    if iguais.is_empty() && !faixa {
        return None;
    }
    Some((iguais.len(), faixa, inicio, fim))
}

fn planejar(t: &Tabela, onde: Option<&Expr>, params: &[Valor], lsn: u64) -> Acesso {
    let preds = predicados(t, onde, params);
    if preds.is_empty() {
        return Acesso::Tudo;
    }
    let d = t.def();
    let mut melhor: Option<(usize, Acesso)> = None;
    if let Some((iguais, faixa, ini, fim)) = limites(&d.chave, &preds) {
        if iguais == d.chave.len() {
            return Acesso::Ponto(ini.expect("prefixo com todas as colunas"));
        }
        melhor = Some((
            iguais * 2 + usize::from(faixa),
            Acesso::FaixaChave(ini, fim),
        ));
    }
    // Índice secundário guarda o estado atual: não serve para "como estava".
    if lsn == u64::MAX {
        for (i, idx) in d.indices.iter().enumerate() {
            if let Some((iguais, faixa, ini, fim)) = limites(&idx.colunas, &preds) {
                let nota = iguais * 2 + usize::from(faixa);
                if melhor.as_ref().is_none_or(|(m, _)| nota > *m) {
                    melhor = Some((nota, Acesso::Indice(i, ini, fim)));
                }
            }
        }
    }
    melhor.map_or(Acesso::Tudo, |(_, a)| a)
}

/// Passa cada linha (antes do filtro) para `f`; `f` devolve `false` para parar.
fn percorrer<P: Paginas + ?Sized>(
    p: &P,
    t: &Tabela,
    acesso: &Acesso,
    lsn: u64,
    f: &mut dyn FnMut(Vec<Valor>) -> Resultado<bool>,
) -> Resultado<()> {
    match acesso {
        Acesso::Ponto(k) => {
            if let Some(l) = t.obter_por_bytes(p, k, lsn)? {
                f(l)?;
            }
        }
        Acesso::FaixaChave(..) | Acesso::Tudo => {
            let (ini, fim) = match acesso {
                Acesso::FaixaChave(i, f) => (i.as_deref(), f.as_deref()),
                _ => (None, None),
            };
            let mut v = t.varrer_bytes(p, ini, fim, lsn)?;
            while let Some(l) = v.proxima()? {
                if !f(l)? {
                    break;
                }
            }
        }
        Acesso::Indice(i, ini, fim) => {
            for pk in t.buscar_no_indice(p, *i, ini.as_deref(), fim.as_deref())? {
                if let Some(l) = t.obter_por_bytes(p, &pk, lsn)? {
                    if !f(l)? {
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

fn inteiro_constante(e: &Option<Expr>, params: &[Valor], o_que: &str) -> Resultado<Option<u64>> {
    match e {
        None => Ok(None),
        Some(e) => match constante(e, params) {
            Some(Valor::Inteiro(i)) if i >= 0 => Ok(Some(i as u64)),
            _ => semantica(format!("{o_que} precisa ser um inteiro >= 0")),
        },
    }
}

/// Fonte de linhas de um SELECT: nomes das colunas e as linhas (já filtradas).
fn fonte<P: Paginas + ?Sized>(
    p: &P,
    s: &Selecao,
    params: &[Valor],
    parar_em: Option<usize>,
) -> Resultado<(Vec<String>, Vec<Vec<Valor>>, String)> {
    let lsn = inteiro_constante(&s.como_estava, params, "AS OF")?.unwrap_or(u64::MAX);
    let Some(nome) = &s.tabela else {
        return Ok((Vec::new(), vec![Vec::new()], "sem tabela".into()));
    };
    let sistema = match nome.as_str() {
        "sys_tabelas" => Some((catalogo::colunas_sys_tabelas(), catalogo::sys_tabelas(p)?)),
        "sys_colunas" => Some((catalogo::colunas_sys_colunas(), catalogo::sys_colunas(p)?)),
        "sys_indices" => Some((catalogo::colunas_sys_indices(), catalogo::sys_indices(p)?)),
        _ => None,
    };
    if let Some((cols, linhas)) = sistema {
        let nomes: Vec<String> = cols.into_iter().map(|c| c.nome).collect();
        let mut v = Vec::new();
        for l in linhas {
            if s.onde.as_ref().map_or(Ok(true), |o| {
                passa(
                    o,
                    &Ctx {
                        nomes: &nomes,
                        linha: &l,
                        params,
                    },
                )
            })? {
                v.push(l);
            }
        }
        return Ok((nomes, v, "tabela do sistema".into()));
    }
    let t = catalogo::tabela_obrigatoria(p, nome)?;
    let nomes: Vec<String> = t.def().colunas.iter().map(|c| c.nome.clone()).collect();
    if let Some(o) = &s.onde {
        conferir_colunas(o, &nomes, &[])?;
    }
    let acesso = planejar(&t, s.onde.as_ref(), params, lsn);
    let plano = descrever(&acesso, &t);
    let mut linhas = Vec::new();
    let mut erro = None;
    percorrer(p, &t, &acesso, lsn, &mut |l| {
        match s.onde.as_ref().map_or(Ok(true), |o| {
            passa(
                o,
                &Ctx {
                    nomes: &nomes,
                    linha: &l,
                    params,
                },
            )
        }) {
            Ok(true) => linhas.push(l),
            Ok(false) => {}
            Err(e) => {
                erro = Some(e);
                return Ok(false);
            }
        }
        Ok(parar_em.is_none_or(|n| linhas.len() < n))
    })?;
    if let Some(e) = erro {
        return Err(e);
    }
    Ok((nomes, linhas, plano))
}

fn nome_saida(e: &Expr, apelido: &Option<String>, i: usize) -> String {
    if let Some(a) = apelido {
        return a.clone();
    }
    match e {
        Expr::Coluna(c) => c.clone(),
        Expr::Funcao(f, ..) => f.clone(),
        _ => format!("coluna{}", i + 1),
    }
}

fn selecionar<P: Paginas + ?Sized>(
    p: &P,
    s: &Selecao,
    params: &[Valor],
) -> Resultado<(Saida, String)> {
    let agregado = !s.agrupar.is_empty()
        || s.tendo.is_some()
        || s.itens
            .iter()
            .any(|i| matches!(i, Item::Expr(e, _) if tem_agregado(e)));
    let limite = inteiro_constante(&s.limite, params, "LIMIT")?;
    let deslocamento = inteiro_constante(&s.deslocamento, params, "OFFSET")?.unwrap_or(0);
    // Sem ordenação nem agregação, dá para parar de ler cedo.
    let parar_em = (!agregado && s.ordem.is_empty())
        .then_some(limite.map(|l| (l + deslocamento) as usize))
        .flatten();
    let (nomes, linhas, plano) = fonte(p, s, params, parar_em)?;

    let mut colunas = Vec::new();
    let mut exprs: Vec<Expr> = Vec::new();
    for e in s.itens.iter().filter_map(|i| match i {
        Item::Expr(e, _) => Some(e),
        Item::Tudo => None,
    }) {
        conferir_colunas(e, &nomes, &[])?;
    }
    for e in s.agrupar.iter().chain(&s.tendo) {
        conferir_colunas(e, &nomes, &[])?;
    }
    for (i, item) in s.itens.iter().enumerate() {
        match item {
            Item::Tudo => {
                if agregado {
                    return semantica("SELECT * não combina com GROUP BY ou agregados");
                }
                for n in &nomes {
                    colunas.push(n.clone());
                    exprs.push(Expr::Coluna(n.clone()));
                }
            }
            Item::Expr(e, apelido) => {
                colunas.push(nome_saida(e, apelido, i));
                exprs.push(e.clone());
            }
        }
    }

    for (e, _) in &s.ordem {
        conferir_colunas(e, &nomes, &colunas)?;
    }
    // Cada registro: (valores de saída, chaves de ordenação).
    let mut registros: Vec<(Vec<Valor>, Vec<Valor>)> = Vec::new();
    let chave_ordem = |saida: &[Valor],
                       e: &Expr,
                       linha: &dyn Fn(&Expr) -> Resultado<Valor>|
     -> Resultado<Valor> {
        if let Expr::Valor(Valor::Inteiro(n)) = e {
            return saida
                .get((*n as usize).wrapping_sub(1))
                .cloned()
                .ok_or_else(|| {
                    crate::ErroSql::Semantica(format!(
                        "ORDER BY {n}: não existe a coluna {n} no resultado"
                    ))
                });
        }
        if let Expr::Coluna(c) = e {
            if let Some(i) = colunas.iter().position(|x| x == c) {
                if !nomes.contains(c) || agregado {
                    return Ok(saida[i].clone());
                }
            }
        }
        linha(e)
    };
    if agregado {
        // Grupos pela chave codificada dos valores do GROUP BY (em ordem de chave).
        let mut grupos: std::collections::BTreeMap<Vec<u8>, Vec<Vec<Valor>>> = Default::default();
        if s.agrupar.is_empty() {
            grupos.insert(Vec::new(), linhas);
        } else {
            for l in linhas {
                let ctx = Ctx {
                    nomes: &nomes,
                    linha: &l,
                    params,
                };
                let k: Vec<Valor> = s
                    .agrupar
                    .iter()
                    .map(|g| avaliar(g, &ctx))
                    .collect::<Resultado<_>>()?;
                let chave = codificar_chave(&k).unwrap_or_else(|_| format!("{k:?}").into_bytes());
                grupos.entry(chave).or_default().push(l);
            }
        }
        let vazia = vec![Valor::Nulo; nomes.len()];
        for (_, g) in grupos {
            let primeira = g.first().unwrap_or(&vazia).clone();
            let ctx = Ctx {
                nomes: &nomes,
                linha: &primeira,
                params,
            };
            let valor = |e: &Expr| -> Resultado<Valor> {
                avaliar(&resolver_agregados(e, &nomes, &g, params)?, &ctx)
            };
            let saida: Vec<Valor> = exprs.iter().map(&valor).collect::<Resultado<_>>()?;
            if let Some(t) = &s.tendo {
                if valor(t)? != Valor::Booleano(true) {
                    continue;
                }
            }
            let ordem = s
                .ordem
                .iter()
                .map(|(e, _)| chave_ordem(&saida, e, &valor))
                .collect::<Resultado<_>>()?;
            registros.push((saida, ordem));
        }
    } else {
        for l in &linhas {
            let ctx = Ctx {
                nomes: &nomes,
                linha: l,
                params,
            };
            let valor = |e: &Expr| avaliar(e, &ctx);
            let saida: Vec<Valor> = exprs.iter().map(valor).collect::<Resultado<_>>()?;
            let ordem = s
                .ordem
                .iter()
                .map(|(e, _)| chave_ordem(&saida, e, &valor))
                .collect::<Resultado<_>>()?;
            registros.push((saida, ordem));
        }
    }
    if !s.ordem.is_empty() {
        registros.sort_by(|a, b| {
            for (i, (_, desc)) in s.ordem.iter().enumerate() {
                let o = ordem_total(&a.1[i], &b.1[i]);
                let o = if *desc { o.reverse() } else { o };
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            std::cmp::Ordering::Equal
        });
    }
    let linhas: Vec<Vec<Valor>> = registros
        .into_iter()
        .skip(deslocamento as usize)
        .take(limite.map_or(usize::MAX, |l| l as usize))
        .map(|(s, _)| s)
        .collect();
    Ok((
        Saida {
            colunas,
            linhas,
            afetadas: 0,
            ultimo_id: None,
        },
        plano,
    ))
}

/// Coluna do catálogo a partir da definição do SQL (avalia o DEFAULT).
fn coluna_de(c: &ColDef, params: &[Valor]) -> Resultado<Coluna> {
    let mut col = Coluna {
        nome: c.nome.clone(),
        tipo: c.tipo,
        nao_nulo: c.nao_nulo,
        padrao: Padrao::Nenhum,
    };
    col.padrao = match &c.padrao {
        None => Padrao::Nenhum,
        Some(Expr::Funcao(f, args, _)) if args.is_empty() && (f == "agora" || f == "now") => {
            Padrao::Agora
        }
        Some(e) => match constante(e, params) {
            Some(v) => Padrao::Valor(coagir(v, &col)?),
            None => {
                return semantica(format!(
                    "o DEFAULT da coluna \"{}\" precisa ser um valor fixo ou AGORA()",
                    c.nome
                ));
            }
        },
    };
    Ok(col)
}

fn coagir(v: Valor, c: &Coluna) -> Resultado<Valor> {
    match (v, c.tipo) {
        (Valor::Nulo, _) => Ok(Valor::Nulo),
        (Valor::Inteiro(i), Tipo::Real) => Ok(Valor::Real(i as f64)),
        (Valor::Decimal(d), Tipo::Real) => Ok(Valor::Real(d.para_real())),
        (v @ (Valor::Inteiro(_) | Valor::Real(_)), Tipo::Decimal) => match decimal_de(&v) {
            Some(d) => Ok(Valor::Decimal(d)),
            None => semantica(format!(
                "valor fora do limite do DECIMAL na coluna \"{}\"",
                c.nome
            )),
        },
        // Texto para decimal ou data: útil para quem manda JSON e quer o valor exato.
        (Valor::Texto(t), Tipo::Decimal) => match Decimal::de_texto(&t) {
            Some(d) => Ok(Valor::Decimal(d)),
            None => semantica(format!(
                "\"{t}\" não é um número decimal (coluna \"{}\")",
                c.nome
            )),
        },
        (Valor::Texto(t), Tipo::DataHora) => match datahora::de_texto(&t) {
            Some(d) => Ok(Valor::DataHora(d)),
            None => semantica(format!(
                "\"{t}\" não é uma data (coluna \"{}\"; use AAAA-MM-DD ou AAAA-MM-DD HH:MM:SS)",
                c.nome
            )),
        },
        (v, t) if v.tipo() == Some(t) => Ok(v),
        (v, t) => semantica(format!(
            "a coluna \"{}\" é {} e recebeu {}",
            c.nome,
            t.nome(),
            v.tipo().map_or("NULL", |x| x.nome())
        )),
    }
}

fn linhas_que_casam<P: Paginas + ?Sized>(
    p: &P,
    t: &Tabela,
    onde: Option<&Expr>,
    params: &[Valor],
) -> Resultado<Vec<Vec<Valor>>> {
    let nomes: Vec<String> = t.def().colunas.iter().map(|c| c.nome.clone()).collect();
    if let Some(o) = onde {
        conferir_colunas(o, &nomes, &[])?;
    }
    let acesso = planejar(t, onde, params, u64::MAX);
    let mut v = Vec::new();
    let mut erro = None;
    percorrer(p, t, &acesso, u64::MAX, &mut |l| {
        match onde.map_or(Ok(true), |o| {
            passa(
                o,
                &Ctx {
                    nomes: &nomes,
                    linha: &l,
                    params,
                },
            )
        }) {
            Ok(true) => v.push(l),
            Ok(false) => {}
            Err(e) => {
                erro = Some(e);
                return Ok(false);
            }
        }
        Ok(true)
    })?;
    erro.map_or(Ok(v), Err)
}

fn sistema(nome: &str) -> Resultado<()> {
    if nome.starts_with(catalogo::PREFIXO_SISTEMA) {
        return semantica(format!("\"{nome}\" é uma tabela do sistema (só leitura)"));
    }
    Ok(())
}

pub(crate) fn executar_comando<P: PaginasMut + ?Sized>(
    tx: &mut P,
    c: &Comando,
    params: &[Valor],
) -> Resultado<Saida> {
    let afetadas = |n: u64| Saida {
        colunas: Vec::new(),
        linhas: Vec::new(),
        afetadas: n,
        ultimo_id: None,
    };
    match c {
        Comando::Selecionar(s) => Ok(selecionar(tx, s, params)?.0),
        Comando::Explicar(s) => {
            let (_, plano) = selecionar(tx, s, params)?;
            Ok(Saida {
                colunas: vec!["plano".into()],
                linhas: vec![vec![Valor::Texto(plano)]],
                afetadas: 0,
                ultimo_id: None,
            })
        }
        Comando::CriarTabela {
            nome,
            se_nao_existe,
            colunas,
            chave,
        } => {
            if *se_nao_existe && catalogo::tabela(tx, nome)?.is_some() {
                return Ok(afetadas(0));
            }
            let cols: Vec<Coluna> = colunas
                .iter()
                .map(|c| coluna_de(c, params))
                .collect::<Resultado<_>>()?;
            let autos: Vec<&str> = colunas
                .iter()
                .filter(|c| c.auto)
                .map(|c| c.nome.as_str())
                .collect();
            if autos.len() > 1 {
                return semantica("só uma coluna pode ser de numeração automática");
            }
            let chave: Vec<&str> = chave.iter().map(String::as_str).collect();
            catalogo::criar_tabela_com(tx, nome, &cols, &chave, autos.first().copied())?;
            Ok(afetadas(0))
        }
        Comando::AdicionarColuna { tabela, coluna } => {
            sistema(tabela)?;
            catalogo::adicionar_coluna(tx, tabela, &coluna_de(coluna, params)?)?;
            Ok(afetadas(0))
        }
        Comando::RenomearTabela { de, para } => {
            sistema(de)?;
            catalogo::renomear_tabela(tx, de, para)?;
            Ok(afetadas(0))
        }
        Comando::RenomearColuna { tabela, de, para } => {
            sistema(tabela)?;
            catalogo::renomear_coluna(tx, tabela, de, para)?;
            Ok(afetadas(0))
        }
        Comando::ApagarTabela { nome, se_existe } => {
            match catalogo::remover_tabela(tx, nome) {
                Err(ErroCatalogo::TabelaNaoExiste(_)) if *se_existe => {}
                r => r?,
            }
            Ok(afetadas(0))
        }
        Comando::CriarIndice {
            nome,
            tabela,
            colunas,
            unico,
            se_nao_existe,
        } => {
            sistema(tabela)?;
            let cols: Vec<&str> = colunas.iter().map(String::as_str).collect();
            match catalogo::criar_indice(tx, nome, tabela, &cols, *unico) {
                Err(ErroCatalogo::IndiceJaExiste(_)) if *se_nao_existe => {}
                r => r?,
            }
            Ok(afetadas(0))
        }
        Comando::ApagarIndice { nome, se_existe } => {
            match catalogo::remover_indice(tx, nome) {
                Err(ErroCatalogo::IndiceNaoExiste(_)) if *se_existe => {}
                r => r?,
            }
            Ok(afetadas(0))
        }
        Comando::Inserir {
            tabela,
            colunas,
            linhas,
            conflito,
        } => {
            sistema(tabela)?;
            let t = catalogo::tabela_obrigatoria(tx, tabela)?;
            let d = t.def();
            let posicoes: Vec<usize> = match colunas {
                None => (0..d.colunas.len()).collect(),
                Some(cs) => cs
                    .iter()
                    .map(|c| {
                        d.colunas.iter().position(|x| &x.nome == c).ok_or_else(|| {
                            crate::ErroSql::Semantica(format!("a tabela não tem a coluna \"{c}\""))
                        })
                    })
                    .collect::<Resultado<_>>()?,
            };
            let mut n = 0;
            let mut ultimo_id = None;
            for exprs in linhas {
                if exprs.len() != posicoes.len() {
                    return semantica(format!(
                        "{} coluna(s) e {} valor(es)",
                        posicoes.len(),
                        exprs.len()
                    ));
                }
                let mut parcial: Vec<Option<Valor>> = vec![None; d.colunas.len()];
                for (e, &pos) in exprs.iter().zip(&posicoes) {
                    let v = avaliar(
                        e,
                        &Ctx {
                            nomes: &[],
                            linha: &[],
                            params,
                        },
                    )?;
                    parcial[pos] = Some(coagir(v, &d.colunas[pos])?);
                }
                let linha = t.preencher(tx, parcial)?;
                if let Some(Valor::Inteiro(id)) = d.auto.map(|a| &linha[a]) {
                    ultimo_id = Some(*id);
                }
                match conflito {
                    Conflito::Erro => {
                        t.inserir(tx, &linha)?;
                        n += 1;
                    }
                    Conflito::Substituir => {
                        t.gravar(tx, &linha)?;
                        n += 1;
                    }
                    Conflito::Ignorar => match t.inserir(tx, &linha) {
                        Ok(()) => n += 1,
                        Err(ErroCatalogo::ChaveDuplicada) => {}
                        Err(e) => return Err(e.into()),
                    },
                }
            }
            Ok(Saida {
                ultimo_id,
                ..afetadas(n)
            })
        }
        Comando::Atualizar {
            tabela,
            mudancas,
            onde,
        } => {
            sistema(tabela)?;
            let t = catalogo::tabela_obrigatoria(tx, tabela)?;
            let d = t.def();
            let nomes: Vec<String> = d.colunas.iter().map(|c| c.nome.clone()).collect();
            let alvos: Vec<(usize, &Expr)> = mudancas
                .iter()
                .map(|(c, e)| {
                    nomes
                        .iter()
                        .position(|x| x == c)
                        .map(|i| (i, e))
                        .ok_or_else(|| {
                            crate::ErroSql::Semantica(format!("a tabela não tem a coluna \"{c}\""))
                        })
                })
                .collect::<Resultado<_>>()?;
            for (_, e) in &alvos {
                conferir_colunas(e, &nomes, &[])?;
            }
            let linhas = linhas_que_casam(tx, &t, onde.as_ref(), params)?;
            for antiga in &linhas {
                let mut nova = antiga.clone();
                for (i, e) in &alvos {
                    let v = avaliar(
                        e,
                        &Ctx {
                            nomes: &nomes,
                            linha: antiga,
                            params,
                        },
                    )?;
                    nova[*i] = coagir(v, &d.colunas[*i])?;
                }
                if t.chave_de(&nova) != t.chave_de(antiga) {
                    t.remover(tx, &t.chave_de(antiga))?;
                    t.inserir(tx, &nova)?;
                } else {
                    t.gravar(tx, &nova)?;
                }
            }
            Ok(afetadas(linhas.len() as u64))
        }
        Comando::Remover { tabela, onde } => {
            sistema(tabela)?;
            let t = catalogo::tabela_obrigatoria(tx, tabela)?;
            let linhas = linhas_que_casam(tx, &t, onde.as_ref(), params)?;
            for l in &linhas {
                t.remover(tx, &t.chave_de(l))?;
            }
            Ok(afetadas(linhas.len() as u64))
        }
    }
}

/// Executa um SELECT (ou EXPLAIN) numa leitura.
pub(crate) fn consultar_comando<P: Paginas + ?Sized>(
    p: &P,
    c: &Comando,
    params: &[Valor],
) -> Resultado<Saida> {
    match c {
        Comando::Selecionar(s) => Ok(selecionar(p, s, params)?.0),
        Comando::Explicar(s) => {
            let (_, plano) = selecionar(p, s, params)?;
            Ok(Saida {
                colunas: vec!["plano".into()],
                linhas: vec![vec![Valor::Texto(plano)]],
                afetadas: 0,
                ultimo_id: None,
            })
        }
        _ => semantica("só SELECT numa leitura; para gravar use uma transação"),
    }
}

pub(crate) fn comandos(sql: &str) -> Resultado<Vec<Comando>> {
    analisar(sql)
}
