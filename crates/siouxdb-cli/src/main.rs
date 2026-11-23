//! Comando `sioux`.
//!
//! ```text
//! sioux --version
//! sioux criar <arquivo> [--pagina 4|8|16|32] [--sem-cifra]
//! sioux info  <arquivo>
//! sioux verificar <arquivo>
//! sioux trocar-senha <arquivo>
//! sioux checkpoint <arquivo>
//! sioux tabelas <arquivo>
//! sioux sql <arquivo> "<comandos>"
//! sioux backup <arquivo> <destino>
//! sioux compactar <arquivo> <tabela> [manter_desde_lsn]
//! ```
//!
//! Arquivos novos são **cifrados por padrão**. A senha é pedida no terminal (sem eco) ou
//! lida da variável `SIOUX_SENHA` (para scripts; a nova senha de `trocar-senha` vem de
//! `SIOUX_SENHA_NOVA`).

use siouxdb::armazenamento::{ErroArmazenamento, ParametrosSenha, SenhaNova, TamanhoPagina};
use siouxdb::transacoes::{Banco, Config, ErroBanco};
use siouxdb::vfs::{Vfs, VfsReal};
use std::path::Path;
use std::process::ExitCode;

type Bd = Banco<<VfsReal as Vfs>::Arquivo>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let resultado = match args.as_slice() {
        ["--version" | "-V" | "versao"] => {
            println!(
                "sioux {} (formato {})",
                siouxdb::VERSAO,
                siouxdb::VERSAO_FORMATO
            );
            Ok(())
        }
        ["criar", arquivo, resto @ ..] => criar(arquivo, resto),
        ["info", arquivo] => info(arquivo),
        ["verificar", arquivo] => verificar(arquivo),
        ["trocar-senha", arquivo] => trocar_senha(arquivo),
        ["checkpoint", arquivo] => checkpoint(arquivo),
        ["tabelas", arquivo] => tabelas(arquivo),
        ["sql", arquivo, comando] => sql(arquivo, comando),
        ["backup", arquivo, destino] => backup(arquivo, destino),
        ["compactar", arquivo, tabela, resto @ ..] => compactar(arquivo, tabela, resto),
        _ => {
            ajuda();
            return ExitCode::from(2);
        }
    };
    match resultado {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("erro: {e}");
            ExitCode::FAILURE
        }
    }
}

fn ajuda() {
    println!(
        "SiouxDB {} — pré-alfa (não guarde dados reais)",
        siouxdb::VERSAO
    );
    println!();
    println!("Uso:");
    println!("  sioux --version");
    println!(
        "  sioux criar <arquivo> [--pagina 4|8|16|32] [--sem-cifra]   cria um banco vazio (cifrado por padrão)"
    );
    println!("  sioux info <arquivo>                                      mostra o cabeçalho");
    println!(
        "  sioux verificar <arquivo>                                 lê e confere todas as páginas"
    );
    println!(
        "  sioux trocar-senha <arquivo>                              troca a senha sem recifrar o banco"
    );
    println!();
    println!("Senha: pedida no terminal, ou SIOUX_SENHA / SIOUX_SENHA_NOVA em scripts.");
}

fn pedir_senha(variavel: &str, pergunta: &str) -> Result<String, String> {
    if let Ok(s) = std::env::var(variavel) {
        return Ok(s);
    }
    rpassword::prompt_password(pergunta).map_err(|e| format!("não foi possível ler a senha: {e}"))
}

fn senha_nova(variavel: &str) -> Result<String, String> {
    if let Ok(s) = std::env::var(variavel) {
        return validar_senha(s);
    }
    let s = pedir_senha(variavel, "Nova senha: ")?;
    let conferir = pedir_senha(variavel, "Repita a senha: ")?;
    if s != conferir {
        return Err("as senhas não conferem".into());
    }
    validar_senha(s)
}

fn validar_senha(s: String) -> Result<String, String> {
    if s.chars().count() < 8 {
        return Err("a senha precisa de pelo menos 8 caracteres".into());
    }
    Ok(s)
}

/// Abre o banco (recuperando o WAL); se for cifrado, pede a senha.
fn abrir(arquivo: &str) -> Result<Bd, String> {
    let vfs = VfsReal::novo();
    let caminho = Path::new(arquivo);
    if !caminho.exists() {
        return Err(format!("o arquivo não existe: {arquivo}"));
    }
    match Bd::abrir(&vfs, caminho, None, Config::default()) {
        Err(ErroBanco::Armazenamento(ErroArmazenamento::SenhaNecessaria)) => {
            let senha = pedir_senha("SIOUX_SENHA", "Senha: ")?;
            Bd::abrir(&vfs, caminho, Some(senha.as_bytes()), Config::default())
                .map_err(|e| e.to_string())
        }
        r => r.map_err(|e| e.to_string()),
    }
}

fn criar(arquivo: &str, resto: &[&str]) -> Result<(), String> {
    let mut tamanho = TamanhoPagina::default();
    let mut cifrar = true;
    let mut i = 0;
    while i < resto.len() {
        match resto[i] {
            "--pagina" => {
                let kb = resto
                    .get(i + 1)
                    .ok_or("falta o tamanho depois de --pagina")?;
                tamanho = kb
                    .parse::<u32>()
                    .ok()
                    .and_then(|kb| TamanhoPagina::de_bytes(kb * 1024))
                    .ok_or_else(|| {
                        format!("tamanho de página inválido: {kb} (use 4, 8, 16 ou 32)")
                    })?;
                i += 2;
            }
            "--sem-cifra" => {
                cifrar = false;
                i += 1;
            }
            outra => return Err(format!("opção desconhecida: {outra}")),
        }
    }
    if Path::new(arquivo).exists() {
        return Err(format!("o arquivo já existe: {arquivo}"));
    }
    let senha = if cifrar {
        Some(senha_nova("SIOUX_SENHA")?)
    } else {
        None
    };
    let senha = senha.as_deref().map(|s| SenhaNova {
        senha: s.as_bytes(),
        parametros: ParametrosSenha::default(),
    });
    let b = Bd::criar(
        &VfsReal::novo(),
        Path::new(arquivo),
        tamanho,
        senha,
        Config::default(),
    )
    .map_err(|e| e.to_string())?;
    let p = b.paginador();
    let modo = if p.cifrado() {
        "cifrado"
    } else {
        "EM CLARO (sem cifra)"
    };
    println!(
        "criado {arquivo}: páginas de {} KiB, {modo}",
        p.tamanho_pagina().bytes() / 1024
    );
    Ok(())
}

fn info(arquivo: &str) -> Result<(), String> {
    let b = abrir(arquivo)?;
    let p = b.paginador();
    let c = p.cabecalho();
    let meta = b.ler().map_err(|e| e.to_string())?.meta();
    let est = b.estatisticas();
    let tam = c.tamanho_pagina.bytes() as u64;
    println!("arquivo          {arquivo}");
    println!("formato          {}", siouxdb::VERSAO_FORMATO);
    match &c.protecao {
        Some(pr) => println!(
            "cifra            XChaCha20-Poly1305 · senha Argon2id ({} MiB, {} iterações, {} linha(s))",
            pr.parametros.memoria_kib / 1024,
            pr.parametros.iteracoes,
            pr.parametros.paralelismo
        ),
        None => println!("cifra            nenhuma (arquivo em claro)"),
    }
    println!(
        "página           {} KiB ({} bytes úteis)",
        tam / 1024,
        p.tamanho_util()
    );
    println!("páginas          {}", meta.total_paginas);
    println!(
        "tamanho          {} bytes (+ {} no WAL)",
        meta.total_paginas * tam,
        est.bytes_wal
    );
    println!(
        "último LSN       {} (checkpoint em {})",
        est.lsn_escrito, est.lsn_checkpoint
    );
    println!("páginas no WAL   {}", est.paginas_no_wal);
    println!("raiz do catálogo {}", meta.raiz_catalogo);
    println!("raiz do razão    {}", meta.raiz_razao);
    println!("primeira livre   {}", meta.primeira_livre);
    println!("geração cabeçalho {}", c.geracao);
    Ok(())
}

fn verificar(arquivo: &str) -> Result<(), String> {
    let b = abrir(arquivo)?;
    let leitura = b.ler().map_err(|e| e.to_string())?;
    let total = leitura.meta().total_paginas;
    let mut ruins = 0u64;
    for n in siouxdb::armazenamento::PAGINAS_RESERVADAS..total {
        match leitura.pagina(n) {
            Ok(_) => {}
            Err(
                e @ ErroBanco::Armazenamento(
                    ErroArmazenamento::PaginaCorrompida(_)
                    | ErroArmazenamento::PaginaForaDoLugar { .. },
                ),
            ) => {
                ruins += 1;
                println!("  {e}");
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    if ruins == 0 {
        println!(
            "ok: {total} páginas, todas íntegras (LSN {})",
            leitura.lsn()
        );
        Ok(())
    } else {
        Err(format!("{ruins} página(s) com problema"))
    }
}

fn trocar_senha(arquivo: &str) -> Result<(), String> {
    let b = abrir(arquivo)?;
    let p = b.paginador();
    if !p.cifrado() {
        return Err("o arquivo não é cifrado".into());
    }
    let nova = senha_nova("SIOUX_SENHA_NOVA")?;
    p.trocar_senha(SenhaNova {
        senha: nova.as_bytes(),
        parametros: ParametrosSenha::default(),
    })
    .map_err(|e| e.to_string())?;
    println!("senha trocada");
    Ok(())
}

fn checkpoint(arquivo: &str) -> Result<(), String> {
    let b = abrir(arquivo)?;
    let antes = b.estatisticas();
    b.checkpoint().map_err(|e| e.to_string())?;
    let depois = b.estatisticas();
    println!(
        "checkpoint: {} página(s) do WAL passaram para o arquivo principal (LSN {})",
        antes.paginas_no_wal - depois.paginas_no_wal,
        depois.lsn_checkpoint
    );
    Ok(())
}

fn tabelas(arquivo: &str) -> Result<(), String> {
    let b = abrir(arquivo)?;
    let l = b.ler().map_err(|e| e.to_string())?;
    let linhas = siouxdb::catalogo::catalogo::sys_tabelas(&l).map_err(|e| e.to_string())?;
    if linhas.is_empty() {
        println!("nenhuma tabela");
    }
    for t in linhas {
        let v: Vec<String> = t
            .iter()
            .map(|x| match x {
                siouxdb::catalogo::Valor::Texto(s) => s.clone(),
                siouxdb::catalogo::Valor::Inteiro(i) => i.to_string(),
                outro => format!("{outro:?}"),
            })
            .collect();
        println!(
            "{:<24} id {:<4} colunas {:<3} linhas {:<10} versões {}",
            v[0], v[1], v[2], v[3], v[4]
        );
    }
    Ok(())
}

fn backup(arquivo: &str, destino: &str) -> Result<(), String> {
    let b = abrir(arquivo)?;
    let bytes = b
        .copiar_para(&VfsReal::novo(), Path::new(destino))
        .map_err(|e| e.to_string())?;
    println!("cópia em {destino} ({bytes} bytes, com o WAL ao lado); abre com a mesma senha");
    Ok(())
}

fn compactar(arquivo: &str, tabela: &str, resto: &[&str]) -> Result<(), String> {
    let manter = match resto {
        [] => u64::MAX,
        [l] => l.parse().map_err(|_| format!("LSN inválido: {l}"))?,
        _ => return Err("uso: sioux compactar <arquivo> <tabela> [manter_desde_lsn]".into()),
    };
    let b = abrir(arquivo)?;
    let mut tx = b.escrever().map_err(|e| e.to_string())?;
    let t =
        siouxdb::catalogo::catalogo::tabela_obrigatoria(&tx, tabela).map_err(|e| e.to_string())?;
    let r = t.compactar(&mut tx, manter).map_err(|e| e.to_string())?;
    tx.confirmar().map_err(|e| e.to_string())?;
    b.checkpoint().map_err(|e| e.to_string())?;
    println!(
        "versões: {} → {} ({} chave(s) removida(s) saíram de vez)",
        r.versoes_antes, r.versoes_depois, r.chaves_apagadas
    );
    Ok(())
}

fn texto_valor(v: &siouxdb::catalogo::Valor) -> String {
    use siouxdb::catalogo::Valor;
    match v {
        Valor::Nulo => "NULL".into(),
        Valor::Inteiro(i) => i.to_string(),
        Valor::Real(r) => r.to_string(),
        Valor::Texto(t) => t.clone(),
        Valor::Bytes(b) => b.iter().map(|x| format!("{x:02x}")).collect(),
        Valor::Booleano(b) => b.to_string(),
    }
}

fn sql(arquivo: &str, comando: &str) -> Result<(), String> {
    let b = abrir(arquivo)?;
    let mut tx = b.escrever().map_err(|e| e.to_string())?;
    let saidas = siouxdb::sql::executar(&mut tx, comando, &[]).map_err(|e| e.to_string())?;
    let lsn = tx.confirmar().map_err(|e| e.to_string())?;
    for s in saidas {
        if s.colunas.is_empty() {
            println!("{} linha(s) afetada(s)", s.afetadas);
            continue;
        }
        let texto: Vec<Vec<String>> = s
            .linhas
            .iter()
            .map(|l| l.iter().map(texto_valor).collect())
            .collect();
        let larguras: Vec<usize> = (0..s.colunas.len())
            .map(|i| {
                texto
                    .iter()
                    .map(|l| l[i].chars().count())
                    .chain([s.colunas[i].chars().count()])
                    .max()
                    .unwrap_or(0)
                    .min(60)
            })
            .collect();
        let linha = |v: &[String]| {
            v.iter()
                .zip(&larguras)
                .map(|(x, w)| format!("{:<w$}", x.chars().take(60).collect::<String>(), w = *w))
                .collect::<Vec<_>>()
                .join(" | ")
        };
        println!("{}", linha(&s.colunas));
        println!(
            "{}",
            larguras
                .iter()
                .map(|w| "-".repeat(*w))
                .collect::<Vec<_>>()
                .join("-+-")
        );
        for l in &texto {
            println!("{}", linha(l));
        }
        println!("({} linha(s))", s.linhas.len());
    }
    println!("LSN {lsn}");
    Ok(())
}
