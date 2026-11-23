//! SiouxDB Studio.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod nucleo;

use nucleo::{PainelResposta, Resumo, Studio, Verificacao};
use siouxdb::metricas::LimitesAlerta;
use std::sync::Arc;
use tauri::State;

type Estado<'a> = State<'a, Arc<Studio>>;

#[tauri::command]
fn abrir(estado: Estado<'_>, caminho: String) -> Result<Resumo, String> {
    estado.abrir(&caminho)
}

#[tauri::command]
fn criar(estado: Estado<'_>, caminho: String, kb: u32) -> Result<Resumo, String> {
    estado.criar(&caminho, kb)
}

#[tauri::command]
fn demonstracao(estado: Estado<'_>) -> Result<Resumo, String> {
    estado.demonstracao()
}

#[tauri::command]
fn fechar(estado: Estado<'_>) {
    estado.fechar();
}

#[tauri::command]
fn painel(estado: Estado<'_>) -> Option<PainelResposta> {
    estado.painel()
}

#[tauri::command]
fn verificar(estado: Estado<'_>) -> Result<Verificacao, String> {
    estado.verificar()
}

#[tauri::command]
fn definir_limites(estado: Estado<'_>, limites: LimitesAlerta) {
    estado.definir_limites(limites);
}

fn main() {
    let studio = Studio::novo();
    Studio::iniciar_coleta(&studio);
    tauri::Builder::default()
        .manage(studio)
        .invoke_handler(tauri::generate_handler![
            abrir,
            criar,
            demonstracao,
            fechar,
            painel,
            verificar,
            definir_limites
        ])
        .run(tauri::generate_context!())
        .expect("falha ao iniciar o SiouxDB Studio");
}
