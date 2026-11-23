use std::fmt;

/// Resultado das operações de criptografia.
pub type Resultado<T> = Result<T, ErroCripto>;

/// Erros de criptografia. As mensagens nunca incluem chaves nem senhas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErroCripto {
    /// Senha errada, ou os parâmetros/chave embrulhada do cabeçalho foram alterados.
    SenhaErradaOuAdulterado,
    /// A página não autenticou: alterada, fora do lugar ou de outro arquivo.
    PaginaAdulterada,
    /// Parâmetros do Argon2 fora do aceito.
    ParametrosInvalidos(String),
    /// Buffer pequeno demais para cifrar.
    TamanhoInvalido(usize),
    /// O sistema operacional não forneceu números aleatórios.
    SemAleatorio,
    /// Falha inesperada da biblioteca de cifra.
    FalhaInterna,
}

impl fmt::Display for ErroCripto {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErroCripto::SenhaErradaOuAdulterado => write!(f, "senha errada ou cabeçalho alterado"),
            ErroCripto::PaginaAdulterada => write!(
                f,
                "página não autenticou (alterada, fora do lugar ou de outro arquivo)"
            ),
            ErroCripto::ParametrosInvalidos(m) => write!(f, "parâmetros de senha inválidos: {m}"),
            ErroCripto::TamanhoInvalido(n) => {
                write!(f, "buffer de {n} bytes é pequeno demais para cifrar")
            }
            ErroCripto::SemAleatorio => write!(f, "o sistema não forneceu números aleatórios"),
            ErroCripto::FalhaInterna => write!(f, "falha interna da cifra"),
        }
    }
}

impl std::error::Error for ErroCripto {}
