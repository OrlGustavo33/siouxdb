//! Criptografia do SiouxDB.
//!
//! **Regra do projeto:** nada de cifra inventada. Tudo aqui é cola fina sobre bibliotecas
//! auditadas da RustCrypto:
//!
//! - páginas e envelopes: **XChaCha20-Poly1305** (nonce de 192 bits sorteado a cada
//!   gravação — seguro com nonce aleatório, sem contador para manter);
//! - chave a partir da senha: **Argon2id**, com custo configurável;
//! - chaves em memória são apagadas ao sair de uso (`zeroize`).
//!
//! Modelo de chaves (envelope): a **chave do arquivo** é sorteada na criação e cifra as
//! páginas. Ela é guardada no cabeçalho **embrulhada** pela chave derivada da senha. Trocar
//! a senha = reembrulhar 72 bytes, sem recifrar o banco.

mod erro;

pub use erro::{ErroCripto, Resultado};

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{Key, Tag, XChaCha20Poly1305, XNonce};
use std::fmt;
use zeroize::{Zeroize, Zeroizing};

/// Bytes do nonce (XChaCha20).
pub const TAMANHO_NONCE: usize = 24;
/// Bytes da etiqueta de autenticação (Poly1305).
pub const TAMANHO_TAG: usize = 16;
/// Bytes que a cifra ocupa em cada página (nonce + etiqueta).
pub const SOBRA_CIFRA: usize = TAMANHO_NONCE + TAMANHO_TAG;
/// Bytes de uma chave.
pub const TAMANHO_CHAVE: usize = 32;
/// Bytes do sal da senha.
pub const TAMANHO_SAL: usize = 16;
/// Bytes de uma chave embrulhada (nonce + chave cifrada + etiqueta).
pub const TAMANHO_EMBRULHADA: usize = TAMANHO_NONCE + TAMANHO_CHAVE + TAMANHO_TAG;

/// Chave simétrica de 256 bits. Apagada da memória quando sai de uso; nunca aparece em `Debug`.
#[derive(Clone)]
pub struct Chave(Zeroizing<[u8; TAMANHO_CHAVE]>);

impl fmt::Debug for Chave {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Chave(***)")
    }
}

impl Chave {
    /// Chave sorteada pelo gerador do sistema operacional.
    pub fn aleatoria() -> Resultado<Chave> {
        let mut c = Zeroizing::new([0u8; TAMANHO_CHAVE]);
        aleatorio(c.as_mut())?;
        Ok(Chave(c))
    }

    fn cifra(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(Key::from_slice(self.0.as_ref()))
    }
}

/// Custo do Argon2id. Mais memória e iterações = mais lento para quem tenta adivinhar a senha.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParametrosSenha {
    /// Memória em KiB.
    pub memoria_kib: u32,
    /// Iterações.
    pub iteracoes: u32,
    /// Linhas paralelas.
    pub paralelismo: u32,
}

impl Default for ParametrosSenha {
    /// 64 MiB, 3 iterações, 1 linha (segunda recomendação da RFC 9106).
    fn default() -> Self {
        ParametrosSenha {
            memoria_kib: 64 * 1024,
            iteracoes: 3,
            paralelismo: 1,
        }
    }
}

impl ParametrosSenha {
    /// Custo mínimo, **só para testes** (rápido e fraco).
    pub fn para_testes() -> Self {
        ParametrosSenha {
            memoria_kib: 64,
            iteracoes: 1,
            paralelismo: 1,
        }
    }
}

/// Sorteia um sal novo.
pub fn sal_aleatorio() -> Resultado<[u8; TAMANHO_SAL]> {
    let mut s = [0u8; TAMANHO_SAL];
    aleatorio(&mut s)?;
    Ok(s)
}

/// Deriva a chave da senha com Argon2id.
pub fn derivar_da_senha(
    senha: &[u8],
    sal: &[u8; TAMANHO_SAL],
    p: ParametrosSenha,
) -> Resultado<Chave> {
    let params = argon2::Params::new(
        p.memoria_kib,
        p.iteracoes,
        p.paralelismo,
        Some(TAMANHO_CHAVE),
    )
    .map_err(|e| ErroCripto::ParametrosInvalidos(e.to_string()))?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut c = Zeroizing::new([0u8; TAMANHO_CHAVE]);
    argon
        .hash_password_into(senha, sal, c.as_mut())
        .map_err(|e| ErroCripto::ParametrosInvalidos(e.to_string()))?;
    Ok(Chave(c))
}

/// Cifra `chave` com `kek`. `contexto` é autenticado junto: desembrulhar com outro contexto
/// falha (use os parâmetros do arquivo, para que alterá-los seja detectado).
pub fn embrulhar(
    kek: &Chave,
    chave: &Chave,
    contexto: &[u8],
) -> Resultado<[u8; TAMANHO_EMBRULHADA]> {
    let mut saida = [0u8; TAMANHO_EMBRULHADA];
    aleatorio(&mut saida[..TAMANHO_NONCE])?;
    let (nonce, resto) = saida.split_at_mut(TAMANHO_NONCE);
    let (corpo, tag) = resto.split_at_mut(TAMANHO_CHAVE);
    corpo.copy_from_slice(chave.0.as_ref());
    let t = kek
        .cifra()
        .encrypt_in_place_detached(XNonce::from_slice(nonce), contexto, corpo)
        .map_err(|_| ErroCripto::FalhaInterna)?;
    tag.copy_from_slice(&t);
    Ok(saida)
}

/// Desfaz [`embrulhar`]. Senha errada, contexto diferente ou bytes alterados dão o mesmo
/// erro, de propósito (não diz qual foi).
pub fn desembrulhar(
    kek: &Chave,
    embrulhada: &[u8; TAMANHO_EMBRULHADA],
    contexto: &[u8],
) -> Resultado<Chave> {
    let mut corpo = Zeroizing::new([0u8; TAMANHO_CHAVE]);
    corpo.copy_from_slice(&embrulhada[TAMANHO_NONCE..TAMANHO_NONCE + TAMANHO_CHAVE]);
    kek.cifra()
        .decrypt_in_place_detached(
            XNonce::from_slice(&embrulhada[..TAMANHO_NONCE]),
            contexto,
            corpo.as_mut(),
            Tag::from_slice(&embrulhada[TAMANHO_NONCE + TAMANHO_CHAVE..]),
        )
        .map_err(|_| ErroCripto::SenhaErradaOuAdulterado)?;
    Ok(Chave(corpo))
}

/// Cifra de páginas com a chave do arquivo.
///
/// Layout de uma página cifrada de `n` bytes: `[nonce 24][conteúdo n-40][etiqueta 16]`.
/// Os **dados associados** são o número da página e a versão do formato: uma página
/// cifrada copiada para outra posição não decifra.
#[derive(Debug, Clone)]
pub struct CifraPagina {
    chave: Chave,
}

impl CifraPagina {
    /// Cifra com a chave dada.
    pub fn nova(chave: Chave) -> Self {
        CifraPagina { chave }
    }

    fn aad(numero: u64, versao: u32) -> [u8; 20] {
        let mut a = [0u8; 20];
        a[..8].copy_from_slice(b"SIOUXPAG");
        a[8..16].copy_from_slice(&numero.to_le_bytes());
        a[16..].copy_from_slice(&versao.to_le_bytes());
        a
    }

    /// Cifra no lugar. Antes: conteúdo em claro em `pagina[24..n-16]`. Depois: página pronta
    /// para o disco. Nonce novo a cada chamada.
    pub fn cifrar(&self, numero: u64, versao: u32, pagina: &mut [u8]) -> Resultado<()> {
        let n = pagina.len();
        if n <= SOBRA_CIFRA {
            return Err(ErroCripto::TamanhoInvalido(n));
        }
        let (nonce, resto) = pagina.split_at_mut(TAMANHO_NONCE);
        aleatorio(nonce)?;
        let (corpo, tag) = resto.split_at_mut(n - SOBRA_CIFRA);
        let t = self
            .chave
            .cifra()
            .encrypt_in_place_detached(XNonce::from_slice(nonce), &Self::aad(numero, versao), corpo)
            .map_err(|_| ErroCripto::FalhaInterna)?;
        tag.copy_from_slice(&t);
        Ok(())
    }

    /// Decifra no lugar. Depois: conteúdo em claro em `pagina[24..n-16]`. Se a página foi
    /// alterada, está no lugar errado ou a chave é outra, devolve erro e **apaga** o buffer.
    pub fn decifrar(&self, numero: u64, versao: u32, pagina: &mut [u8]) -> Resultado<()> {
        let n = pagina.len();
        if n <= SOBRA_CIFRA {
            return Err(ErroCripto::TamanhoInvalido(n));
        }
        let (nonce, resto) = pagina.split_at_mut(TAMANHO_NONCE);
        let (corpo, tag) = resto.split_at_mut(n - SOBRA_CIFRA);
        let r = self.chave.cifra().decrypt_in_place_detached(
            XNonce::from_slice(nonce),
            &Self::aad(numero, versao),
            corpo,
            Tag::from_slice(tag),
        );
        if r.is_err() {
            pagina.zeroize();
            return Err(ErroCripto::PaginaAdulterada);
        }
        Ok(())
    }

    /// Etiqueta que autentica `dados` (sem cifrar nada): `[nonce 24][etiqueta 16]`.
    /// Usada para proteger o cabeçalho do arquivo, que precisa ficar legível.
    pub fn autenticar(&self, dados: &[u8]) -> Resultado<[u8; SOBRA_CIFRA]> {
        let mut saida = [0u8; SOBRA_CIFRA];
        aleatorio(&mut saida[..TAMANHO_NONCE])?;
        let t = self
            .chave
            .cifra()
            .encrypt_in_place_detached(XNonce::from_slice(&saida[..TAMANHO_NONCE]), dados, &mut [])
            .map_err(|_| ErroCripto::FalhaInterna)?;
        saida[TAMANHO_NONCE..].copy_from_slice(&t);
        Ok(saida)
    }

    /// Confere uma etiqueta feita por [`CifraPagina::autenticar`].
    pub fn conferir(&self, dados: &[u8], etiqueta: &[u8; SOBRA_CIFRA]) -> bool {
        self.chave
            .cifra()
            .decrypt_in_place_detached(
                XNonce::from_slice(&etiqueta[..TAMANHO_NONCE]),
                dados,
                &mut [],
                Tag::from_slice(&etiqueta[TAMANHO_NONCE..]),
            )
            .is_ok()
    }
}

/// Preenche `destino` com bytes aleatórios do sistema operacional.
pub fn bytes_aleatorios(destino: &mut [u8]) -> Resultado<()> {
    aleatorio(destino)
}

fn aleatorio(destino: &mut [u8]) -> Resultado<()> {
    getrandom::fill(destino).map_err(|_| ErroCripto::SemAleatorio)
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn senha_embrulha_e_desembrulha() {
        let sal = sal_aleatorio().unwrap();
        let p = ParametrosSenha::para_testes();
        let kek = derivar_da_senha(b"senha boa", &sal, p).unwrap();
        let chave = Chave::aleatoria().unwrap();
        let e = embrulhar(&kek, &chave, b"ctx").unwrap();
        let volta = desembrulhar(&kek, &e, b"ctx").unwrap();
        assert_eq!(volta.0.as_ref(), chave.0.as_ref());

        // mesma senha e sal = mesma chave; senha errada, contexto errado ou byte trocado = erro
        let kek2 = derivar_da_senha(b"senha boa", &sal, p).unwrap();
        assert!(desembrulhar(&kek2, &e, b"ctx").is_ok());
        let errada = derivar_da_senha(b"senha ruim", &sal, p).unwrap();
        assert!(matches!(
            desembrulhar(&errada, &e, b"ctx"),
            Err(ErroCripto::SenhaErradaOuAdulterado)
        ));
        assert!(desembrulhar(&kek, &e, b"outro").is_err());
        let mut alterada = e;
        alterada[30] ^= 1;
        assert!(desembrulhar(&kek, &alterada, b"ctx").is_err());
        assert_eq!(format!("{chave:?}"), "Chave(***)");
    }

    #[test]
    fn parametros_invalidos_dao_erro() {
        let sal = [0u8; TAMANHO_SAL];
        let p = ParametrosSenha {
            memoria_kib: 1,
            iteracoes: 1,
            paralelismo: 1,
        };
        assert!(matches!(
            derivar_da_senha(b"x", &sal, p),
            Err(ErroCripto::ParametrosInvalidos(_))
        ));
    }

    #[test]
    fn pagina_cifra_decifra_e_detecta() {
        let c = CifraPagina::nova(Chave::aleatoria().unwrap());
        let mut p = vec![0u8; 4096];
        p[TAMANHO_NONCE..TAMANHO_NONCE + 5].copy_from_slice(b"sioux");
        let claro = p[TAMANHO_NONCE..4096 - TAMANHO_TAG].to_vec();
        c.cifrar(7, 0, &mut p).unwrap();
        assert!(!p.windows(5).any(|w| w == b"sioux"), "texto em claro vazou");
        let cifrada = p.clone();

        c.decifrar(7, 0, &mut p).unwrap();
        assert_eq!(&p[TAMANHO_NONCE..4096 - TAMANHO_TAG], &claro[..]);

        // outra posição, outra versão, outra chave, um bit trocado: tudo falha
        for (num, ver) in [(8, 0), (7, 1)] {
            let mut x = cifrada.clone();
            assert!(matches!(
                c.decifrar(num, ver, &mut x),
                Err(ErroCripto::PaginaAdulterada)
            ));
            assert!(x.iter().all(|&b| b == 0), "buffer deveria ser apagado");
        }
        let outra = CifraPagina::nova(Chave::aleatoria().unwrap());
        assert!(outra.decifrar(7, 0, &mut cifrada.clone()).is_err());
        let mut x = cifrada.clone();
        x[2000] ^= 0x80;
        assert!(c.decifrar(7, 0, &mut x).is_err());

        // mesma página cifrada duas vezes dá bytes diferentes (nonce novo)
        let mut a = vec![1u8; 512];
        let mut b = vec![1u8; 512];
        c.cifrar(1, 0, &mut a).unwrap();
        c.cifrar(1, 0, &mut b).unwrap();
        assert_ne!(a, b);
        assert!(c.cifrar(1, 0, &mut [0u8; 40]).is_err());
    }

    #[test]
    fn autentica_dados_em_claro() {
        let c = CifraPagina::nova(Chave::aleatoria().unwrap());
        let e = c.autenticar(b"cabecalho").unwrap();
        assert!(c.conferir(b"cabecalho", &e));
        assert!(!c.conferir(b"cabecalhO", &e));
        let outra = CifraPagina::nova(Chave::aleatoria().unwrap());
        assert!(!outra.conferir(b"cabecalho", &e));
    }
}
