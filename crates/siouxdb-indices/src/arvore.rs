//! Árvore B+ sobre páginas.
//!
//! - Chaves e valores são bytes; a ordem é a dos bytes (quem chama codifica a chave de modo
//!   que essa ordem seja a desejada).
//! - A **raiz nunca muda de página**: quando ela divide, o conteúdo desce para páginas novas.
//!   Quem guarda o número da raiz não precisa atualizá-lo.
//! - Inserção em ordem crescente (o caso de chaves sequenciais) deixa as folhas cheias: a
//!   divisão manda só a entrada nova para a folha nova.
//! - A chave que sobe para o nó pai é a **menor separadora possível** entre as duas folhas,
//!   não a chave inteira.
//! - Remoção não redistribui: uma folha que fica vazia é liberada e sai do pai.
//!
//! Tudo acontece dentro de uma transação, então uma queda no meio de uma divisão não deixa
//! a árvore pela metade.

use crate::no::{self, No, R};
use crate::{ErroIndice, Resultado};
use siouxdb_armazenamento::{NumeroPagina, Pagina, TipoPagina};
use siouxdb_transacoes::{Paginas, PaginasMut};
use std::sync::Arc;

/// Altura máxima aceita (uma árvore real tem 3 ou 4 níveis; mais que isso é laço).
const ALTURA_MAXIMA: usize = 48;

fn ruim<T>(pagina: NumeroPagina, r: R<T>) -> Resultado<T> {
    r.map_err(|motivo| ErroIndice::Corrompido { pagina, motivo })
}

fn abrir(pagina: &Pagina, numero: NumeroPagina) -> Resultado<No<'_>> {
    if pagina.tipo() != TipoPagina::NoArvore {
        return Err(ErroIndice::Corrompido {
            pagina: numero,
            motivo: "a página não é um nó de árvore",
        });
    }
    ruim(numero, No::novo(pagina.conteudo()))
}

fn conteudo_mut<P: PaginasMut + ?Sized>(tx: &mut P, numero: NumeroPagina) -> Resultado<&mut [u8]> {
    let p = tx.pagina_mut(numero)?;
    if p.tipo() != TipoPagina::NoArvore {
        return Err(ErroIndice::Corrompido {
            pagina: numero,
            motivo: "a página não é um nó de árvore",
        });
    }
    Ok(p.conteudo_mut())
}

/// Maior `chave.len() + valor.len()` aceito para páginas com `tamanho_conteudo` bytes.
pub fn maximo_entrada(tamanho_conteudo: usize) -> usize {
    no::maximo_celula(tamanho_conteudo).saturating_sub(10)
}

/// Cria uma árvore vazia e devolve a página raiz.
pub fn criar<P: PaginasMut + ?Sized>(tx: &mut P) -> Resultado<NumeroPagina> {
    Ok(tx.alocar(TipoPagina::NoArvore)?)
}

/// Nós internos visitados na descida: (página, índice do filho seguido).
type Caminho = Vec<(NumeroPagina, usize)>;

/// Desce até a folha de `chave`. Devolve a folha e o caminho (nó interno, índice do filho).
fn descer<P: Paginas + ?Sized>(
    p: &P,
    raiz: NumeroPagina,
    chave: &[u8],
) -> Resultado<(NumeroPagina, Arc<Pagina>, Caminho)> {
    let mut caminho = Vec::new();
    let mut atual = raiz;
    loop {
        let pagina = p.pagina(atual)?;
        let no = abrir(&pagina, atual)?;
        if no.folha() {
            return Ok((atual, pagina, caminho));
        }
        if caminho.len() >= ALTURA_MAXIMA {
            return Err(ErroIndice::Corrompido {
                pagina: atual,
                motivo: "árvore funda demais (laço?)",
            });
        }
        let i = ruim(atual, no.filho_para(chave))?;
        caminho.push((atual, i));
        atual = ruim(atual, no.filho(i))?;
    }
}

/// Busca `chave` e aplica `f` ao valor, sem copiá-lo.
pub fn obter_com<P: Paginas + ?Sized, T>(
    p: &P,
    raiz: NumeroPagina,
    chave: &[u8],
    f: impl FnOnce(&[u8]) -> T,
) -> Resultado<Option<T>> {
    let (folha, pagina, _) = descer(p, raiz, chave)?;
    let no = abrir(&pagina, folha)?;
    Ok(match ruim(folha, no.buscar(chave))? {
        Ok(i) => Some(f(ruim(folha, no.valor(i))?)),
        Err(_) => None,
    })
}

/// Busca `chave`.
pub fn obter<P: Paginas + ?Sized>(
    p: &P,
    raiz: NumeroPagina,
    chave: &[u8],
) -> Resultado<Option<Vec<u8>>> {
    obter_com(p, raiz, chave, <[u8]>::to_vec)
}

/// Menor prefixo de `direita` que ainda é maior que `esquerda` (com `esquerda < direita`).
fn separador_curto<'a>(esquerda: &[u8], direita: &'a [u8]) -> &'a [u8] {
    let comum = esquerda
        .iter()
        .zip(direita)
        .take_while(|(a, b)| a == b)
        .count();
    &direita[..(comum + 1).min(direita.len())]
}

/// Onde cortar as células de um nó que estourou. `1 <= m <= len - 1`.
fn ponto_de_corte(cels: &[Vec<u8>], anexando: bool) -> usize {
    if anexando {
        return cels.len() - 1;
    }
    let total: usize = cels.iter().map(|c| c.len() + 2).sum();
    let mut soma = 0;
    for (i, c) in cels.iter().enumerate() {
        soma += c.len() + 2;
        if soma * 2 >= total {
            return (i + 1).clamp(1, cels.len() - 1);
        }
    }
    cels.len() - 1
}

/// Grava `chave → valor`. Devolve o valor anterior, se a chave já existia.
pub fn inserir<P: PaginasMut + ?Sized>(
    tx: &mut P,
    raiz: NumeroPagina,
    chave: &[u8],
    valor: &[u8],
) -> Resultado<Option<Vec<u8>>> {
    let maximo = maximo_entrada(tx.tamanho_conteudo());
    if chave.len() + valor.len() > maximo {
        return Err(ErroIndice::EntradaGrandeDemais {
            tamanho: chave.len() + valor.len(),
            maximo,
        });
    }
    let (folha, pagina, caminho) = descer(tx, raiz, chave)?;
    let busca = ruim(folha, abrir(&pagina, folha)?.buscar(chave))?;
    drop(pagina);

    let b = conteudo_mut(tx, folha)?;
    let mut anterior = None;
    let pos = match busca {
        Ok(i) => {
            let atual = ruim(folha, ruim(folha, No::novo(b))?.valor(i))?.to_vec();
            if atual.len() == valor.len() {
                ruim(folha, no::trocar_valor(b, i, valor))?;
                return Ok(Some(atual));
            }
            ruim(folha, no::remover(b, i))?;
            anterior = Some(atual);
            i
        }
        Err(i) => i,
    };
    let cel = no::celula_folha(chave, valor);
    if ruim(folha, no::inserir(b, pos, &cel))? {
        return Ok(anterior);
    }

    // Não coube: divide a folha.
    let mut cels = ruim(folha, ruim(folha, No::novo(b))?.celulas())?;
    cels.insert(pos, cel);
    let m = ponto_de_corte(&cels, pos == cels.len() - 1);
    let separador = separador_curto(
        no::chave_da_celula(&cels[m - 1], true),
        no::chave_da_celula(&cels[m], true),
    )
    .to_vec();
    ruim(folha, no::reescrever(b, 0, 0, &cels[..m]))?;
    let direita = criar(tx)?;
    ruim(
        direita,
        no::reescrever(conteudo_mut(tx, direita)?, 0, 0, &cels[m..]),
    )?;
    subir(tx, raiz, caminho, folha, separador, direita)?;
    Ok(anterior)
}

/// Depois de `esquerda` dividir (ficando com a metade de baixo) e nascer `direita`, registra
/// a separadora nos nós de cima, dividindo-os também se preciso.
fn subir<P: PaginasMut + ?Sized>(
    tx: &mut P,
    raiz: NumeroPagina,
    mut caminho: Caminho,
    mut esquerda: NumeroPagina,
    mut separador: Vec<u8>,
    mut direita: NumeroPagina,
) -> Resultado<()> {
    loop {
        let Some((pai, i)) = caminho.pop() else {
            // Quem dividiu foi a raiz: o conteúdo dela desce para uma página nova e a raiz
            // vira um nó interno com dois filhos. O número da raiz não muda.
            debug_assert_eq!(esquerda, raiz);
            let copia = tx.pagina(raiz)?.conteudo().to_vec();
            let nivel = copia[0];
            let nova = criar(tx)?;
            conteudo_mut(tx, nova)?.copy_from_slice(&copia);
            let cel = no::celula_interna(&separador, nova);
            return ruim(
                raiz,
                no::reescrever(conteudo_mut(tx, raiz)?, nivel + 1, direita, &[cel]),
            );
        };
        let b = conteudo_mut(tx, pai)?;
        // O ponteiro que levava a `esquerda` passa a levar a `direita`, e entra antes dele
        // a célula (separador → esquerda).
        ruim(pai, no::definir_filho(b, i, direita))?;
        let cel = no::celula_interna(&separador, esquerda);
        if ruim(pai, no::inserir(b, i, &cel))? {
            return Ok(());
        }
        // O pai também estourou: a célula do meio sobe.
        let no_pai = ruim(pai, No::novo(b))?;
        let (nivel, filho_direito) = (no_pai.nivel(), no_pai.filho_direito());
        let mut cels = ruim(pai, no_pai.celulas())?;
        cels.insert(i, cel);
        if cels.len() < 3 {
            return Err(ErroIndice::Corrompido {
                pagina: pai,
                motivo: "nó interno pequeno demais para dividir",
            });
        }
        let m = ponto_de_corte(&cels, false).min(cels.len() - 2);
        let sobe = no::chave_da_celula(&cels[m], false).to_vec();
        let filho_do_meio = no::filho_da_celula(&cels[m]);
        ruim(pai, no::reescrever(b, nivel, filho_do_meio, &cels[..m]))?;
        let nova = criar(tx)?;
        ruim(
            nova,
            no::reescrever(
                conteudo_mut(tx, nova)?,
                nivel,
                filho_direito,
                &cels[m + 1..],
            ),
        )?;
        esquerda = pai;
        separador = sobe;
        direita = nova;
    }
}

/// Tira `chave`. Devolve o valor que estava lá, se existia.
pub fn remover<P: PaginasMut + ?Sized>(
    tx: &mut P,
    raiz: NumeroPagina,
    chave: &[u8],
) -> Resultado<Option<Vec<u8>>> {
    let (folha, pagina, mut caminho) = descer(tx, raiz, chave)?;
    let no_folha = abrir(&pagina, folha)?;
    let Ok(pos) = ruim(folha, no_folha.buscar(chave))? else {
        return Ok(None);
    };
    let anterior = ruim(folha, no_folha.valor(pos))?.to_vec();
    let ficou_vazia = no_folha.n() == 1;
    drop(pagina);
    ruim(folha, no::remover(conteudo_mut(tx, folha)?, pos))?;
    if !ficou_vazia || folha == raiz {
        return Ok(Some(anterior));
    }

    // A folha esvaziou: libera e tira do pai; se o pai ficar sem filhos, sobe.
    let mut vazio = folha;
    loop {
        tx.liberar(vazio)?;
        let Some((pai, i)) = caminho.pop() else {
            return Err(ErroIndice::Corrompido {
                pagina: vazio,
                motivo: "nó vazio sem pai",
            });
        };
        let b = conteudo_mut(tx, pai)?;
        let n = ruim(pai, No::novo(b))?.n();
        if n == 0 {
            // O pai só tinha este filho.
            if pai == raiz {
                ruim(pai, no::reescrever::<Vec<u8>>(b, 0, 0, &[]))?;
                return Ok(Some(anterior));
            }
            vazio = pai;
            continue;
        }
        if i < n {
            ruim(pai, no::remover(b, i))?;
        } else {
            // Era o filho da direita: o último filho com chave assume o lugar.
            let novo = ruim(pai, ruim(pai, No::novo(b))?.filho(n - 1))?;
            ruim(pai, no::remover(b, n - 1))?;
            ruim(pai, no::definir_filho(b, n - 1, novo))?;
        }
        break;
    }
    // Raiz interna com um filho só: o filho sobe para a raiz (a árvore perde um nível).
    loop {
        let pagina = tx.pagina(raiz)?;
        let no_raiz = abrir(&pagina, raiz)?;
        if no_raiz.folha() || no_raiz.n() > 0 {
            return Ok(Some(anterior));
        }
        let filho = no_raiz.filho_direito();
        let pagina_filho = tx.pagina(filho)?;
        abrir(&pagina_filho, filho)?;
        let copia = pagina_filho.conteudo().to_vec();
        drop((pagina, pagina_filho));
        conteudo_mut(tx, raiz)?.copy_from_slice(&copia);
        tx.liberar(filho)?;
    }
}

/// Libera todas as páginas da árvore (inclusive a raiz). Devolve quantas eram.
pub fn liberar<P: PaginasMut + ?Sized>(tx: &mut P, raiz: NumeroPagina) -> Resultado<u64> {
    fn rec<P: PaginasMut + ?Sized>(
        tx: &mut P,
        numero: NumeroPagina,
        nivel: usize,
    ) -> Resultado<u64> {
        if nivel > ALTURA_MAXIMA {
            return Err(ErroIndice::Corrompido {
                pagina: numero,
                motivo: "árvore funda demais (laço?)",
            });
        }
        let pagina = tx.pagina(numero)?;
        let no = abrir(&pagina, numero)?;
        let filhos: Vec<u64> = if no.folha() {
            Vec::new()
        } else {
            ruim(numero, (0..=no.n()).map(|i| no.filho(i)).collect())?
        };
        drop(pagina);
        let mut total = 1;
        for f in filhos {
            total += rec(tx, f, nivel + 1)?;
        }
        tx.liberar(numero)?;
        Ok(total)
    }
    rec(tx, raiz, 0)
}

/// Percorre as entradas em ordem de chave, numa faixa.
pub struct Cursor<'p, P: Paginas + ?Sized> {
    paginas: &'p P,
    /// Do nó raiz até a folha atual: (página, índice do filho ou da entrada).
    pilha: Vec<(NumeroPagina, Arc<Pagina>, usize)>,
    fim: Option<Vec<u8>>,
    acabou: bool,
}

impl<'p, P: Paginas + ?Sized> Cursor<'p, P> {
    /// Cursor nas chaves `inicio <= chave < fim` (`None` = sem limite daquele lado).
    pub fn novo(
        paginas: &'p P,
        raiz: NumeroPagina,
        inicio: Option<&[u8]>,
        fim: Option<&[u8]>,
    ) -> Resultado<Self> {
        let mut c = Cursor {
            paginas,
            pilha: Vec::new(),
            fim: fim.map(<[u8]>::to_vec),
            acabou: false,
        };
        c.descer(raiz, inicio)?;
        Ok(c)
    }

    /// Desce a partir de `numero` até uma folha: pela `chave`, ou sempre pelo primeiro filho.
    fn descer(&mut self, mut numero: NumeroPagina, chave: Option<&[u8]>) -> Resultado<()> {
        loop {
            if self.pilha.len() > ALTURA_MAXIMA {
                return Err(ErroIndice::Corrompido {
                    pagina: numero,
                    motivo: "árvore funda demais (laço?)",
                });
            }
            let pagina = self.paginas.pagina(numero)?;
            let no = abrir(&pagina, numero)?;
            let i = match chave {
                None => 0,
                Some(k) if no.folha() => match ruim(numero, no.buscar(k))? {
                    Ok(i) | Err(i) => i,
                },
                Some(k) => ruim(numero, no.filho_para(k))?,
            };
            let proximo = if no.folha() {
                None
            } else {
                Some(ruim(numero, no.filho(i))?)
            };
            self.pilha.push((numero, pagina, i));
            match proximo {
                None => return Ok(()),
                Some(f) => numero = f,
            }
        }
    }

    /// Deixa o topo da pilha numa entrada válida de folha. `false` = acabou a árvore.
    fn posicionar(&mut self) -> Resultado<bool> {
        loop {
            let Some((numero, pagina, i)) = self.pilha.last() else {
                return Ok(false);
            };
            if *i < abrir(pagina, *numero)?.n() {
                return Ok(true);
            }
            // Folha esgotada: sobe até um nó que ainda tenha filho à direita e desce por ele.
            self.pilha.pop();
            loop {
                let Some((numero, pagina, i)) = self.pilha.last_mut() else {
                    return Ok(false);
                };
                let no = abrir(pagina, *numero)?;
                if *i < no.n() {
                    *i += 1;
                    let filho = ruim(*numero, no.filho(*i))?;
                    self.descer(filho, None)?;
                    break;
                }
                self.pilha.pop();
            }
        }
    }

    /// Próxima entrada, aplicando `f` à chave e ao valor sem copiá-los.
    pub fn proximo_com<T>(&mut self, f: impl FnOnce(&[u8], &[u8]) -> T) -> Resultado<Option<T>> {
        if self.acabou || !self.posicionar()? {
            self.acabou = true;
            return Ok(None);
        }
        let (numero, pagina, i) = self.pilha.last_mut().expect("posicionar garantiu a folha");
        let no = abrir(pagina, *numero)?;
        let chave = ruim(*numero, no.chave(*i))?;
        if self.fim.as_deref().is_some_and(|fim| chave >= fim) {
            self.acabou = true;
            return Ok(None);
        }
        let t = f(chave, ruim(*numero, no.valor(*i))?);
        *i += 1;
        Ok(Some(t))
    }

    /// Próxima entrada (chave, valor).
    pub fn proximo(&mut self) -> Resultado<Option<(Vec<u8>, Vec<u8>)>> {
        self.proximo_com(|k, v| (k.to_vec(), v.to_vec()))
    }
}

/// Números de uma árvore conferida por [`verificar`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResumoArvore {
    /// Níveis (1 = só a raiz).
    pub altura: usize,
    /// Páginas no total.
    pub paginas: u64,
    /// Páginas folha.
    pub folhas: u64,
    /// Entradas (chaves).
    pub entradas: u64,
    /// Bytes das células nas folhas (para calcular a ocupação).
    pub bytes_nas_folhas: u64,
}

/// Confere a estrutura inteira: chaves em ordem dentro de cada nó, cada filho dentro dos
/// limites do pai, todas as folhas na mesma profundidade e nenhum nó (fora a raiz) vazio.
pub fn verificar<P: Paginas + ?Sized>(p: &P, raiz: NumeroPagina) -> Resultado<ResumoArvore> {
    fn rec<P: Paginas + ?Sized>(
        p: &P,
        numero: NumeroPagina,
        minimo: Option<&[u8]>,
        maximo: Option<&[u8]>,
        profundidade: usize,
        r: &mut ResumoArvore,
    ) -> Resultado<usize> {
        let erro = |motivo| ErroIndice::Corrompido {
            pagina: numero,
            motivo,
        };
        if profundidade > ALTURA_MAXIMA {
            return Err(erro("árvore funda demais (laço?)"));
        }
        let eh_raiz = profundidade == 0;
        let pagina = p.pagina(numero)?;
        let no = abrir(&pagina, numero)?;
        r.paginas += 1;
        let mut anterior: Option<&[u8]> = None;
        for i in 0..no.n() {
            let k = ruim(numero, no.chave(i))?;
            if anterior.is_some_and(|a| a >= k) {
                return Err(erro("chaves fora de ordem"));
            }
            if minimo.is_some_and(|m| k < m) || maximo.is_some_and(|m| k >= m) {
                return Err(erro("chave fora dos limites do nó pai"));
            }
            anterior = Some(k);
        }
        if no.folha() {
            if no.n() == 0 && !eh_raiz {
                return Err(erro("folha vazia fora da raiz"));
            }
            r.folhas += 1;
            r.entradas += no.n() as u64;
            for i in 0..no.n() {
                r.bytes_nas_folhas += ruim(numero, no.celula(i))?.len() as u64 + 2;
            }
            return Ok(1);
        }
        if no.nivel() as usize > ALTURA_MAXIMA {
            return Err(erro("nível absurdo"));
        }
        let mut altura = None;
        for i in 0..=no.n() {
            let de = if i == 0 {
                minimo
            } else {
                Some(ruim(numero, no.chave(i - 1))?)
            };
            let ate = if i == no.n() {
                maximo
            } else {
                Some(ruim(numero, no.chave(i))?)
            };
            let h = rec(p, ruim(numero, no.filho(i))?, de, ate, profundidade + 1, r)?;
            if altura.is_some_and(|a| a != h) {
                return Err(erro("folhas em profundidades diferentes"));
            }
            altura = Some(h);
        }
        let h = altura.expect("nó interno tem ao menos o filho da direita") + 1;
        if h != no.nivel() as usize + 1 {
            return Err(erro("nível gravado não bate com a altura"));
        }
        Ok(h)
    }
    let mut r = ResumoArvore::default();
    r.altura = rec(p, raiz, None, None, 0, &mut r)?;
    Ok(r)
}
