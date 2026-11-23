# SiouxDB

**O banco de dados em arquivo para dados em que você precisa confiar.**

Banco relacional embutido, com SQL, num arquivo só, como o SQLite. A diferença está no miolo:

- **cifrado por padrão**, inclusive por coluna, com permissões garantidas pela criptografia;
- **verificável**, com cadeia de hashes que prova que ninguém alterou o passado;
- **histórico consultável**: toda mudança fica registrada (`AS OF`).

> **Estado: pré-alfa.** O projeto acabou de começar. Hoje existe só a camada de acesso a disco (com simulação de queda de energia para testes). **Não use para guardar dados reais.** O caminho completo está em [`docs/PLANO_IMPLEMENTACAO.md`](docs/PLANO_IMPLEMENTACAO.md).

*[English summary below.](#english-summary)*

---

## A ideia

Bancos embutidos comuns **sobrescrevem** o dado: o `UPDATE` apaga o valor antigo, o arquivo fica aberto para quem o copiar e ninguém consegue provar que um registro antigo não foi mexido. O SiouxDB funciona como um **livro-razão**:

- toda mudança é anexada, com hora, e nada é apagado no lugar;
- cada bloco gravado leva o hash do anterior; pontos de controle são assinados e podem ser ancorados num carimbo de tempo externo;
- o arquivo inteiro é cifrado, e cada coluna sensível tem a própria chave. Quem não recebeu a chave de uma coluna **não consegue lê-la, nem com outro programa**;
- destruir a chave de uma pessoa deixa os dados dela ilegíveis, inclusive no histórico e nos backups (direito de exclusão da LGPD/GDPR).

```sql
CREATE TABLE clientes (
  nome     TEXTO,
  cpf      TEXTO CRIPTOGRAFADO PESQUISAVEL,
  titular  CHAVE_TITULAR
);

SELECT * FROM estoque AS OF '2026-11-20 10:00';  -- como estava ontem às 10h
VERIFY;                                          -- alguém alterou o passado?
SELECT SUM(total) FROM vendas WITH ERRO 1%;      -- resposta na hora, com margem de erro
```
*(sintaxe planejada; ainda não implementada)*

Para quem: saúde, jurídico, financeiro, governo, logs de auditoria, cofres de senhas e qualquer app que guarde dado pessoal num arquivo local.

## O ecossistema planejado

| Peça | O que faz |
|---|---|
| **siouxdb** (motor) | SQL relacional, transações, cifra nativa, histórico e verificação. Biblioteca Rust com interface C |
| **sioux-servidor** | O mesmo motor em **modo servidor**, como o PostgreSQL: fala o protocolo do PostgreSQL (psql, DBeaver, JDBC, sqlx... conectam direto), TLS 1.3, SCRAM e gerência de conexões com limites, fila e tempos configuráveis |
| **sioux-studio** | Cliente visual estilo DBeaver, em Tauri, com **painel de monitoramento em tempo real** (conexões, consultas/s, latência, cache, WAL, fila do escritor, saúde). Abre SiouxDB, PostgreSQL, MySQL, Firebird e SQLite |
| **sioux-backup** | Backup a quente, incremental e contínuo, verificado por hash e testado sozinho |
| **sioux-cluster** | Replicação opcional com Raft e quórum configurável |
| **sioux-ia** | IA local opcional: perguntas em português viram SQL conferido, busca por significado |
| **sioux** (CLI) e bibliotecas | Linha de comando, Python, Node e WebAssembly |
| **sioux-migra** | Migração de outros sistemas com mapeamento proposto por IA e revisado por pessoa, com certificado |
| **sioux-sync** | Sincronização contínua com PostgreSQL, MySQL e Firebird |
| **sioux-ponte** | Troca de sistema por partes, com o sistema antigo exposto como API segura |

Só o motor é obrigatório; o resto é opcional. Embutido (como o SQLite) ou servidor (como o PostgreSQL) é escolha de uso: o arquivo é o mesmo.

## Stack

- **Linguagem:** Rust (edição 2024), workspace Cargo, `unsafe` proibido por padrão.
- **SQL:** `sqlparser` + dialeto próprio; execução vetorizada.
- **Armazenamento:** páginas próprias (sem `mmap`), WAL com gravação em grupo, camada quente em log, camada fria em colunas comprimidas com resumos por bloco.
- **Criptografia:** só bibliotecas auditadas (RustCrypto): XChaCha20-Poly1305 / AES-256-GCM, Argon2id, BLAKE3, Ed25519, HMAC para índice cego.
- **Integridade:** cadeia de hashes, Merkle Mountain Range, pontos de controle assinados.
- **Interfaces:** C (`cbindgen`), Python (`pyo3`), Node (`napi-rs`), WebAssembly; Studio em Tauri.
- **Cluster:** Raft (`openraft`).
- **Qualidade:** simulação determinística com quedas de energia, fuzzing (`cargo-fuzz`), comparação de resultados com o SQLite (`sqllogictest`), Miri, `cargo-deny`, benchmarks contra SQLite e DuckDB.

## Estado e roteiro

| Versão | Conteúdo |
|---|---|
| **v0.1** | Núcleo confiável: arquivo cifrado, SQL básico, transações, WAL, cadeia de hashes, `VERIFY`, `AS OF`, CLI e interface C |
| v0.2 | Modo servidor (protocolo do PostgreSQL, gerência de conexões), Studio com painel em tempo real e Backup |
| v0.3 | Migra, Sync e Ponte |
| v0.4 | Análise e desempenho: colunas, índices adaptativo e aprendido, perfis e ajuste automático |
| v0.5+ | Cluster com Raft e IA local |

**Feito até agora (M0, M1 e M2):**
- workspace e CI em Linux, macOS e Windows;
- `siouxdb-vfs`: disco real e disco simulado. O simulado reproduz queda de energia com gravações perdidas ou cortadas no meio de um setor, de forma determinística por semente;
- `siouxdb-armazenamento`: páginas com verificação (CRC32C + número da página), cabeçalho em duas cópias alternadas, cache SIEVE e paginador. Testado com centenas de quedas simuladas: nunca aceita página misturada;
- `siouxdb-cripto`: arquivo **cifrado por padrão** — cada página com XChaCha20-Poly1305 (página trocada de lugar ou alterada não decifra), senha por Argon2id, chave do arquivo em envelope (trocar a senha não recifra nada) e cabeçalho autenticado. Os testes de queda de energia rodam também com cifra;
- comando `sioux criar | info | verificar | trocar-senha`.
- `siouxdb-metricas`: histograma sem trava, séries de 60 s e alertas configuráveis;
- **SiouxDB Studio** (`studio/`, Tauri): painel de monitoramento em tempo real sobre um arquivo ou uma demonstração com carga gerada.

## Compilar e testar

```bash
git clone https://github.com/OrlGustavo33/siouxdb
cd siouxdb
cargo test
cargo run -p siouxdb-cli -- criar teste.sdb     # pede uma senha (cifrado por padrão)
cargo run -p siouxdb-cli -- info teste.sdb
```

## Como o projeto nasceu

O SiouxDB foi **idealizado pela Sioux** (SiouxSys, sistemas de gestão para varejo, atacado e serviços) a partir da necessidade de guardar dados de clientes com segurança, migrar bases de sistemas antigos e provar a integridade de registros financeiros e fiscais.

O desenho e o código estão sendo desenvolvidos com o **Claude**, da Anthropic, usando o **Claude Code dentro do VS Code**: arquitetura, código, testes e documentação escritos em parceria com a IA, com revisão e decisão humanas. Contribuições de pessoas são muito bem-vindas.

## Contribuir e segurança

- Como contribuir: [`CONTRIBUTING.md`](CONTRIBUTING.md).
- Falhas de segurança: **não abra issue pública**, siga [`SECURITY.md`](SECURITY.md).
- Especificações: [`spec/`](spec/).

## Licença

Escolha a que preferir: [MIT](LICENSE-MIT) ou [Apache-2.0](LICENSE-APACHE).

Toda contribuição enviada para este projeto é licenciada nos mesmos dois termos, salvo aviso explícito em contrário.

---

## English summary

**SiouxDB** is an embedded relational SQL database in a single file, written in Rust, designed for data you need to trust:

- **encrypted by default**, including per-column keys, where permissions are enforced cryptographically;
- **tamper-evident**, with a hash chain, Merkle Mountain Range and signed checkpoints;
- **time travel**: every change is kept and queryable with `AS OF`.

It can also run in **server mode**, like PostgreSQL: same file, speaking the PostgreSQL wire protocol (so `psql`, JDBC, `sqlx`, etc. connect as-is), TLS 1.3 + SCRAM, and careful connection management (per-user/IP limits, admission queue, idle-in-transaction timeouts, fair single-writer queue, graceful shutdown).

The planned ecosystem adds a desktop studio (Tauri) with a simple real-time monitoring dashboard, hot/incremental backups, optional Raft replication, optional local AI, and migration, sync and bridge tools for moving off legacy databases.

**Status: pre-alpha.** The disk layer (with a deterministic crash simulator) and checksummed page storage (dual header, SIEVE cache) and per-page encryption (XChaCha20-Poly1305, Argon2id password, envelope key, authenticated header) exist today. Do not store real data yet.

The project was conceived by **Sioux** (SiouxSys) and is being built with **Claude** (Anthropic) through **Claude Code in VS Code**, with human review. It is dual-licensed under MIT or Apache-2.0.
