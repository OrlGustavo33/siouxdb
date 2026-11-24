//! Avaliação de expressões (lógica de três valores do SQL: comparação com NULL dá NULL).

use crate::Resultado;
use crate::erro::semantica;
use crate::parser::{Expr, Op};
use siouxdb_catalogo::{Decimal, Valor, datahora};
use std::cmp::Ordering;

pub(crate) struct Ctx<'a> {
    pub nomes: &'a [String],
    pub linha: &'a [Valor],
    pub params: &'a [Valor],
}

pub(crate) const AGREGADOS: [&str; 5] = ["count", "sum", "min", "max", "avg"];

pub(crate) fn tem_agregado(e: &Expr) -> bool {
    match e {
        Expr::Funcao(n, args, _) => {
            AGREGADOS.contains(&n.as_str()) || args.iter().any(tem_agregado)
        }
        Expr::Nao(a) | Expr::Negativo(a) | Expr::EhNulo(a, _) => tem_agregado(a),
        Expr::Bin(_, a, b) | Expr::Como(a, b, _) => tem_agregado(a) || tem_agregado(b),
        Expr::Em(a, l, _) => tem_agregado(a) || l.iter().any(tem_agregado),
        Expr::Entre(a, b, c, _) => tem_agregado(a) || tem_agregado(b) || tem_agregado(c),
        _ => false,
    }
}

pub(crate) fn tem_coluna(e: &Expr) -> bool {
    match e {
        Expr::Coluna(_) => true,
        Expr::Funcao(_, args, estrela) => *estrela || args.iter().any(tem_coluna),
        Expr::Nao(a) | Expr::Negativo(a) | Expr::EhNulo(a, _) => tem_coluna(a),
        Expr::Bin(_, a, b) | Expr::Como(a, b, _) => tem_coluna(a) || tem_coluna(b),
        Expr::Em(a, l, _) => tem_coluna(a) || l.iter().any(tem_coluna),
        Expr::Entre(a, b, c, _) => tem_coluna(a) || tem_coluna(b) || tem_coluna(c),
        _ => false,
    }
}

/// Confere, antes de executar, que toda coluna citada existe (`extras` = apelidos aceitos).
/// Posição da coluna `c` nos `nomes` do contexto. Os nomes podem ser simples (`id`, uma
/// tabela só) ou qualificados (`p.id`, com JOIN); `c` pode ser qualquer um dos dois.
pub(crate) fn resolver_coluna(nomes: &[String], c: &str) -> Resultado<usize> {
    if let Some(i) = nomes.iter().position(|n| n == c) {
        return Ok(i);
    }
    match c.rsplit_once('.') {
        // `t.x` com nomes simples: o prefixo é aceito.
        Some((_, resto)) => {
            if let Some(i) = nomes.iter().position(|n| n == resto) {
                return Ok(i);
            }
        }
        // `x` com nomes qualificados: precisa existir em uma tabela só.
        None => {
            let achou: Vec<usize> = nomes
                .iter()
                .enumerate()
                .filter(|(_, n)| n.rsplit_once('.').is_some_and(|(_, r)| r == c))
                .map(|(i, _)| i)
                .collect();
            match achou.len() {
                1 => return Ok(achou[0]),
                0 => {}
                _ => return semantica(format!("a coluna \"{c}\" é ambígua: use tabela.coluna")),
            }
        }
    }
    semantica(format!("coluna \"{c}\" não existe"))
}

pub(crate) fn conferir_colunas(e: &Expr, nomes: &[String], extras: &[String]) -> Resultado<()> {
    let r = |x: &Expr| conferir_colunas(x, nomes, extras);
    match e {
        Expr::Coluna(c) if !extras.contains(c) => resolver_coluna(nomes, c).map(|_| ()),
        Expr::Funcao(_, args, _) => args.iter().try_for_each(r),
        Expr::Nao(a) | Expr::Negativo(a) | Expr::EhNulo(a, _) => r(a),
        Expr::Bin(_, a, b) | Expr::Como(a, b, _) => r(a).and_then(|_| r(b)),
        Expr::Em(a, l, _) => r(a).and_then(|_| l.iter().try_for_each(r)),
        Expr::Entre(a, b, c, _) => r(a).and_then(|_| r(b)).and_then(|_| r(c)),
        _ => Ok(()),
    }
}

/// Compara dois valores. `None` se algum é NULL. Inteiro e real se comparam como números.
pub(crate) fn comparar(a: &Valor, b: &Valor) -> Resultado<Option<Ordering>> {
    Ok(Some(match (a, b) {
        (Valor::Nulo, _) | (_, Valor::Nulo) => return Ok(None),
        (Valor::Decimal(x), Valor::Inteiro(y)) => x.cmp(&Decimal::de_inteiro(*y)),
        (Valor::Inteiro(x), Valor::Decimal(y)) => Decimal::de_inteiro(*x).cmp(y),
        // Real comparado com decimal é convertido para decimal (10 casas): assim o filtro e
        // o índice usam exatamente a mesma conta.
        (Valor::Decimal(x), Valor::Real(y)) => match Decimal::de_real(*y) {
            Some(d) => x.cmp(&d),
            None => x.para_real().total_cmp(y),
        },
        (Valor::Real(x), Valor::Decimal(y)) => match Decimal::de_real(*x) {
            Some(d) => d.cmp(y),
            None => x.total_cmp(&y.para_real()),
        },
        (Valor::DataHora(x), Valor::Texto(t)) => x.cmp(&data_de(t)?),
        (Valor::Texto(t), Valor::DataHora(y)) => data_de(t)?.cmp(y),
        (Valor::Inteiro(x), Valor::Real(y)) => (*x as f64).total_cmp(y),
        (Valor::Real(x), Valor::Inteiro(y)) => x.total_cmp(&(*y as f64)),
        (x, y) if x.tipo() == y.tipo() => x.comparar(y),
        (x, y) => {
            return semantica(format!(
                "não dá para comparar {} com {}",
                x.tipo().map_or("NULL", |t| t.nome()),
                y.tipo().map_or("NULL", |t| t.nome())
            ));
        }
    }))
}

fn data_de(t: &str) -> Resultado<i64> {
    match datahora::de_texto(t) {
        Some(d) => Ok(d),
        None => semantica(format!(
            "\"{t}\" não é uma data (use AAAA-MM-DD ou AAAA-MM-DD HH:MM:SS)"
        )),
    }
}

/// Valor como decimal, se for número.
pub(crate) fn decimal_de(v: &Valor) -> Option<Decimal> {
    match v {
        Valor::Decimal(d) => Some(*d),
        Valor::Inteiro(i) => Some(Decimal::de_inteiro(*i)),
        Valor::Real(r) => Decimal::de_real(*r),
        _ => None,
    }
}

/// Ordem para ORDER BY: NULL primeiro, tipos diferentes pela ordem dos tipos.
pub(crate) fn ordem_total(a: &Valor, b: &Valor) -> Ordering {
    match comparar(a, b) {
        Ok(Some(o)) => o,
        _ => a.comparar(b),
    }
}

fn booleano(v: &Valor, o_que: &str) -> Resultado<Option<bool>> {
    match v {
        Valor::Nulo => Ok(None),
        Valor::Booleano(b) => Ok(Some(*b)),
        _ => semantica(format!("{o_que} precisa ser verdadeiro/falso")),
    }
}

/// A linha passa no filtro? (Só `verdadeiro` passa; NULL não.)
pub(crate) fn passa(e: &Expr, ctx: &Ctx<'_>) -> Resultado<bool> {
    Ok(booleano(&avaliar(e, ctx)?, "a condição")? == Some(true))
}

fn numerico(op: Op, a: &Valor, b: &Valor) -> Resultado<Valor> {
    use Valor::{Inteiro, Nulo, Real};
    Ok(match (a, b) {
        (Nulo, _) | (_, Nulo) => Nulo,
        (Inteiro(x), Inteiro(y)) => {
            let r = match op {
                Op::Mais => x.checked_add(*y),
                Op::Menos => x.checked_sub(*y),
                Op::Vezes => x.checked_mul(*y),
                Op::Dividir | Op::Resto if *y == 0 => return semantica("divisão por zero"),
                Op::Dividir => x.checked_div(*y),
                Op::Resto => x.checked_rem(*y),
                _ => None,
            };
            match r {
                Some(v) => Inteiro(v),
                None => return semantica("número inteiro fora do limite"),
            }
        }
        (Valor::Decimal(_), Inteiro(_) | Real(_) | Valor::Decimal(_))
        | (Inteiro(_) | Real(_), Valor::Decimal(_)) => {
            let (Some(x), Some(y)) = (decimal_de(a), decimal_de(b)) else {
                return semantica("número fora do limite do DECIMAL");
            };
            let r = match op {
                Op::Mais => x.somar(y),
                Op::Menos => x.subtrair(y),
                Op::Vezes => x.multiplicar(y),
                Op::Dividir | Op::Resto if y == Decimal::ZERO => {
                    return semantica("divisão por zero");
                }
                Op::Dividir => x.dividir(y),
                Op::Resto => x.bruto().checked_rem(y.bruto()).map(Decimal::de_bruto),
                _ => None,
            };
            match r {
                Some(d) => Valor::Decimal(d),
                None => return semantica("número fora do limite do DECIMAL"),
            }
        }
        (Inteiro(_) | Real(_), Inteiro(_) | Real(_)) => {
            let f = |v: &Valor| match v {
                Inteiro(i) => *i as f64,
                Real(r) => *r,
                _ => 0.0,
            };
            let (x, y) = (f(a), f(b));
            Real(match op {
                Op::Mais => x + y,
                Op::Menos => x - y,
                Op::Vezes => x * y,
                Op::Dividir => x / y,
                Op::Resto => x % y,
                _ => f64::NAN,
            })
        }
        _ => return semantica("conta com valor que não é número"),
    })
}

fn como(texto: &str, padrao: &str) -> bool {
    // % = qualquer sequência, _ = um caractere. Iterativo, sem recursão exponencial.
    let (t, p): (Vec<char>, Vec<char>) = (texto.chars().collect(), padrao.chars().collect());
    let (mut i, mut j) = (0, 0);
    let (mut estrela, mut marca) = (None, 0);
    while i < t.len() {
        if j < p.len() && (p[j] == '_' || p[j] == t[i]) {
            i += 1;
            j += 1;
        } else if j < p.len() && p[j] == '%' {
            estrela = Some(j);
            marca = i;
            j += 1;
        } else if let Some(e) = estrela {
            j = e + 1;
            marca += 1;
            i = marca;
        } else {
            return false;
        }
    }
    while j < p.len() && p[j] == '%' {
        j += 1;
    }
    j == p.len()
}

pub(crate) fn avaliar(e: &Expr, ctx: &Ctx<'_>) -> Resultado<Valor> {
    Ok(match e {
        Expr::Valor(v) => v.clone(),
        Expr::Param(n) => match ctx.params.get(*n) {
            Some(v) => v.clone(),
            None => {
                return semantica(format!(
                    "faltou o parâmetro {} (vieram {})",
                    n + 1,
                    ctx.params.len()
                ));
            }
        },
        Expr::Coluna(c) => ctx.linha[resolver_coluna(ctx.nomes, c)?].clone(),
        Expr::Nao(a) => match booleano(&avaliar(a, ctx)?, "NOT")? {
            None => Valor::Nulo,
            Some(b) => Valor::Booleano(!b),
        },
        Expr::Negativo(a) => match avaliar(a, ctx)? {
            Valor::Inteiro(i) => Valor::Inteiro(
                i.checked_neg()
                    .ok_or(())
                    .or_else(|_| semantica("número fora do limite"))?,
            ),
            Valor::Real(r) => Valor::Real(-r),
            Valor::Decimal(d) => match d.negativo() {
                Some(d) => Valor::Decimal(d),
                None => return semantica("número fora do limite"),
            },
            Valor::Nulo => Valor::Nulo,
            _ => return semantica("sinal de menos em valor que não é número"),
        },
        Expr::Bin(op @ (Op::E | Op::Ou), a, b) => {
            let x = booleano(&avaliar(a, ctx)?, "AND/OR")?;
            // Curto-circuito quando o resultado já está decidido.
            match (op, x) {
                (Op::E, Some(false)) => return Ok(Valor::Booleano(false)),
                (Op::Ou, Some(true)) => return Ok(Valor::Booleano(true)),
                _ => {}
            }
            let y = booleano(&avaliar(b, ctx)?, "AND/OR")?;
            match (op, x, y) {
                (Op::E, _, Some(false)) => Valor::Booleano(false),
                (Op::Ou, _, Some(true)) => Valor::Booleano(true),
                (_, Some(x), Some(y)) => {
                    Valor::Booleano(if *op == Op::E { x && y } else { x || y })
                }
                _ => Valor::Nulo,
            }
        }
        Expr::Bin(Op::Concatenar, a, b) => match (avaliar(a, ctx)?, avaliar(b, ctx)?) {
            (Valor::Nulo, _) | (_, Valor::Nulo) => Valor::Nulo,
            (x, y) => Valor::Texto(format!("{}{}", texto_de(&x), texto_de(&y))),
        },
        Expr::Bin(op @ (Op::Mais | Op::Menos | Op::Vezes | Op::Dividir | Op::Resto), a, b) => {
            numerico(*op, &avaliar(a, ctx)?, &avaliar(b, ctx)?)?
        }
        Expr::Bin(op, a, b) => {
            let o = comparar(&avaliar(a, ctx)?, &avaliar(b, ctx)?)?;
            match o {
                None => Valor::Nulo,
                Some(o) => Valor::Booleano(match op {
                    Op::Igual => o == Ordering::Equal,
                    Op::Diferente => o != Ordering::Equal,
                    Op::Menor => o == Ordering::Less,
                    Op::MenorIgual => o != Ordering::Greater,
                    Op::Maior => o == Ordering::Greater,
                    _ => o != Ordering::Less,
                }),
            }
        }
        Expr::EhNulo(a, negado) => {
            Valor::Booleano(matches!(avaliar(a, ctx)?, Valor::Nulo) != *negado)
        }
        Expr::Em(a, lista, negado) => {
            let x = avaliar(a, ctx)?;
            if matches!(x, Valor::Nulo) {
                return Ok(Valor::Nulo);
            }
            let mut viu_nulo = false;
            for item in lista {
                match comparar(&x, &avaliar(item, ctx)?)? {
                    Some(Ordering::Equal) => return Ok(Valor::Booleano(!negado)),
                    None => viu_nulo = true,
                    _ => {}
                }
            }
            if viu_nulo {
                Valor::Nulo
            } else {
                Valor::Booleano(*negado)
            }
        }
        Expr::Entre(a, de, ate, negado) => {
            let x = avaliar(a, ctx)?;
            let (d, t) = (
                comparar(&x, &avaliar(de, ctx)?)?,
                comparar(&x, &avaliar(ate, ctx)?)?,
            );
            match (d, t) {
                (Some(d), Some(t)) => {
                    Valor::Booleano((d != Ordering::Less && t != Ordering::Greater) != *negado)
                }
                _ => Valor::Nulo,
            }
        }
        Expr::Como(a, p, negado) => match (avaliar(a, ctx)?, avaliar(p, ctx)?) {
            (Valor::Texto(t), Valor::Texto(p)) => Valor::Booleano(como(&t, &p) != *negado),
            (Valor::Nulo, _) | (_, Valor::Nulo) => Valor::Nulo,
            _ => return semantica("LIKE só compara textos"),
        },
        Expr::Funcao(nome, args, _) => {
            if AGREGADOS.contains(&nome.as_str()) {
                return semantica(format!(
                    "{}() só pode aparecer no SELECT, HAVING ou ORDER BY",
                    nome.to_uppercase()
                ));
            }
            let v: Vec<Valor> = args
                .iter()
                .map(|a| avaliar(a, ctx))
                .collect::<Resultado<_>>()?;
            funcao(nome, v)?
        }
    })
}

fn texto_de(v: &Valor) -> String {
    match v {
        Valor::Texto(t) => t.clone(),
        Valor::Inteiro(i) => i.to_string(),
        Valor::Real(r) => r.to_string(),
        Valor::Booleano(b) => b.to_string(),
        Valor::Bytes(b) => b.iter().map(|x| format!("{x:02x}")).collect(),
        Valor::Decimal(d) => d.to_string(),
        Valor::DataHora(t) => datahora::para_texto(*t),
        Valor::Nulo => String::new(),
    }
}

fn funcao(nome: &str, v: Vec<Valor>) -> Resultado<Valor> {
    let um = |v: &Vec<Valor>| {
        if v.len() == 1 {
            Ok(v[0].clone())
        } else {
            semantica(format!("{}() recebe 1 argumento", nome.to_uppercase()))
        }
    };
    Ok(match nome {
        "lower" | "minusculas" => match um(&v)? {
            Valor::Texto(t) => Valor::Texto(t.to_lowercase()),
            x => x,
        },
        "upper" | "maiusculas" => match um(&v)? {
            Valor::Texto(t) => Valor::Texto(t.to_uppercase()),
            x => x,
        },
        "length" | "tamanho" => match um(&v)? {
            Valor::Texto(t) => Valor::Inteiro(t.chars().count() as i64),
            Valor::Bytes(b) => Valor::Inteiro(b.len() as i64),
            Valor::Nulo => Valor::Nulo,
            _ => return semantica("LENGTH() recebe texto ou bytes"),
        },
        "abs" => match um(&v)? {
            Valor::Inteiro(i) => Valor::Inteiro(
                i.checked_abs()
                    .ok_or(())
                    .or_else(|_| semantica("número fora do limite"))?,
            ),
            Valor::Real(r) => Valor::Real(r.abs()),
            Valor::Decimal(d) => match d.absoluto() {
                Some(d) => Valor::Decimal(d),
                None => return semantica("número fora do limite"),
            },
            Valor::Nulo => Valor::Nulo,
            _ => return semantica("ABS() recebe número"),
        },
        "agora" | "now" if v.is_empty() => Valor::DataHora(datahora::agora()),
        "coalesce" => v
            .into_iter()
            .find(|x| !matches!(x, Valor::Nulo))
            .unwrap_or(Valor::Nulo),
        _ => return semantica(format!("função desconhecida: {nome}()")),
    })
}

/// Junta em `molde` (sem repetir) os agregados que aparecem em `e`, conferindo o uso.
pub(crate) fn coletar_agregados(e: &Expr, molde: &mut Vec<Expr>) -> Resultado<()> {
    let mut r = |x: &Expr| coletar_agregados(x, molde);
    match e {
        Expr::Funcao(n, args, estrela) if AGREGADOS.contains(&n.as_str()) => {
            if *estrela && n != "count" {
                return semantica(format!("{}(*) não existe; use COUNT(*)", n.to_uppercase()));
            }
            if !*estrela && args.len() != 1 {
                return semantica(format!("{}() recebe 1 argumento", n.to_uppercase()));
            }
            if !*estrela && tem_agregado(&args[0]) {
                return semantica("agregado dentro de agregado não é permitido");
            }
            if !molde.contains(e) {
                molde.push(e.clone());
            }
        }
        Expr::Funcao(_, args, _) => args.iter().try_for_each(r)?,
        Expr::Nao(a) | Expr::Negativo(a) | Expr::EhNulo(a, _) => r(a)?,
        Expr::Bin(_, a, b) | Expr::Como(a, b, _) => {
            r(a)?;
            r(b)?;
        }
        Expr::Em(a, l, _) => {
            r(a)?;
            l.iter().try_for_each(r)?;
        }
        Expr::Entre(a, b, c, _) => {
            r(a)?;
            r(b)?;
            r(c)?;
        }
        _ => {}
    }
    Ok(())
}

/// Estado de um agregado num grupo: soma, conta, mínimo ou máximo, linha a linha (o grupo
/// nunca guarda as linhas).
#[derive(Debug, Clone)]
pub(crate) struct Acumulador {
    nome: String,
    arg: Option<Expr>,
    n: i64,
    valor: Option<Valor>,
}

impl Acumulador {
    /// Para um agregado do molde.
    pub(crate) fn novo(e: &Expr) -> Acumulador {
        let (nome, arg) = match e {
            Expr::Funcao(n, args, estrela) => (n.clone(), (!*estrela).then(|| args[0].clone())),
            _ => (String::new(), None),
        };
        Acumulador {
            nome,
            arg,
            n: 0,
            valor: None,
        }
    }

    /// Conta mais uma linha.
    pub(crate) fn somar(&mut self, ctx: &Ctx<'_>) -> Resultado<()> {
        let Some(arg) = &self.arg else {
            self.n += 1;
            return Ok(());
        };
        let v = avaliar(arg, ctx)?;
        if matches!(v, Valor::Nulo) {
            return Ok(());
        }
        self.n += 1;
        match self.nome.as_str() {
            "count" => {}
            "min" | "max" => {
                let troca = match &self.valor {
                    None => true,
                    Some(m) => {
                        let o = comparar(&v, m)?.unwrap_or(Ordering::Equal);
                        (self.nome == "min" && o == Ordering::Less)
                            || (self.nome == "max" && o == Ordering::Greater)
                    }
                };
                if troca {
                    self.valor = Some(v);
                }
            }
            _ => {
                let soma = self.valor.take().unwrap_or(Valor::Inteiro(0));
                self.valor = Some(numerico(Op::Mais, &soma, &v)?);
            }
        }
        Ok(())
    }

    /// Valor final.
    pub(crate) fn resultado(&self) -> Resultado<Valor> {
        if self.nome == "count" {
            return Ok(Valor::Inteiro(self.n));
        }
        let Some(v) = &self.valor else {
            return Ok(Valor::Nulo);
        };
        if self.nome == "avg" {
            numerico(Op::Dividir, v, &Valor::Real(self.n as f64))
        } else {
            Ok(v.clone())
        }
    }
}

/// Troca cada agregado de `e` pelo valor calculado (na mesma posição do `molde`).
pub(crate) fn trocar_agregados(e: &Expr, molde: &[Expr], valores: &[Valor]) -> Expr {
    let r = |x: &Expr| trocar_agregados(x, molde, valores);
    match e {
        Expr::Funcao(n, _, _) if AGREGADOS.contains(&n.as_str()) => molde
            .iter()
            .position(|m| m == e)
            .map_or(Expr::Valor(Valor::Nulo), |i| {
                Expr::Valor(valores[i].clone())
            }),
        Expr::Funcao(n, args, estrela) => {
            Expr::Funcao(n.clone(), args.iter().map(r).collect(), *estrela)
        }
        Expr::Nao(a) => Expr::Nao(Box::new(r(a))),
        Expr::Negativo(a) => Expr::Negativo(Box::new(r(a))),
        Expr::EhNulo(a, n) => Expr::EhNulo(Box::new(r(a)), *n),
        Expr::Bin(op, a, b) => Expr::Bin(*op, Box::new(r(a)), Box::new(r(b))),
        Expr::Como(a, b, n) => Expr::Como(Box::new(r(a)), Box::new(r(b)), *n),
        Expr::Em(a, l, n) => Expr::Em(Box::new(r(a)), l.iter().map(r).collect(), *n),
        Expr::Entre(a, b, c, n) => Expr::Entre(Box::new(r(a)), Box::new(r(b)), Box::new(r(c)), *n),
        outro => outro.clone(),
    }
}

#[cfg(test)]
mod testes {
    use super::como;

    #[test]
    fn like() {
        assert!(como("são paulo", "s%"));
        assert!(como("são paulo", "%paulo"));
        assert!(como("abc", "a_c"));
        assert!(!como("abc", "a_d"));
        assert!(como("", "%"));
        assert!(como("aaa", "%a%a%"));
        assert!(!como("ab", "abc%"));
        assert!(como(&"a".repeat(50), &"%a".repeat(20)));
    }
}
