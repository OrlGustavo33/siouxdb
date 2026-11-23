# SiouxDB Studio

Cliente visual do SiouxDB em [Tauri 2](https://tauri.app). Hoje ele faz uma coisa e faz bem: o **painel de monitoramento em tempo real**.

## O painel

Uma tela só, atualizada a cada segundo (pausa quando a janela fica oculta):

- **Seis números grandes**, cada um com o gráfico dos últimos 60 s: conexões, operações por segundo, latência p99 (com p50), acerto do cache, gravações por segundo e tamanho do banco;
- **alertas** em amarelo/vermelho, só quando algo passa do limite. Os limites são configuráveis (botão de ajustes);
- **conexões agora** e **saúde**: fila do escritor, última verificação, confirmações, último LSN, páginas a liberar, backup e réplicas.

Fontes de dados:
- **Arquivo** (`Abrir` / `Novo`): o Studio abre o arquivo `.sdb` e mede as operações que ele próprio faz;
- **Demonstração**: cria um banco temporário e gera carga (ondas de leitura e gravação com picos) para ver o painel funcionando. O arquivo é apagado ao fechar;
- **Servidor** (quando o `sioux-servidor` existir): o mesmo painel, lendo `/painel/fluxo` do servidor, com todas as conexões e botões Cancelar/Derrubar.

`Verificar` lê todas as páginas e confere a verificação de cada uma.

## Rodar

Linux precisa do WebKitGTK (`libwebkit2gtk-4.1-dev`). Depois:

```bash
cd studio/src-tauri
cargo run
```

A interface (`studio/ui/`) é HTML, CSS e JavaScript puros, sem npm. Aberta direto no navegador ela mostra uma **prévia com dados simulados** (marcada no rodapé), útil para mexer no visual: `python3 -m http.server -d studio/ui` e abra `http://127.0.0.1:8000/#demonstracao`.

## Como está organizado

- `src-tauri/src/nucleo.rs`: sessão, coleta a cada segundo, verificação e carga de demonstração, sem nada de janela (testado com `cargo test`);
- `src-tauri/src/main.rs`: os comandos que a interface chama;
- as métricas vêm do crate `siouxdb-metricas` (histograma sem trava, séries de 60 s, alertas) — o mesmo que o modo servidor vai usar.
