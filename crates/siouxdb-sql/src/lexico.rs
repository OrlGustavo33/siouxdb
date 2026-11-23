//! Analisador léxico.

use crate::{ErroSql, Resultado};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Tok {
    /// Identificador ou palavra-chave. `citado` = veio entre aspas duplas (não é palavra-chave).
    Nome {
        texto: String,
        citado: bool,
    },
    Texto(String),
    Inteiro(i64),
    Real(f64),
    /// `?` (None) ou `$n` (Some(n), começando em 1).
    Param(Option<usize>),
    Simbolo(&'static str),
    Fim,
}

pub(crate) struct Ficha {
    pub tok: Tok,
    pub pos: usize,
}

const SIMBOLOS: [&str; 18] = [
    "<>", "!=", "<=", ">=", "||", "(", ")", ",", ";", "*", "=", "<", ">", "+", "-", "/", "%", ".",
];

pub(crate) fn fichas(sql: &str) -> Resultado<Vec<Ficha>> {
    let c: Vec<char> = sql.chars().collect();
    let mut v = Vec::new();
    let mut i = 0;
    let erro = |pos: usize, m: &str| ErroSql::Sintaxe {
        posicao: pos,
        mensagem: m.to_string(),
    };
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        if ch == '-' && c.get(i + 1) == Some(&'-') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if ch == '/' && c.get(i + 1) == Some(&'*') {
            let ini = i;
            i += 2;
            while i + 1 < c.len() && !(c[i] == '*' && c[i + 1] == '/') {
                i += 1;
            }
            if i + 1 >= c.len() {
                return Err(erro(ini, "comentário /* sem fim"));
            }
            i += 2;
            continue;
        }
        let pos = i;
        if ch.is_alphabetic() || ch == '_' {
            let ini = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            let texto: String = c[ini..i].iter().collect::<String>().to_lowercase();
            v.push(Ficha {
                tok: Tok::Nome {
                    texto,
                    citado: false,
                },
                pos,
            });
            continue;
        }
        if ch == '"' {
            let (s, fim) =
                entre_aspas(&c, i, '"').ok_or_else(|| erro(pos, "nome entre aspas sem fim"))?;
            i = fim;
            v.push(Ficha {
                tok: Tok::Nome {
                    texto: s.to_lowercase(),
                    citado: true,
                },
                pos,
            });
            continue;
        }
        if ch == '\'' {
            let (s, fim) =
                entre_aspas(&c, i, '\'').ok_or_else(|| erro(pos, "texto sem o ' do fim"))?;
            i = fim;
            v.push(Ficha {
                tok: Tok::Texto(s),
                pos,
            });
            continue;
        }
        if ch.is_ascii_digit() || (ch == '.' && c.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let ini = i;
            let mut real = false;
            while i < c.len()
                && (c[i].is_ascii_digit()
                    || c[i] == '.'
                    || c[i] == 'e'
                    || c[i] == 'E'
                    || ((c[i] == '+' || c[i] == '-') && (c[i - 1] == 'e' || c[i - 1] == 'E')))
            {
                real |= !c[i].is_ascii_digit();
                i += 1;
            }
            let t: String = c[ini..i].iter().collect();
            let tok = if real {
                Tok::Real(t.parse().map_err(|_| erro(pos, "número inválido"))?)
            } else {
                match t.parse::<i64>() {
                    Ok(n) => Tok::Inteiro(n),
                    Err(_) => Tok::Real(t.parse().map_err(|_| erro(pos, "número inválido"))?),
                }
            };
            v.push(Ficha { tok, pos });
            continue;
        }
        if ch == '?' {
            i += 1;
            v.push(Ficha {
                tok: Tok::Param(None),
                pos,
            });
            continue;
        }
        if ch == '$' {
            i += 1;
            let ini = i;
            while i < c.len() && c[i].is_ascii_digit() {
                i += 1;
            }
            let n: usize = c[ini..i]
                .iter()
                .collect::<String>()
                .parse()
                .map_err(|_| erro(pos, "parâmetro $ sem número"))?;
            if n == 0 {
                return Err(erro(pos, "parâmetros começam em $1"));
            }
            v.push(Ficha {
                tok: Tok::Param(Some(n)),
                pos,
            });
            continue;
        }
        let resto: String = c[i..(i + 2).min(c.len())].iter().collect();
        match SIMBOLOS.iter().find(|s| resto.starts_with(**s)) {
            Some(s) => {
                i += s.chars().count();
                v.push(Ficha {
                    tok: Tok::Simbolo(s),
                    pos,
                });
            }
            None => return Err(erro(pos, &format!("caractere inesperado '{ch}'"))),
        }
    }
    v.push(Ficha {
        tok: Tok::Fim,
        pos: c.len(),
    });
    Ok(v)
}

/// Texto entre aspas (aspa dobrada = aspa literal). Devolve o texto e a posição depois dele.
fn entre_aspas(c: &[char], ini: usize, aspa: char) -> Option<(String, usize)> {
    let mut s = String::new();
    let mut i = ini + 1;
    loop {
        let ch = *c.get(i)?;
        if ch == aspa {
            if c.get(i + 1) == Some(&aspa) {
                s.push(aspa);
                i += 2;
                continue;
            }
            return Some((s, i + 1));
        }
        s.push(ch);
        i += 1;
    }
}
