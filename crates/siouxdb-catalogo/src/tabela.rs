//! Tabela versionada: log quente + índice da chave primária.
//!
//! O índice (árvore B+) leva da chave primária à **versão mais nova** da linha no log. Uma
//! remoção também é uma versão; a chave continua no índice, apontando para a marca de
//! remoção, para o histórico continuar alcançável.

use crate::log::{self, Pontas, Ponteiro};
use crate::valor::{Tipo, Valor, codificar_chave, codificar_linha, decodificar_linha};
use crate::{ErroCatalogo, Resultado};
use siouxdb_armazenamento::{NumeroPagina, TipoPagina};
use siouxdb_indices::arvore::{self, Cursor, ResumoArvore};
use siouxdb_transacoes::{Paginas, PaginasMut};
use std::time::{SystemTime, UNIX_EPOCH};

/// Uma coluna.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coluna {
    /// Nome (minúsculas, sem acento).
    pub nome: String,
    /// Tipo.
    pub tipo: Tipo,
    /// Recusa nulo? (Colunas da chave primária sempre recusam.)
    pub nao_nulo: bool,
}

impl Coluna {
    /// Coluna que aceita nulo.
    pub fn nova(nome: &str, tipo: Tipo) -> Coluna {
        Coluna {
            nome: nome.to_string(),
            tipo,
            nao_nulo: false,
        }
    }

    /// Coluna que recusa nulo.
    pub fn obrigatoria(nome: &str, tipo: Tipo) -> Coluna {
        Coluna {
            nome: nome.to_string(),
            tipo,
            nao_nulo: true,
        }
    }
}

/// Definição de uma tabela, como está no catálogo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefTabela {
    /// Identificador (nunca reaproveitado).
    pub id: u64,
    /// Nome.
    pub nome: String,
    /// Colunas, na ordem.
    pub colunas: Vec<Coluna>,
    /// Posições das colunas da chave primária, na ordem da chave.
    pub chave: Vec<usize>,
    /// Página de cabeçalho da tabela.
    pub cabecalho: NumeroPagina,
}

/// Uma versão de uma linha, no histórico.
#[derive(Debug, Clone, PartialEq)]
pub struct VersaoLinha {
    /// LSN da transação que gravou.
    pub lsn: u64,
    /// Instante da gravação (µs desde 1970).
    pub instante: u64,
    /// A linha, ou `None` se esta versão é uma remoção.
    pub linha: Option<Vec<Valor>>,
}

/// O que uma compactação fez.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResumoCompactacao {
    /// Versões no log antes.
    pub versoes_antes: u64,
    /// Versões no log depois.
    pub versoes_depois: u64,
    /// Chaves removidas que saíram de vez do índice.
    pub chaves_apagadas: u64,
}

/// Contagens de uma tabela.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Contagem {
    /// Linhas que existem agora.
    pub linhas: u64,
    /// Versões no log (toda gravação e remoção conta).
    pub versoes: u64,
}

/// Dados guardados na página de cabeçalho da tabela.
#[derive(Debug, Clone, Copy, Default)]
struct Cab {
    raiz: NumeroPagina,
    pontas: Pontas,
    versoes: u64,
    linhas: u64,
}

const RUIM: ErroCatalogo = ErroCatalogo::Corrompido("cabeçalho de tabela com forma inesperada");

impl Cab {
    fn ler<P: Paginas + ?Sized>(p: &P, numero: NumeroPagina) -> Resultado<Cab> {
        let pagina = p.pagina(numero)?;
        if pagina.tipo() != TipoPagina::CabecalhoTabela {
            return Err(RUIM);
        }
        let b = pagina.conteudo();
        let v = |i: usize| u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().expect("8"));
        Ok(Cab {
            raiz: v(0),
            pontas: Pontas {
                primeira: v(1),
                cauda: v(2),
            },
            versoes: v(3),
            linhas: v(4),
        })
    }

    fn gravar<P: PaginasMut + ?Sized>(&self, tx: &mut P, numero: NumeroPagina) -> Resultado<()> {
        let b = tx.pagina_mut(numero)?.conteudo_mut();
        for (i, v) in [
            self.raiz,
            self.pontas.primeira,
            self.pontas.cauda,
            self.versoes,
            self.linhas,
        ]
        .into_iter()
        .enumerate()
        {
            b[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
        }
        Ok(())
    }
}

fn agora() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

enum Modo {
    Inserir,
    Gravar,
    Remover,
}

/// Uma tabela aberta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tabela {
    def: DefTabela,
}

impl Tabela {
    pub(crate) fn nova(def: DefTabela) -> Tabela {
        Tabela { def }
    }

    /// Cria as páginas de uma tabela nova (cabeçalho e raiz do índice) e devolve o cabeçalho.
    pub(crate) fn criar_paginas<P: PaginasMut + ?Sized>(tx: &mut P) -> Resultado<NumeroPagina> {
        let cabecalho = tx.alocar(TipoPagina::CabecalhoTabela)?;
        let raiz = arvore::criar(tx)?;
        Cab {
            raiz,
            ..Cab::default()
        }
        .gravar(tx, cabecalho)?;
        Ok(cabecalho)
    }

    /// Libera todas as páginas da tabela.
    pub(crate) fn liberar_paginas<P: PaginasMut + ?Sized>(&self, tx: &mut P) -> Resultado<()> {
        let cab = Cab::ler(tx, self.def.cabecalho)?;
        arvore::liberar(tx, cab.raiz)?;
        log::liberar(tx, cab.pontas.primeira)?;
        tx.liberar(self.def.cabecalho)?;
        Ok(())
    }

    /// Definição da tabela.
    pub fn def(&self) -> &DefTabela {
        &self.def
    }

    fn invalida(&self, motivo: String) -> ErroCatalogo {
        ErroCatalogo::LinhaInvalida(format!("tabela \"{}\": {motivo}", self.def.nome))
    }

    fn validar(&self, linha: &[Valor]) -> Resultado<()> {
        if linha.len() != self.def.colunas.len() {
            return Err(self.invalida(format!(
                "esperava {} valores, vieram {}",
                self.def.colunas.len(),
                linha.len()
            )));
        }
        for (i, (v, c)) in linha.iter().zip(&self.def.colunas).enumerate() {
            match v.tipo() {
                None if c.nao_nulo || self.def.chave.contains(&i) => {
                    return Err(self.invalida(format!("a coluna \"{}\" não aceita nulo", c.nome)));
                }
                Some(t) if t != c.tipo => {
                    return Err(self.invalida(format!(
                        "a coluna \"{}\" é {} e recebeu {}",
                        c.nome,
                        c.tipo.nome(),
                        t.nome()
                    )));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Codifica a chave primária de uma linha completa.
    fn chave_da_linha(&self, linha: &[Valor]) -> Resultado<Vec<u8>> {
        let partes: Vec<Valor> = self.def.chave.iter().map(|&i| linha[i].clone()).collect();
        codificar_chave(&partes)
    }

    /// Codifica uma chave primária dada pelos valores das colunas da chave, na ordem.
    fn chave_dos_valores(&self, chave: &[Valor]) -> Resultado<Vec<u8>> {
        if chave.len() != self.def.chave.len() {
            return Err(self.invalida(format!(
                "a chave tem {} coluna(s), vieram {}",
                self.def.chave.len(),
                chave.len()
            )));
        }
        for (v, &i) in chave.iter().zip(&self.def.chave) {
            if v.tipo() != Some(self.def.colunas[i].tipo) {
                return Err(self.invalida(format!(
                    "valor de tipo errado para a coluna de chave \"{}\"",
                    self.def.colunas[i].nome
                )));
            }
        }
        codificar_chave(chave)
    }

    fn escrever<P: PaginasMut + ?Sized>(
        &self,
        tx: &mut P,
        chave: &[u8],
        dados: &[u8],
        modo: Modo,
    ) -> Resultado<bool> {
        let mut cab = Cab::ler(tx, self.def.cabecalho)?;
        let anterior = match arvore::obter_com(tx, cab.raiz, chave, Ponteiro::de_bytes)? {
            Some(p) => p?,
            None => Ponteiro::NENHUM,
        };
        let existia = !anterior.nenhum() && !log::ler_com(tx, anterior, |v| v.removida)?;
        match modo {
            Modo::Inserir if existia => return Err(ErroCatalogo::ChaveDuplicada),
            Modo::Remover if !existia => return Ok(false),
            _ => {}
        }
        let removendo = matches!(modo, Modo::Remover);
        let ponteiro = log::anexar(
            tx,
            &mut cab.pontas,
            removendo,
            anterior,
            tx.lsn(),
            agora(),
            chave,
            dados,
        )?;
        arvore::inserir(tx, cab.raiz, chave, &ponteiro.bytes())?;
        cab.versoes += 1;
        match (existia, removendo) {
            (false, false) => cab.linhas += 1,
            (true, true) => cab.linhas -= 1,
            _ => {}
        }
        cab.gravar(tx, self.def.cabecalho)?;
        Ok(existia)
    }

    /// Insere uma linha nova. Erro [`ErroCatalogo::ChaveDuplicada`] se a chave já existe.
    pub fn inserir<P: PaginasMut + ?Sized>(&self, tx: &mut P, linha: &[Valor]) -> Resultado<()> {
        self.validar(linha)?;
        let chave = self.chave_da_linha(linha)?;
        self.escrever(tx, &chave, &codificar_linha(linha), Modo::Inserir)
            .map(|_| ())
    }

    /// Grava a linha, criando ou substituindo. Devolve `true` se já existia.
    pub fn gravar<P: PaginasMut + ?Sized>(&self, tx: &mut P, linha: &[Valor]) -> Resultado<bool> {
        self.validar(linha)?;
        let chave = self.chave_da_linha(linha)?;
        self.escrever(tx, &chave, &codificar_linha(linha), Modo::Gravar)
    }

    /// Remove a linha com essa chave primária. Devolve `true` se existia.
    pub fn remover<P: PaginasMut + ?Sized>(&self, tx: &mut P, chave: &[Valor]) -> Resultado<bool> {
        let chave = self.chave_dos_valores(chave)?;
        self.escrever(tx, &chave, &[], Modo::Remover)
    }

    /// Linha na versão que valia no LSN `ate` (`u64::MAX` = a mais nova).
    fn resolver<P: Paginas + ?Sized>(
        &self,
        p: &P,
        mut ponteiro: Ponteiro,
        ate: u64,
    ) -> Resultado<Option<Vec<Valor>>> {
        let mut passos = 0u64;
        while !ponteiro.nenhum() {
            let (lsn, removida, anterior, linha) = log::ler_com(p, ponteiro, |v| {
                let linha = (v.lsn <= ate && !v.removida)
                    .then(|| log::dados_completos(p, &v).and_then(|d| decodificar_linha(&d)));
                (v.lsn, v.removida, v.anterior, linha)
            })?;
            if lsn <= ate {
                return if removida {
                    Ok(None)
                } else {
                    linha.transpose()
                };
            }
            ponteiro = anterior;
            passos += 1;
            if passos > 1 << 40 {
                return Err(ErroCatalogo::Corrompido("corrente de versões sem fim"));
            }
        }
        Ok(None)
    }

    fn ponteiro_de<P: Paginas + ?Sized>(&self, p: &P, chave: &[Valor]) -> Resultado<Ponteiro> {
        let chave = self.chave_dos_valores(chave)?;
        let cab = Cab::ler(p, self.def.cabecalho)?;
        Ok(
            match arvore::obter_com(p, cab.raiz, &chave, Ponteiro::de_bytes)? {
                Some(pt) => pt?,
                None => Ponteiro::NENHUM,
            },
        )
    }

    /// Busca pela chave primária.
    pub fn obter<P: Paginas + ?Sized>(
        &self,
        p: &P,
        chave: &[Valor],
    ) -> Resultado<Option<Vec<Valor>>> {
        self.obter_em(p, chave, u64::MAX)
    }

    /// Busca pela chave primária **como estava** depois da transação de LSN `lsn`.
    pub fn obter_em<P: Paginas + ?Sized>(
        &self,
        p: &P,
        chave: &[Valor],
        lsn: u64,
    ) -> Resultado<Option<Vec<Valor>>> {
        let ponteiro = self.ponteiro_de(p, chave)?;
        self.resolver(p, ponteiro, lsn)
    }

    /// Todas as versões da linha, da mais nova para a mais antiga.
    pub fn historico<P: Paginas + ?Sized>(
        &self,
        p: &P,
        chave: &[Valor],
    ) -> Resultado<Vec<VersaoLinha>> {
        let mut ponteiro = self.ponteiro_de(p, chave)?;
        let mut versoes = Vec::new();
        while !ponteiro.nenhum() {
            let (v, anterior) = log::ler_com(p, ponteiro, |v| {
                let linha = (!v.removida)
                    .then(|| log::dados_completos(p, &v).and_then(|d| decodificar_linha(&d)))
                    .transpose();
                (
                    linha.map(|linha| VersaoLinha {
                        lsn: v.lsn,
                        instante: v.instante,
                        linha,
                    }),
                    v.anterior,
                )
            })?;
            versoes.push(v?);
            ponteiro = anterior;
        }
        Ok(versoes)
    }

    /// Percorre as linhas em ordem de chave primária, com `inicio <= chave < fim`. Os limites
    /// podem ser um começo da chave composta (ex.: só a primeira coluna).
    pub fn varrer<'p, P: Paginas + ?Sized>(
        &'p self,
        p: &'p P,
        inicio: Option<&[Valor]>,
        fim: Option<&[Valor]>,
    ) -> Resultado<Varredura<'p, P>> {
        self.varrer_em(p, inicio, fim, u64::MAX)
    }

    /// Como [`Tabela::varrer`], vendo a tabela **como estava** no LSN `lsn`.
    pub fn varrer_em<'p, P: Paginas + ?Sized>(
        &'p self,
        p: &'p P,
        inicio: Option<&[Valor]>,
        fim: Option<&[Valor]>,
        lsn: u64,
    ) -> Resultado<Varredura<'p, P>> {
        let cab = Cab::ler(p, self.def.cabecalho)?;
        let inicio = inicio.map(codificar_chave).transpose()?;
        let fim = fim.map(codificar_chave).transpose()?;
        let cursor = Cursor::novo(p, cab.raiz, inicio.as_deref(), fim.as_deref())?;
        Ok(Varredura {
            tabela: self,
            paginas: p,
            cursor,
            lsn,
        })
    }

    /// **Compacta** o histórico: reescreve o log guardando só o que é preciso para ler a
    /// tabela como estava em qualquer LSN `>= manter_desde`. Para cada chave ficam as
    /// versões posteriores a `manter_desde` e a que valia nele (a "base"); chaves
    /// removidas antes de `manter_desde` saem de vez. As páginas do log antigo (e os
    /// transbordos) voltam para o banco.
    ///
    /// Com `manter_desde = u64::MAX` fica só a versão atual de cada linha. Leituras "como
    /// estava" antes de `manter_desde` deixam de ser exatas. Roda numa transação só: a
    /// tabela inteira passa pela memória.
    pub fn compactar<P: PaginasMut + ?Sized>(
        &self,
        tx: &mut P,
        manter_desde: u64,
    ) -> Resultado<ResumoCompactacao> {
        let mut cab = Cab::ler(tx, self.def.cabecalho)?;
        let mut entradas = Vec::new();
        {
            let mut c = Cursor::novo(tx, cab.raiz, None, None)?;
            while let Some(e) =
                c.proximo_com(|k, v| Ponteiro::de_bytes(v).map(|p| (k.to_vec(), p)))?
            {
                entradas.push(e?);
            }
        }
        let mut novo = Pontas::default();
        let mut r = ResumoCompactacao {
            versoes_antes: cab.versoes,
            ..ResumoCompactacao::default()
        };
        let mut linhas = 0;
        for (chave, mut ponteiro) in entradas {
            // Da mais nova para a mais antiga, até a primeira com LSN <= `manter_desde` (a base).
            let mut guardar: Vec<(bool, u64, u64, Vec<u8>)> = Vec::new();
            while !ponteiro.nenhum() {
                let (versao, anterior) = log::ler_com(tx, ponteiro, |v| {
                    let dados = if v.removida {
                        Ok(Vec::new())
                    } else {
                        log::dados_completos(tx, &v).map(|d| d.into_owned())
                    };
                    (
                        dados.map(|d| (v.removida, v.lsn, v.instante, d)),
                        v.anterior,
                    )
                })?;
                let versao = versao?;
                let base = versao.1 <= manter_desde;
                guardar.push(versao);
                if base {
                    break;
                }
                ponteiro = anterior;
            }
            // Base que é remoção não precisa ficar (antes dela não há nada a ler).
            if guardar
                .last()
                .is_some_and(|(removida, lsn, ..)| *removida && *lsn <= manter_desde)
            {
                guardar.pop();
            }
            if guardar.is_empty() {
                arvore::remover(tx, cab.raiz, &chave)?;
                r.chaves_apagadas += 1;
                continue;
            }
            if !guardar[0].0 {
                linhas += 1;
            }
            let mut anterior = Ponteiro::NENHUM;
            for (removida, lsn, instante, dados) in guardar.iter().rev() {
                anterior = log::anexar(
                    tx, &mut novo, *removida, anterior, *lsn, *instante, &chave, dados,
                )?;
                r.versoes_depois += 1;
            }
            arvore::inserir(tx, cab.raiz, &chave, &anterior.bytes())?;
        }
        log::liberar(tx, cab.pontas.primeira)?;
        cab.pontas = novo;
        cab.versoes = r.versoes_depois;
        cab.linhas = linhas;
        cab.gravar(tx, self.def.cabecalho)?;
        Ok(r)
    }

    /// Quantas linhas e versões a tabela tem.
    pub fn contagem<P: Paginas + ?Sized>(&self, p: &P) -> Resultado<Contagem> {
        let cab = Cab::ler(p, self.def.cabecalho)?;
        Ok(Contagem {
            linhas: cab.linhas,
            versoes: cab.versoes,
        })
    }

    /// Confere a estrutura do índice da chave primária.
    pub fn verificar_indice<P: Paginas + ?Sized>(&self, p: &P) -> Resultado<ResumoArvore> {
        let cab = Cab::ler(p, self.def.cabecalho)?;
        Ok(arvore::verificar(p, cab.raiz)?)
    }
}

/// Linhas de uma tabela em ordem de chave. Ver [`Tabela::varrer`].
pub struct Varredura<'p, P: Paginas + ?Sized> {
    tabela: &'p Tabela,
    paginas: &'p P,
    cursor: Cursor<'p, P>,
    lsn: u64,
}

impl<P: Paginas + ?Sized> Varredura<'_, P> {
    /// Próxima linha (pula chaves removidas ou que não existiam no LSN pedido).
    pub fn proxima(&mut self) -> Resultado<Option<Vec<Valor>>> {
        while let Some(ponteiro) = self.cursor.proximo_com(|_, v| Ponteiro::de_bytes(v))? {
            if let Some(linha) = self.tabela.resolver(self.paginas, ponteiro?, self.lsn)? {
                return Ok(Some(linha));
            }
        }
        Ok(None)
    }

    /// Junta todas as linhas restantes.
    pub fn todas(mut self) -> Resultado<Vec<Vec<Valor>>> {
        let mut v = Vec::new();
        while let Some(l) = self.proxima()? {
            v.push(l);
        }
        Ok(v)
    }
}
