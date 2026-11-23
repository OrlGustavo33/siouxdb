use crate::NumeroPagina;
use siouxdb_vfs::ErroVfs;
use std::fmt;

/// Resultado das operações de armazenamento.
pub type Resultado<T> = Result<T, ErroArmazenamento>;

/// Erros do armazenamento.
#[derive(Debug)]
pub enum ErroArmazenamento {
    /// Erro da camada de disco.
    Vfs(ErroVfs),
    /// O arquivo não é um banco SiouxDB (assinatura não confere).
    NaoEhSiouxDb,
    /// O arquivo é de uma versão de formato que esta biblioteca não lê.
    VersaoNaoSuportada(u32),
    /// Nenhuma das duas cópias do cabeçalho está íntegra.
    CabecalhoCorrompido,
    /// Tamanho de página inválido no cabeçalho ou na criação.
    TamanhoPaginaInvalido(u32),
    /// A página não passou na verificação (conteúdo alterado ou gravação incompleta).
    PaginaCorrompida(NumeroPagina),
    /// A página lida é de outra posição do arquivo.
    PaginaForaDoLugar {
        /// Página pedida.
        esperada: NumeroPagina,
        /// Número gravado dentro da página encontrada.
        encontrada: NumeroPagina,
    },
    /// Número de página fora do arquivo ou reservado.
    PaginaInexistente(NumeroPagina),
    /// O arquivo é menor do que o cabeçalho diz (foi cortado).
    ArquivoTruncado,
    /// Uma confirmação falhou antes; o estado em disco é incerto. Feche e abra de novo.
    ConfirmacaoFalhou,
    /// O buffer não tem o tamanho de página do arquivo.
    TamanhoDeBufferErrado {
        /// Tamanho de página do arquivo.
        esperado: usize,
        /// Tamanho recebido.
        recebido: usize,
    },
}

impl fmt::Display for ErroArmazenamento {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErroArmazenamento::Vfs(e) => write!(f, "{e}"),
            ErroArmazenamento::NaoEhSiouxDb => write!(f, "o arquivo não é um banco SiouxDB"),
            ErroArmazenamento::VersaoNaoSuportada(v) => {
                write!(f, "versão de formato {v} não suportada")
            }
            ErroArmazenamento::CabecalhoCorrompido => {
                write!(f, "as duas cópias do cabeçalho estão corrompidas")
            }
            ErroArmazenamento::TamanhoPaginaInvalido(t) => {
                write!(f, "tamanho de página inválido: {t} bytes")
            }
            ErroArmazenamento::PaginaCorrompida(n) => {
                write!(f, "página {n} corrompida (verificação falhou)")
            }
            ErroArmazenamento::PaginaForaDoLugar {
                esperada,
                encontrada,
            } => {
                write!(
                    f,
                    "página fora do lugar: pedida {esperada}, encontrada {encontrada}"
                )
            }
            ErroArmazenamento::PaginaInexistente(n) => write!(f, "página {n} não existe"),
            ErroArmazenamento::ArquivoTruncado => {
                write!(f, "o arquivo é menor do que o cabeçalho indica")
            }
            ErroArmazenamento::ConfirmacaoFalhou => {
                write!(
                    f,
                    "uma confirmação anterior falhou; feche e abra o arquivo de novo"
                )
            }
            ErroArmazenamento::TamanhoDeBufferErrado { esperado, recebido } => {
                write!(f, "buffer de {recebido} bytes, a página tem {esperado}")
            }
        }
    }
}

impl std::error::Error for ErroArmazenamento {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ErroArmazenamento::Vfs(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ErroVfs> for ErroArmazenamento {
    fn from(e: ErroVfs) -> Self {
        ErroArmazenamento::Vfs(e)
    }
}
