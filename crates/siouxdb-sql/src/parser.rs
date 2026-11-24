//! Parser (descida recursiva) do subconjunto de SQL do SiouxDB.

use crate::lexico::{Ficha, Tok, fichas};
use crate::{ErroSql, Resultado};
use siouxdb_catalogo::{Tipo, Valor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    Igual,
    Diferente,
    Menor,
    MenorIgual,
    Maior,
    MaiorIgual,
    E,
    Ou,
    Mais,
    Menos,
    Vezes,
    Dividir,
    Resto,
    Concatenar,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Expr {
    Valor(Valor),
    Coluna(String),
    Param(usize),
    Nao(Box<Expr>),
    Negativo(Box<Expr>),
    Bin(Op, Box<Expr>, Box<Expr>),
    EhNulo(Box<Expr>, bool),
    Em(Box<Expr>, Vec<Expr>, bool),
    Entre(Box<Expr>, Box<Expr>, Box<Expr>, bool),
    Como(Box<Expr>, Box<Expr>, bool),
    /// Nome em minúsculas, argumentos, `COUNT(*)`.
    Funcao(String, Vec<Expr>, bool),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Item {
    Tudo,
    Expr(Expr, Option<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Juncao {
    pub tabela: String,
    pub apelido: Option<String>,
    pub esquerda: bool,
    pub on: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Selecao {
    pub itens: Vec<Item>,
    pub tabela: Option<String>,
    pub apelido: Option<String>,
    pub juncoes: Vec<Juncao>,
    pub como_estava: Option<Expr>,
    pub onde: Option<Expr>,
    pub agrupar: Vec<Expr>,
    pub tendo: Option<Expr>,
    pub ordem: Vec<(Expr, bool)>,
    pub limite: Option<Expr>,
    pub deslocamento: Option<Expr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Conflito {
    Erro,
    Ignorar,
    Substituir,
}

/// Definição de coluna num CREATE TABLE ou ALTER TABLE.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ColDef {
    pub nome: String,
    pub tipo: Tipo,
    pub nao_nulo: bool,
    pub padrao: Option<Expr>,
    pub auto: bool,
    pub chave: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Comando {
    CriarTabela {
        nome: String,
        se_nao_existe: bool,
        colunas: Vec<ColDef>,
        chave: Vec<String>,
    },
    AdicionarColuna {
        tabela: String,
        coluna: ColDef,
    },
    RenomearTabela {
        de: String,
        para: String,
    },
    RenomearColuna {
        tabela: String,
        de: String,
        para: String,
    },
    ApagarTabela {
        nome: String,
        se_existe: bool,
    },
    CriarIndice {
        nome: String,
        tabela: String,
        colunas: Vec<String>,
        unico: bool,
        se_nao_existe: bool,
    },
    ApagarIndice {
        nome: String,
        se_existe: bool,
    },
    Inserir {
        tabela: String,
        colunas: Option<Vec<String>>,
        linhas: Vec<Vec<Expr>>,
        conflito: Conflito,
    },
    Selecionar(Selecao),
    Explicar(Selecao),
    /// `VERIFY ['lsn:hash']`: confere o livro-razão (e a âncora, se vier).
    Verificar {
        ancora: Option<String>,
    },
    Atualizar {
        tabela: String,
        mudancas: Vec<(String, Expr)>,
        onde: Option<Expr>,
    },
    Remover {
        tabela: String,
        onde: Option<Expr>,
    },
}

impl Comando {
    pub fn so_leitura(&self) -> bool {
        matches!(
            self,
            Comando::Selecionar(_) | Comando::Explicar(_) | Comando::Verificar { .. }
        )
    }
}

struct Parser {
    f: Vec<Ficha>,
    i: usize,
    proximo_param: usize,
}

/// Lê um ou mais comandos separados por `;`.
pub(crate) fn analisar(sql: &str) -> Resultado<Vec<Comando>> {
    let mut p = Parser {
        f: fichas(sql)?,
        i: 0,
        proximo_param: 0,
    };
    let mut v = Vec::new();
    loop {
        while p.simbolo(";") {}
        if p.tok() == &Tok::Fim {
            break;
        }
        v.push(p.comando()?);
        if !p.simbolo(";") && p.tok() != &Tok::Fim {
            return p.erro("esperava ; ou o fim do comando");
        }
    }
    if v.is_empty() {
        return p.erro("nenhum comando");
    }
    Ok(v)
}

impl Parser {
    fn tok(&self) -> &Tok {
        &self.f[self.i].tok
    }

    fn erro<T>(&self, m: &str) -> Resultado<T> {
        let achou = match self.tok() {
            Tok::Nome { texto, .. } => format!(" (achou \"{texto}\")"),
            Tok::Simbolo(s) => format!(" (achou \"{s}\")"),
            Tok::Fim => " (achou o fim)".into(),
            _ => String::new(),
        };
        Err(ErroSql::Sintaxe {
            posicao: self.f[self.i].pos,
            mensagem: format!("{m}{achou}"),
        })
    }

    fn avancar(&mut self) {
        if self.i + 1 < self.f.len() {
            self.i += 1;
        }
    }

    /// Consome a palavra-chave se for a próxima.
    fn chave(&mut self, k: &str) -> bool {
        if matches!(self.tok(), Tok::Nome { texto, citado: false } if texto == k) {
            self.avancar();
            true
        } else {
            false
        }
    }

    fn espera_chave(&mut self, k: &str) -> Resultado<()> {
        if self.chave(k) {
            Ok(())
        } else {
            self.erro(&format!("esperava {}", k.to_uppercase()))
        }
    }

    fn eh_chave(&self, k: &str) -> bool {
        matches!(self.tok(), Tok::Nome { texto, citado: false } if texto == k)
    }

    fn simbolo(&mut self, s: &str) -> bool {
        if matches!(self.tok(), Tok::Simbolo(x) if *x == s) {
            self.avancar();
            true
        } else {
            false
        }
    }

    fn espera(&mut self, s: &str) -> Resultado<()> {
        if self.simbolo(s) {
            Ok(())
        } else {
            self.erro(&format!("esperava \"{s}\""))
        }
    }

    fn nome(&mut self) -> Resultado<String> {
        match self.tok().clone() {
            Tok::Nome { texto, citado } => {
                if !citado && RESERVADAS.contains(&texto.as_str()) {
                    return self.erro("esperava um nome");
                }
                self.avancar();
                Ok(texto)
            }
            _ => self.erro("esperava um nome"),
        }
    }

    fn lista_nomes(&mut self) -> Resultado<Vec<String>> {
        self.espera("(")?;
        let mut v = vec![self.nome()?];
        while self.simbolo(",") {
            v.push(self.nome()?);
        }
        self.espera(")")?;
        Ok(v)
    }

    fn comando(&mut self) -> Resultado<Comando> {
        if self.chave("create") {
            let unico = self.chave("unique");
            if self.chave("index") {
                let se_nao_existe = self.se_nao_existe()?;
                let nome = self.nome()?;
                self.espera_chave("on")?;
                let tabela = self.nome()?;
                let colunas = self.lista_nomes()?;
                return Ok(Comando::CriarIndice {
                    nome,
                    tabela,
                    colunas,
                    unico,
                    se_nao_existe,
                });
            }
            if unico {
                return self.erro("esperava INDEX");
            }
            self.espera_chave("table")?;
            return self.criar_tabela();
        }
        if self.chave("drop") {
            let tabela = if self.chave("table") {
                true
            } else if self.chave("index") {
                false
            } else {
                return self.erro("esperava TABLE ou INDEX");
            };
            let se_existe = if self.chave("if") {
                self.espera_chave("exists")?;
                true
            } else {
                false
            };
            let nome = self.nome()?;
            return Ok(if tabela {
                Comando::ApagarTabela { nome, se_existe }
            } else {
                Comando::ApagarIndice { nome, se_existe }
            });
        }
        if self.chave("alter") {
            self.espera_chave("table")?;
            return self.alterar_tabela();
        }
        if self.eh_chave("insert") || self.eh_chave("replace") {
            return self.inserir();
        }
        if self.chave("verify") || self.chave("verificar") {
            let ancora = match self.tok() {
                Tok::Texto(t) => Some(t.clone()),
                _ => None,
            };
            if ancora.is_some() {
                self.avancar();
            }
            return Ok(Comando::Verificar { ancora });
        }
        if self.chave("explain") {
            self.espera_chave("select")?;
            return Ok(Comando::Explicar(self.selecao()?));
        }
        if self.chave("select") {
            return Ok(Comando::Selecionar(self.selecao()?));
        }
        if self.chave("update") {
            let tabela = self.nome()?;
            self.espera_chave("set")?;
            let mut mudancas = Vec::new();
            loop {
                let c = self.nome()?;
                self.espera("=")?;
                mudancas.push((c, self.expr()?));
                if !self.simbolo(",") {
                    break;
                }
            }
            let onde = if self.chave("where") {
                Some(self.expr()?)
            } else {
                None
            };
            return Ok(Comando::Atualizar {
                tabela,
                mudancas,
                onde,
            });
        }
        if self.chave("delete") {
            self.espera_chave("from")?;
            let tabela = self.nome()?;
            let onde = if self.chave("where") {
                Some(self.expr()?)
            } else {
                None
            };
            return Ok(Comando::Remover { tabela, onde });
        }
        self.erro(
            "comando desconhecido (use CREATE, ALTER, DROP, INSERT, SELECT, UPDATE, DELETE, EXPLAIN ou VERIFY)",
        )
    }

    /// Apelido de tabela no FROM/JOIN: `AS x` (menos `AS OF`) ou um nome solto.
    fn apelido_de_tabela(&mut self) -> Resultado<Option<String>> {
        let as_of = matches!(&self.f[self.i].tok, Tok::Nome { texto, citado: false } if texto == "as")
            && matches!(self.f.get(self.i + 1).map(|f| &f.tok), Some(Tok::Nome { texto, citado: false }) if texto == "of");
        if as_of {
            return Ok(None);
        }
        let tem_as = self.chave("as");
        let livre = matches!(self.tok(), Tok::Nome { texto, citado } if *citado || !RESERVADAS.contains(&texto.as_str()));
        Ok(if tem_as || livre {
            Some(self.nome()?)
        } else {
            None
        })
    }

    fn se_nao_existe(&mut self) -> Resultado<bool> {
        if self.chave("if") {
            self.espera_chave("not")?;
            self.espera_chave("exists")?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Tipo da coluna; o segundo valor diz se é de numeração automática (SERIAL).
    fn tipo(&mut self) -> Resultado<(Tipo, bool)> {
        let Tok::Nome { texto, .. } = self.tok().clone() else {
            return self.erro("esperava o tipo da coluna");
        };
        let mut auto = false;
        let t = match texto.as_str() {
            "inteiro" | "int" | "integer" | "bigint" | "smallint" | "int8" | "int4" => {
                Tipo::Inteiro
            }
            "serial" | "bigserial" => {
                auto = true;
                Tipo::Inteiro
            }
            "real" | "float" | "double" | "float8" => Tipo::Real,
            "decimal" | "numeric" | "dinheiro" | "money" => Tipo::Decimal,
            "texto" | "text" | "varchar" | "char" | "string" => Tipo::Texto,
            "bytes" | "blob" | "bytea" => Tipo::Bytes,
            "booleano" | "bool" | "boolean" => Tipo::Booleano,
            "datahora" | "timestamp" | "timestamptz" | "datetime" | "date" | "data" => {
                Tipo::DataHora
            }
            _ => {
                return self.erro(
                    "tipo desconhecido (INTEIRO, DECIMAL, REAL, TEXTO, BYTES, BOOLEANO ou DATAHORA)",
                );
            }
        };
        self.avancar();
        self.chave("precision");
        if self.simbolo("(") {
            // VARCHAR(100), NUMERIC(10,2): o tamanho é aceito e ignorado.
            while !self.simbolo(")") {
                if self.tok() == &Tok::Fim {
                    return self.erro("esperava \")\"");
                }
                self.avancar();
            }
        }
        Ok((t, auto))
    }

    /// `nome TIPO [NOT NULL] [DEFAULT expr] [PRIMARY KEY] [AUTOINCREMENT]`
    fn coluna(&mut self) -> Resultado<ColDef> {
        let nome = self.nome()?;
        let (tipo, auto) = self.tipo()?;
        let mut c = ColDef {
            nome,
            tipo,
            nao_nulo: false,
            padrao: None,
            auto,
            chave: false,
        };
        loop {
            if self.chave("not") {
                self.espera_chave("null")?;
                c.nao_nulo = true;
            } else if self.chave("null") {
            } else if self.chave("primary") {
                self.espera_chave("key")?;
                c.chave = true;
            } else if self.chave("default") {
                c.padrao = Some(self.unario()?);
            } else if self.chave("autoincrement")
                || self.chave("autoincremento")
                || self.chave("auto_increment")
            {
                c.auto = true;
            } else {
                return Ok(c);
            }
        }
    }

    fn criar_tabela(&mut self) -> Resultado<Comando> {
        let se_nao_existe = self.se_nao_existe()?;
        let nome = self.nome()?;
        self.espera("(")?;
        let (mut colunas, mut chave): (Vec<ColDef>, Vec<String>) = (Vec::new(), Vec::new());
        loop {
            if self.chave("primary") {
                self.espera_chave("key")?;
                if !chave.is_empty() {
                    return self.erro("PRIMARY KEY repetida");
                }
                chave = self.lista_nomes()?;
            } else {
                let c = self.coluna()?;
                if c.chave {
                    if !chave.is_empty() {
                        return self.erro("PRIMARY KEY repetida");
                    }
                    chave = vec![c.nome.clone()];
                }
                colunas.push(c);
            }
            if !self.simbolo(",") {
                break;
            }
        }
        self.espera(")")?;
        if chave.is_empty() {
            return self.erro("a tabela precisa de PRIMARY KEY");
        }
        Ok(Comando::CriarTabela {
            nome,
            se_nao_existe,
            colunas,
            chave,
        })
    }

    fn alterar_tabela(&mut self) -> Resultado<Comando> {
        let tabela = self.nome()?;
        if self.chave("add") {
            self.chave("column");
            let coluna = self.coluna()?;
            if coluna.chave || coluna.auto {
                return self.erro("coluna acrescentada não pode ser PRIMARY KEY nem automática");
            }
            return Ok(Comando::AdicionarColuna { tabela, coluna });
        }
        if self.chave("rename") {
            if self.chave("to") {
                return Ok(Comando::RenomearTabela {
                    de: tabela,
                    para: self.nome()?,
                });
            }
            self.chave("column");
            let de = self.nome()?;
            self.espera_chave("to")?;
            return Ok(Comando::RenomearColuna {
                tabela,
                de,
                para: self.nome()?,
            });
        }
        self.erro("esperava ADD COLUMN, RENAME TO ou RENAME COLUMN")
    }

    fn inserir(&mut self) -> Resultado<Comando> {
        let mut conflito = Conflito::Erro;
        if self.chave("replace") {
            conflito = Conflito::Substituir;
        } else {
            self.espera_chave("insert")?;
            if self.chave("or") {
                conflito = if self.chave("replace") {
                    Conflito::Substituir
                } else if self.chave("ignore") {
                    Conflito::Ignorar
                } else {
                    return self.erro("esperava REPLACE ou IGNORE");
                };
            }
        }
        self.espera_chave("into")?;
        let tabela = self.nome()?;
        let colunas = if matches!(self.tok(), Tok::Simbolo("(")) {
            Some(self.lista_nomes()?)
        } else {
            None
        };
        self.espera_chave("values")?;
        let mut linhas = Vec::new();
        loop {
            self.espera("(")?;
            let mut l = vec![self.expr()?];
            while self.simbolo(",") {
                l.push(self.expr()?);
            }
            self.espera(")")?;
            linhas.push(l);
            if !self.simbolo(",") {
                break;
            }
        }
        if self.chave("on") {
            self.espera_chave("conflict")?;
            self.espera_chave("do")?;
            conflito = if self.chave("nothing") {
                Conflito::Ignorar
            } else if self.chave("update") {
                // ON CONFLICT DO UPDATE: substitui a linha inteira pelos valores novos.
                Conflito::Substituir
            } else {
                return self.erro("esperava NOTHING ou UPDATE");
            };
        }
        Ok(Comando::Inserir {
            tabela,
            colunas,
            linhas,
            conflito,
        })
    }

    fn selecao(&mut self) -> Resultado<Selecao> {
        let mut itens = Vec::new();
        loop {
            if self.simbolo("*") {
                itens.push(Item::Tudo);
            } else {
                let e = self.expr()?;
                // Apelido com AS, ou direto (`SELECT x total`) se o nome não for reservado.
                let tem_as = self.chave("as");
                let nome_livre = matches!(self.tok(), Tok::Nome { texto, citado } if *citado || !RESERVADAS.contains(&texto.as_str()));
                let apelido = if tem_as || nome_livre {
                    Some(self.nome()?)
                } else {
                    None
                };
                itens.push(Item::Expr(e, apelido));
            }
            if !self.simbolo(",") {
                break;
            }
        }
        let mut s = Selecao {
            itens,
            tabela: None,
            apelido: None,
            juncoes: Vec::new(),
            como_estava: None,
            onde: None,
            agrupar: Vec::new(),
            tendo: None,
            ordem: Vec::new(),
            limite: None,
            deslocamento: None,
        };
        if self.chave("from") {
            s.tabela = Some(self.nome()?);
            s.apelido = self.apelido_de_tabela()?;
            loop {
                let esquerda = if self.chave("left") {
                    self.chave("outer");
                    true
                } else {
                    self.chave("inner");
                    false
                };
                if !self.chave("join") {
                    if esquerda {
                        return self.erro("esperava JOIN depois de LEFT");
                    }
                    break;
                }
                let tabela = self.nome()?;
                let apelido = self.apelido_de_tabela()?;
                self.espera_chave("on")?;
                s.juncoes.push(Juncao {
                    tabela,
                    apelido,
                    esquerda,
                    on: self.expr()?,
                });
            }
            if self.chave("as") {
                self.espera_chave("of")?;
                s.como_estava = Some(self.expr()?);
            }
        }
        if self.chave("where") {
            s.onde = Some(self.expr()?);
        }
        if self.chave("group") {
            self.espera_chave("by")?;
            loop {
                s.agrupar.push(self.expr()?);
                if !self.simbolo(",") {
                    break;
                }
            }
        }
        if self.chave("having") {
            s.tendo = Some(self.expr()?);
        }
        if self.chave("order") {
            self.espera_chave("by")?;
            loop {
                let e = self.expr()?;
                let desc = if self.chave("desc") {
                    true
                } else {
                    self.chave("asc");
                    false
                };
                s.ordem.push((e, desc));
                if !self.simbolo(",") {
                    break;
                }
            }
        }
        if self.chave("limit") {
            s.limite = Some(self.expr()?);
        }
        if self.chave("offset") {
            s.deslocamento = Some(self.expr()?);
        }
        Ok(s)
    }

    // Precedência: OR < AND < NOT < comparação < || < + - < * / % < unário.
    fn expr(&mut self) -> Resultado<Expr> {
        let mut e = self.e()?;
        while self.chave("or") {
            e = Expr::Bin(Op::Ou, Box::new(e), Box::new(self.e()?));
        }
        Ok(e)
    }

    fn e(&mut self) -> Resultado<Expr> {
        let mut e = self.nao()?;
        while self.chave("and") {
            e = Expr::Bin(Op::E, Box::new(e), Box::new(self.nao()?));
        }
        Ok(e)
    }

    fn nao(&mut self) -> Resultado<Expr> {
        if self.chave("not") {
            return Ok(Expr::Nao(Box::new(self.nao()?)));
        }
        self.comparacao()
    }

    fn comparacao(&mut self) -> Resultado<Expr> {
        let a = self.concatenacao()?;
        let op = match self.tok() {
            Tok::Simbolo("=") => Some(Op::Igual),
            Tok::Simbolo("<>") | Tok::Simbolo("!=") => Some(Op::Diferente),
            Tok::Simbolo("<") => Some(Op::Menor),
            Tok::Simbolo("<=") => Some(Op::MenorIgual),
            Tok::Simbolo(">") => Some(Op::Maior),
            Tok::Simbolo(">=") => Some(Op::MaiorIgual),
            _ => None,
        };
        if let Some(op) = op {
            self.avancar();
            return Ok(Expr::Bin(op, Box::new(a), Box::new(self.concatenacao()?)));
        }
        if self.chave("is") {
            let negado = self.chave("not");
            self.espera_chave("null")?;
            return Ok(Expr::EhNulo(Box::new(a), negado));
        }
        let negado = self.chave("not");
        if self.chave("in") {
            self.espera("(")?;
            let mut l = vec![self.expr()?];
            while self.simbolo(",") {
                l.push(self.expr()?);
            }
            self.espera(")")?;
            return Ok(Expr::Em(Box::new(a), l, negado));
        }
        if self.chave("between") {
            let de = self.concatenacao()?;
            self.espera_chave("and")?;
            let ate = self.concatenacao()?;
            return Ok(Expr::Entre(
                Box::new(a),
                Box::new(de),
                Box::new(ate),
                negado,
            ));
        }
        if self.chave("like") {
            return Ok(Expr::Como(
                Box::new(a),
                Box::new(self.concatenacao()?),
                negado,
            ));
        }
        if negado {
            return self.erro("esperava IN, BETWEEN ou LIKE depois de NOT");
        }
        Ok(a)
    }

    fn concatenacao(&mut self) -> Resultado<Expr> {
        let mut e = self.soma()?;
        while self.simbolo("||") {
            e = Expr::Bin(Op::Concatenar, Box::new(e), Box::new(self.soma()?));
        }
        Ok(e)
    }

    fn soma(&mut self) -> Resultado<Expr> {
        let mut e = self.produto()?;
        loop {
            let op = if self.simbolo("+") {
                Op::Mais
            } else if self.simbolo("-") {
                Op::Menos
            } else {
                return Ok(e);
            };
            e = Expr::Bin(op, Box::new(e), Box::new(self.produto()?));
        }
    }

    fn produto(&mut self) -> Resultado<Expr> {
        let mut e = self.unario()?;
        loop {
            let op = if self.simbolo("*") {
                Op::Vezes
            } else if self.simbolo("/") {
                Op::Dividir
            } else if self.simbolo("%") {
                Op::Resto
            } else {
                return Ok(e);
            };
            e = Expr::Bin(op, Box::new(e), Box::new(self.unario()?));
        }
    }

    fn unario(&mut self) -> Resultado<Expr> {
        if self.simbolo("-") {
            return Ok(match self.unario()? {
                Expr::Valor(Valor::Inteiro(i)) => Expr::Valor(Valor::Inteiro(-i)),
                Expr::Valor(Valor::Real(r)) => Expr::Valor(Valor::Real(-r)),
                e => Expr::Negativo(Box::new(e)),
            });
        }
        if self.simbolo("+") {
            return self.unario();
        }
        self.primario()
    }

    fn primario(&mut self) -> Resultado<Expr> {
        match self.tok().clone() {
            Tok::Inteiro(i) => {
                self.avancar();
                Ok(Expr::Valor(Valor::Inteiro(i)))
            }
            Tok::Real(r) => {
                self.avancar();
                Ok(Expr::Valor(Valor::Real(r)))
            }
            Tok::Texto(t) => {
                self.avancar();
                Ok(Expr::Valor(Valor::Texto(t)))
            }
            Tok::Param(n) => {
                self.avancar();
                let n = match n {
                    Some(n) => n - 1,
                    None => {
                        self.proximo_param += 1;
                        self.proximo_param - 1
                    }
                };
                Ok(Expr::Param(n))
            }
            Tok::Simbolo("(") => {
                self.avancar();
                let e = self.expr()?;
                self.espera(")")?;
                Ok(e)
            }
            Tok::Nome { texto, citado } => {
                if !citado {
                    match texto.as_str() {
                        "null" => {
                            self.avancar();
                            return Ok(Expr::Valor(Valor::Nulo));
                        }
                        "true" | "verdadeiro" => {
                            self.avancar();
                            return Ok(Expr::Valor(Valor::Booleano(true)));
                        }
                        "false" | "falso" => {
                            self.avancar();
                            return Ok(Expr::Valor(Valor::Booleano(false)));
                        }
                        "current_timestamp" => {
                            self.avancar();
                            return Ok(Expr::Funcao("agora".into(), Vec::new(), false));
                        }
                        _ => {}
                    }
                }
                let nome = self.nome()?;
                if self.simbolo("(") {
                    if self.simbolo("*") {
                        self.espera(")")?;
                        return Ok(Expr::Funcao(nome, Vec::new(), true));
                    }
                    let mut args = Vec::new();
                    if !self.simbolo(")") {
                        args.push(self.expr()?);
                        while self.simbolo(",") {
                            args.push(self.expr()?);
                        }
                        self.espera(")")?;
                    }
                    return Ok(Expr::Funcao(nome, args, false));
                }
                // tabela.coluna: guarda o nome com o prefixo; quem executa resolve.
                if self.simbolo(".") {
                    return Ok(Expr::Coluna(format!("{nome}.{}", self.nome()?)));
                }
                Ok(Expr::Coluna(nome))
            }
            _ => self.erro("esperava um valor, coluna ou expressão"),
        }
    }
}

/// Palavras que não podem ser nome sem aspas.
const RESERVADAS: &[&str] = &[
    "select", "from", "where", "and", "or", "not", "insert", "into", "values", "update", "set",
    "delete", "create", "drop", "table", "index", "on", "primary", "key", "null", "is", "in",
    "between", "like", "order", "by", "group", "having", "limit", "offset", "as", "asc", "desc",
    "unique", "true", "false", "if", "exists", "conflict", "do", "explain", "join", "left",
    "inner", "outer",
];
