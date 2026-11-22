//! Comando `sioux`. Por enquanto só mostra a versão e o estado do projeto.

fn main() {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        Some("--version") | Some("-V") | Some("versao") => {
            println!(
                "sioux {} (formato {})",
                siouxdb::VERSAO,
                siouxdb::VERSAO_FORMATO
            );
        }
        _ => {
            println!("SiouxDB {} — pré-alfa", siouxdb::VERSAO);
            println!("Banco de dados embutido, cifrado por padrão e verificável.");
            println!("Ainda não guarda dados: veja docs/PLANO_IMPLEMENTACAO.md.");
            println!();
            println!("Uso: sioux --version");
        }
    }
}
