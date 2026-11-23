//! Comando `sioux`.
//!
//! ```text
//! sioux --version
//! sioux criar <arquivo> [--pagina 4|8|16|32]
//! sioux info  <arquivo>
//! sioux verificar <arquivo>
//! ```

use siouxdb::armazenamento::{ErroArmazenamento, Paginador, TamanhoPagina};
use siouxdb::vfs::{Vfs, VfsReal};
use std::path::Path;
use std::process::ExitCode;

type Pag = Paginador<<VfsReal as Vfs>::Arquivo>;

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
    println!("  sioux criar <arquivo> [--pagina 4|8|16|32]   cria um banco vazio");
    println!("  sioux info <arquivo>                         mostra o cabeçalho");
    println!("  sioux verificar <arquivo>                    lê e confere todas as páginas");
}

fn criar(arquivo: &str, resto: &[&str]) -> Result<(), String> {
    let tamanho = match resto {
        [] => TamanhoPagina::default(),
        ["--pagina", kb] => kb
            .parse::<u32>()
            .ok()
            .and_then(|kb| TamanhoPagina::de_bytes(kb * 1024))
            .ok_or_else(|| format!("tamanho de página inválido: {kb} (use 4, 8, 16 ou 32)"))?,
        _ => return Err("opção desconhecida (use --pagina 4|8|16|32)".into()),
    };
    let p =
        Pag::criar(&VfsReal::novo(), Path::new(arquivo), tamanho, 16).map_err(|e| e.to_string())?;
    println!(
        "criado {arquivo} (páginas de {} KiB)",
        p.tamanho_pagina().bytes() / 1024
    );
    Ok(())
}

fn info(arquivo: &str) -> Result<(), String> {
    let p = Pag::abrir(&VfsReal::novo(), Path::new(arquivo), 16).map_err(|e| e.to_string())?;
    let c = p.cabecalho();
    let tam = c.tamanho_pagina.bytes() as u64;
    println!("arquivo          {arquivo}");
    println!("formato          {}", siouxdb::VERSAO_FORMATO);
    println!("página           {} KiB", tam / 1024);
    println!("páginas          {}", c.total_paginas);
    println!("tamanho          {} bytes", c.total_paginas * tam);
    println!("geração          {}", c.geracao);
    println!("raiz do catálogo {}", c.raiz_catalogo);
    println!("raiz do razão    {}", c.raiz_razao);
    println!("último LSN       {}", c.ultimo_lsn);
    println!("primeira livre   {}", c.primeira_livre);
    Ok(())
}

fn verificar(arquivo: &str) -> Result<(), String> {
    let p = Pag::abrir(&VfsReal::novo(), Path::new(arquivo), 16).map_err(|e| e.to_string())?;
    let total = p.cabecalho().total_paginas;
    let mut ruins = 0u64;
    for n in siouxdb::armazenamento::PAGINAS_RESERVADAS..total {
        match p.ler(n) {
            Ok(_) => {}
            Err(
                e @ (ErroArmazenamento::PaginaCorrompida(_)
                | ErroArmazenamento::PaginaForaDoLugar { .. }),
            ) => {
                ruins += 1;
                println!("  {e}");
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    if ruins == 0 {
        println!("ok: {total} páginas, todas íntegras");
        Ok(())
    } else {
        Err(format!("{ruins} página(s) com problema"))
    }
}
