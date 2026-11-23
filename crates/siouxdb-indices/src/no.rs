//! Formato de um nó da árvore dentro do conteúdo da página.
//!
//! ```text
//! 0       nível (0 = folha)
//! 1       reservado
//! 2..4    quantidade de células
//! 4..6    bytes usados pelas células, no fim da página
//! 6..8    bytes mortos no meio das células (recuperados ao compactar)
//! 8..16   filho da direita (só nós internos): chaves >= a última chave do nó
//! 16..    vetor de posições (u16 cada), na ordem das chaves
//! ...     espaço livre
//! fim     células, gravadas do fim para o começo
//!
//! célula de folha:    tamanho da chave u16 | tamanho do valor u16 | chave | valor
//! célula de interno:  tamanho da chave u16 | filho u64 | chave      (filho: chaves < chave)
//! ```
//!
//! Uma página zerada é uma folha vazia.

/// Erro interno: o motivo; quem chama acrescenta o número da página.
pub(crate) type R<T> = Result<T, &'static str>;

pub(crate) const CAB: usize = 16;
const FOLHA: usize = 4;
const INTERNO: usize = 10;

fn u16_em(b: &[u8], i: usize) -> R<usize> {
    b.get(i..i + 2)
        .map(|x| u16::from_le_bytes([x[0], x[1]]) as usize)
        .ok_or("posição fora da página")
}

fn por_u16(b: &mut [u8], i: usize, v: usize) {
    b[i..i + 2].copy_from_slice(&(v as u16).to_le_bytes());
}

/// Maior célula aceita: garante pelo menos 4 por nó, para toda divisão dar certo.
pub(crate) fn maximo_celula(tamanho_conteudo: usize) -> usize {
    (tamanho_conteudo.saturating_sub(CAB)) / 4 - 2
}

/// Célula de folha.
pub(crate) fn celula_folha(chave: &[u8], valor: &[u8]) -> Vec<u8> {
    let mut c = Vec::with_capacity(FOLHA + chave.len() + valor.len());
    c.extend_from_slice(&(chave.len() as u16).to_le_bytes());
    c.extend_from_slice(&(valor.len() as u16).to_le_bytes());
    c.extend_from_slice(chave);
    c.extend_from_slice(valor);
    c
}

/// Célula de nó interno.
pub(crate) fn celula_interna(chave: &[u8], filho: u64) -> Vec<u8> {
    let mut c = Vec::with_capacity(INTERNO + chave.len());
    c.extend_from_slice(&(chave.len() as u16).to_le_bytes());
    c.extend_from_slice(&filho.to_le_bytes());
    c.extend_from_slice(chave);
    c
}

/// Chave de uma célula já separada da página.
pub(crate) fn chave_da_celula(cel: &[u8], folha: bool) -> &[u8] {
    let k = u16::from_le_bytes([cel[0], cel[1]]) as usize;
    let ini = if folha { FOLHA } else { INTERNO };
    &cel[ini..ini + k]
}

/// Filho de uma célula interna já separada da página.
pub(crate) fn filho_da_celula(cel: &[u8]) -> u64 {
    u64::from_le_bytes(cel[2..10].try_into().expect("8 bytes"))
}

/// Visão de leitura de um nó.
#[derive(Clone, Copy)]
pub(crate) struct No<'a>(&'a [u8]);

impl<'a> No<'a> {
    /// Confere o cabeçalho do nó.
    pub fn novo(b: &'a [u8]) -> R<Self> {
        if b.len() < 128 || b.len() > u16::MAX as usize {
            return Err("tamanho de página fora do aceito");
        }
        let no = No(b);
        if CAB + 2 * no.n() + no.usado() > b.len() || no.mortos() > no.usado() {
            return Err("cabeçalho do nó incoerente");
        }
        Ok(no)
    }

    pub fn nivel(&self) -> u8 {
        self.0[0]
    }

    pub fn folha(&self) -> bool {
        self.0[0] == 0
    }

    pub fn n(&self) -> usize {
        u16::from_le_bytes([self.0[2], self.0[3]]) as usize
    }

    fn usado(&self) -> usize {
        u16::from_le_bytes([self.0[4], self.0[5]]) as usize
    }

    fn mortos(&self) -> usize {
        u16::from_le_bytes([self.0[6], self.0[7]]) as usize
    }

    pub fn filho_direito(&self) -> u64 {
        u64::from_le_bytes(self.0[8..16].try_into().expect("8 bytes"))
    }

    /// Espaço livre seguido, entre o vetor de posições e as células.
    fn livre_seguido(&self) -> usize {
        self.0.len() - CAB - 2 * self.n() - self.usado()
    }

    pub fn celula(&self, i: usize) -> R<&'a [u8]> {
        if i >= self.n() {
            return Err("índice de célula fora do nó");
        }
        let pos = u16_em(self.0, CAB + 2 * i)?;
        if pos < CAB + 2 * self.n() {
            return Err("célula em cima do vetor de posições");
        }
        let k = u16_em(self.0, pos)?;
        let tam = if self.folha() {
            FOLHA + k + u16_em(self.0, pos + 2)?
        } else {
            INTERNO + k
        };
        self.0
            .get(pos..pos + tam)
            .ok_or("célula passa do fim da página")
    }

    pub fn chave(&self, i: usize) -> R<&'a [u8]> {
        Ok(chave_da_celula(self.celula(i)?, self.folha()))
    }

    /// Valor da célula `i` (só folha).
    pub fn valor(&self, i: usize) -> R<&'a [u8]> {
        let cel = self.celula(i)?;
        let k = u16::from_le_bytes([cel[0], cel[1]]) as usize;
        Ok(&cel[FOLHA + k..])
    }

    /// Filho `i` de um nó interno (`i == n` é o filho da direita).
    pub fn filho(&self, i: usize) -> R<u64> {
        if i == self.n() {
            return Ok(self.filho_direito());
        }
        Ok(filho_da_celula(self.celula(i)?))
    }

    /// Busca binária: `Ok(i)` se achou, `Err(i)` com a posição onde entraria.
    pub fn buscar(&self, chave: &[u8]) -> R<Result<usize, usize>> {
        let (mut a, mut b) = (0, self.n());
        while a < b {
            let m = (a + b) / 2;
            match self.chave(m)?.cmp(chave) {
                std::cmp::Ordering::Less => a = m + 1,
                std::cmp::Ordering::Greater => b = m,
                std::cmp::Ordering::Equal => return Ok(Ok(m)),
            }
        }
        Ok(Err(a))
    }

    /// Em nó interno: índice do filho onde `chave` está (ou estaria).
    pub fn filho_para(&self, chave: &[u8]) -> R<usize> {
        Ok(match self.buscar(chave)? {
            Ok(i) => i + 1,
            Err(i) => i,
        })
    }

    /// Cópia de todas as células, na ordem.
    pub fn celulas(&self) -> R<Vec<Vec<u8>>> {
        (0..self.n())
            .map(|i| self.celula(i).map(<[u8]>::to_vec))
            .collect()
    }
}

/// Reescreve o nó inteiro com as células dadas (já em ordem). Erro se não couber.
pub(crate) fn reescrever<C: AsRef<[u8]>>(
    b: &mut [u8],
    nivel: u8,
    filho_direito: u64,
    cels: &[C],
) -> R<()> {
    let total: usize = cels.iter().map(|c| c.as_ref().len()).sum();
    if CAB + 2 * cels.len() + total > b.len() {
        return Err("células não cabem no nó");
    }
    b[..CAB].fill(0);
    b[0] = nivel;
    por_u16(b, 2, cels.len());
    por_u16(b, 4, total);
    b[8..16].copy_from_slice(&filho_direito.to_le_bytes());
    let mut pos = b.len();
    for (i, c) in cels.iter().enumerate() {
        let c = c.as_ref();
        pos -= c.len();
        b[pos..pos + c.len()].copy_from_slice(c);
        por_u16(b, CAB + 2 * i, pos);
    }
    Ok(())
}

/// Insere a célula na posição `i`. `Ok(false)` se não há espaço (é preciso dividir o nó).
pub(crate) fn inserir(b: &mut [u8], i: usize, cel: &[u8]) -> R<bool> {
    let no = No::novo(b)?;
    let n = no.n();
    if i > n {
        return Err("posição de inserção fora do nó");
    }
    let precisa = cel.len() + 2;
    if precisa > no.livre_seguido() {
        if precisa > no.livre_seguido() + no.mortos() {
            return Ok(false);
        }
        let (nivel, fd, cels) = (no.nivel(), no.filho_direito(), no.celulas()?);
        reescrever(b, nivel, fd, &cels)?;
    }
    let usado = No::novo(b)?.usado();
    let pos = b.len() - usado - cel.len();
    b[pos..pos + cel.len()].copy_from_slice(cel);
    b.copy_within(CAB + 2 * i..CAB + 2 * n, CAB + 2 * i + 2);
    por_u16(b, CAB + 2 * i, pos);
    por_u16(b, 2, n + 1);
    por_u16(b, 4, usado + cel.len());
    Ok(true)
}

/// Tira a célula `i`. O espaço dela vira "morto" até a próxima compactação.
pub(crate) fn remover(b: &mut [u8], i: usize) -> R<()> {
    let no = No::novo(b)?;
    let (n, usado, mortos) = (no.n(), no.usado(), no.mortos());
    let tam = no.celula(i)?.len();
    b.copy_within(CAB + 2 * i + 2..CAB + 2 * n, CAB + 2 * i);
    por_u16(b, 2, n - 1);
    if n == 1 {
        por_u16(b, 4, 0);
        por_u16(b, 6, 0);
    } else {
        por_u16(b, 4, usado);
        por_u16(b, 6, mortos + tam);
    }
    Ok(())
}

/// Troca o valor da célula `i` de uma folha por outro do mesmo tamanho.
pub(crate) fn trocar_valor(b: &mut [u8], i: usize, valor: &[u8]) -> R<()> {
    let no = No::novo(b)?;
    let cel = no.celula(i)?;
    let k = u16::from_le_bytes([cel[0], cel[1]]) as usize;
    if cel.len() - FOLHA - k != valor.len() {
        return Err("valor de tamanho diferente");
    }
    let pos = u16_em(b, CAB + 2 * i)? + FOLHA + k;
    b[pos..pos + valor.len()].copy_from_slice(valor);
    Ok(())
}

/// Troca o filho `i` de um nó interno (`i == n` é o filho da direita).
pub(crate) fn definir_filho(b: &mut [u8], i: usize, filho: u64) -> R<()> {
    let no = No::novo(b)?;
    if i == no.n() {
        b[8..16].copy_from_slice(&filho.to_le_bytes());
        return Ok(());
    }
    no.celula(i)?;
    let pos = u16_em(b, CAB + 2 * i)? + 2;
    b[pos..pos + 8].copy_from_slice(&filho.to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn insere_remove_compacta() {
        let mut b = vec![0u8; 256];
        assert!(No::novo(&b).unwrap().folha());
        for (i, k) in [b"b", b"d", b"f"].iter().enumerate() {
            assert!(inserir(&mut b, i, &celula_folha(*k, b"valor")).unwrap());
        }
        assert!(inserir(&mut b, 1, &celula_folha(b"c", b"x")).unwrap());
        let no = No::novo(&b).unwrap();
        let chaves: Vec<_> = (0..no.n()).map(|i| no.chave(i).unwrap().to_vec()).collect();
        assert_eq!(chaves, [b"b", b"c", b"d", b"f"]);
        assert_eq!(no.valor(1).unwrap(), b"x");
        assert_eq!(no.buscar(b"d").unwrap(), Ok(2));
        assert_eq!(no.buscar(b"e").unwrap(), Err(3));
        assert_eq!(no.buscar(b"a").unwrap(), Err(0));

        // Enche, remove do meio e insere de novo: precisa compactar para caber.
        let grande = celula_folha(b"m", &[7u8; 50]);
        let mut n = 0;
        while inserir(
            &mut b,
            4,
            &celula_folha(format!("g{n:02}").as_bytes(), &[1u8; 20]),
        )
        .unwrap()
        {
            n += 1;
        }
        assert!(!inserir(&mut b, 4, &grande).unwrap());
        remover(&mut b, 4).unwrap();
        remover(&mut b, 4).unwrap();
        remover(&mut b, 4).unwrap();
        let antes = No::novo(&b).unwrap().celulas().unwrap();
        let pos = No::novo(&b).unwrap().buscar(b"m").unwrap().unwrap_err();
        assert!(
            inserir(&mut b, pos, &grande).unwrap(),
            "compactar deveria abrir espaço"
        );
        let depois = No::novo(&b).unwrap().celulas().unwrap();
        assert_eq!(depois.len(), antes.len() + 1);
        trocar_valor(&mut b, pos, &[9u8; 50]).unwrap();
        assert_eq!(No::novo(&b).unwrap().valor(pos).unwrap(), &[9u8; 50]);
        assert!(trocar_valor(&mut b, pos, &[9u8; 49]).is_err());
    }

    #[test]
    fn no_interno_e_pagina_estragada() {
        let mut b = vec![0u8; 256];
        reescrever(
            &mut b,
            1,
            99,
            &[celula_interna(b"k", 10), celula_interna(b"t", 20)],
        )
        .unwrap();
        let no = No::novo(&b).unwrap();
        assert_eq!((no.nivel(), no.n(), no.filho_direito()), (1, 2, 99));
        assert_eq!(no.filho_para(b"a").unwrap(), 0);
        assert_eq!(
            no.filho_para(b"k").unwrap(),
            1,
            "chave igual à separadora vai para a direita"
        );
        assert_eq!(no.filho_para(b"z").unwrap(), 2);
        assert_eq!((no.filho(0).unwrap(), no.filho(2).unwrap()), (10, 99));
        definir_filho(&mut b, 1, 21).unwrap();
        definir_filho(&mut b, 2, 100).unwrap();
        let no = No::novo(&b).unwrap();
        assert_eq!((no.filho(1).unwrap(), no.filho(2).unwrap()), (21, 100));

        // Bytes aleatórios nunca causam pânico: só erro.
        let mut x = 12345u64;
        for _ in 0..2000 {
            let lixo: Vec<u8> = (0..256)
                .map(|_| {
                    x = x
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    (x >> 33) as u8
                })
                .collect();
            if let Ok(no) = No::novo(&lixo) {
                for i in 0..no.n().min(8) {
                    let _ = no.chave(i);
                    let _ = no.filho(i);
                }
                let _ = no.buscar(b"abc");
                let _ = no.celulas();
            }
        }
    }
}
