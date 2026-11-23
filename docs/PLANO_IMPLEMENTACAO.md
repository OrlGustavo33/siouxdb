# SiouxDB — plano de implementação

**Para:** quem vai construir o projeto (pessoas e agentes de IA).
**Base:** o desenho do ecossistema (9 peças, roteiro v0.1 → v0.5+) descrito no README.
**Situação em 23/11/2026:** M0 a M4 prontos (M4: árvore B+, tabelas versionadas com log quente, catálogo `sys_*`); M3: (M3: WAL encadeado, transações tudo-ou-nada, escritor único com fila justa, confirmação em grupo, leitores com foto, checkpoint automático, trava de arquivo); antes disso, (disco real/simulado, páginas verificadas, cabeçalho duplo, cache SIEVE, cifra por página com senha Argon2id, CLI `criar`/`info`/`verificar`/`trocar-senha`) e o painel do Studio. Este documento é o caminho até a v0.5.

**Requisitos acrescentados em 22/11:** o SiouxDB também roda em **modo servidor**, como o PostgreSQL (seção 2.11), com **gerência de conexões** cuidadosa (2.12), e o **Studio** tem um **painel de monitoramento em tempo real**, simples e objetivo (2.13).

---

## 0. Decisões antes da primeira linha de código

| # | Decisão | Proposta | Situação |
|---|---|---|---|
| D1 | Nome definitivo | SiouxDB | ✅ decidido (falta conferir o nome no crates.io/npm/PyPI antes de publicar pacotes) |
| D2 | Idioma do código e da documentação técnica | Português do Brasil, com resumo em inglês no README | ✅ decidido |
| D3 | Licença | MIT OU Apache-2.0 (padrão Rust) | ✅ decidido |
| D4 | Repositório | Público e separado do ERP: `github.com/OrlGustavo33/siouxdb`, Cargo workspace | ✅ criado em 22/11 |
| D5 | Rust mínimo | estável atual, edição 2024, MSRV fixado e testado no CI | proposta |
| D6 | Plataformas da v0.1 | Linux x86_64/arm64, macOS, Windows | proposta |

**Regras que não mudam:**
1. Criptografia só com bibliotecas auditadas. O "algoritmo próprio" fica em índice e armazenamento, nunca em cifra.
2. Nada de `mmap` para o arquivo do banco: gerenciador de páginas próprio.
3. `unsafe` só onde medido e necessário, documentado, e rodando sob Miri no CI.
4. O formato do arquivo é contrato público: documentado, versionado, com arquivos de exemplo no repositório.
5. IA propõe e nunca grava sozinha.
6. Toda funcionalidade nova entra com testes e, se mexer em desempenho, com benchmark.

---

## 1. Estrutura do repositório

```
siouxdb/
├─ crates/
│  ├─ siouxdb-vfs        # acesso a arquivo/disco (real e simulado para testes)
│  ├─ siouxdb-cripto         # envelopes de chave, cifra de página e coluna, Argon2id, índice cego  ✅ M2
│  ├─ siouxdb-armazenamento  # páginas, cache (SIEVE), WAL, camada quente, camada fria em colunas  ✅ M1
│  ├─ siouxdb-razao          # cadeia de hashes, Merkle Mountain Range, pontos de controle assinados
│  ├─ siouxdb-transacoes     # WAL, transações, escritor único com fila justa, fotos, checkpoint  ✅ M3
│  ├─ siouxdb-indices        # B+tree (✅ M4), ART, índice adaptativo, índice aprendido (PGM)
│  ├─ siouxdb-sql            # parser (sqlparser) + extensões, planejador, execução vetorizada
│  ├─ siouxdb-catalogo       # valores, log quente, tabelas versionadas, catálogo sys_* (✅ M4); usuários e permissões depois
│  ├─ siouxdb-metricas       # contadores, histogramas sem trava, séries de 60 s (base do painel)
│  ├─ sioux-servidor         # modo servidor: protocolo do PostgreSQL, TLS, SCRAM, gerência de conexões
│  ├─ siouxdb            # fachada: API pública em Rust (é o que as pessoas usam)
│  ├─ siouxdb-capi       # interface C (cbindgen) — base para outras linguagens
│  ├─ siouxdb-cli        # comando `sioux`
│  ├─ sioux-backup       # backup a quente, incremental, contínuo, restauração
│  ├─ sioux-migra        # migrador com IA
│  ├─ sioux-sync         # captura de mudanças (PostgreSQL, MySQL, Firebird)
│  ├─ sioux-ponte        # API sobre o banco do sistema antigo
│  ├─ sioux-cluster      # Raft com quórum configurável
│  └─ sioux-ia           # modelos locais, texto→SQL, funções de IA, vetores
├─ bindings/
│  ├─ python/            # pyo3 + maturin
│  ├─ node/              # napi-rs
│  └─ wasm/              # wasm-bindgen (VFS em memória/OPFS)
├─ studio/               # app Tauri (Rust + front)
├─ spec/                 # FORMATO.md, PROTOCOLO_LEDGER.md, SQL.md, arquivos de exemplo
├─ sim/                  # simulador determinístico
├─ fuzz/                 # alvos do cargo-fuzz
├─ bench/                # criterion, YCSB, TPC-H reduzido
├─ docs/                 # guias, site
├─ SECURITY.md  CONTRIBUTING.md  LICENSE-MIT  LICENSE-APACHE
└─ .github/workflows/
```

Dependência entre crates (de baixo para cima): `vfs → cripto → armazenamento → transacoes → razao → indices → catalogo → sql → siouxdb → (capi, cli, servidor, ferramentas, bindings, studio)`. `metricas` não depende de nada e é usado por todos.

Nomes de crates, tipos, funções e tabelas do sistema em **português** (ex.: `sys_conexoes`, não `sys_connections`).

---

## 2. Arquitetura técnica (o que construir em cada camada)

### 2.1 VFS — `siouxdb-vfs`
- Trait `Vfs` com abrir, ler/gravar em posição, `fsync`, travar arquivo, tamanho, apagar e renomear de forma atômica.
- Duas implementações:
  - **real**, usando `std::fs` primeiro e `io_uring` depois, como opcional;
  - **simulada**, em memória, que injeta falhas (gravação pela metade, `fsync` que mente, disco cheio, queda em ponto sorteado). O simulador determinístico depende dela.
- Leitura e gravação alinhadas a 4 KiB; opção de `O_DIRECT` mais tarde.

### 2.2 Formato do arquivo (`spec/FORMATO.md`)
- **Cabeçalho duplo** (cópias A e B, cada uma com CRC e número de geração), com:
  - versão do formato e tamanho de página (padrão 8 KiB, configurável na criação);
  - sal e parâmetros do Argon2id, e a chave-mestra embrulhada;
  - raiz do catálogo e raiz do ledger.
- **Página** = cabeçalho (tipo, número, LSN, versão) + conteúdo + **tag de autenticação**. Cada página é cifrada com XChaCha20-Poly1305: nonce aleatório por gravação, número da página e versão como dados associados, o que impede trocar uma página de lugar.
- **Tipos de página:**
  - catálogo;
  - log da camada quente;
  - nó de B+tree;
  - bloco frio (colunas);
  - resumo de bloco;
  - lista de páginas livres;
  - ledger.
- **Arquivos:** `banco.sdb` + `banco.sdb-wal`. O WAL é cifrado da mesma forma.
- **Compatibilidade:** a v1.x lê todo arquivo 1.0. Os arquivos dourados em `spec/exemplos/` são abertos em todo CI.

### 2.3 Criptografia — `siouxdb-crypto`
- **Hierarquia de chaves (envelope):**
  - **senha ou cofre do sistema** → Argon2id (memória e tempo configuráveis) → **KEK do usuário**;
  - **KEK** embrulha a **chave do arquivo** (página) e as **chaves de coluna** a que aquele usuário tem direito;
  - **chave por titular** (opcional, por linha), para destruição seletiva (crypto-shredding).
- **Coluna cifrada:** XChaCha20-Poly1305. **Coluna pesquisável:** valor cifrado + **índice cego** (HMAC-SHA256 com chave própria da coluna, truncado e configurável).
- **Mascaramento** por permissão (`***.***.***-##`), feito no motor, depois de decifrar e antes de devolver.
- **Higiene:** `zeroize` em toda chave, `secrecy` para tipos sensíveis e `mlock` opcional.
- **Fontes de chave plugáveis:** senha, keyring do sistema (crate `keyring`), variável de ambiente e, mais tarde, TPM/YubiKey.
- **Crates:** `chacha20poly1305`, `aes-gcm`, `argon2`, `hmac`, `sha2`, `blake3`, `ed25519-dalek`, `rand_core`/`getrandom`, `zeroize`, `secrecy`.

### 2.4 WAL e transações — `siouxdb-storage` + `siouxdb-mvcc`
- **Um escritor de cada vez** (trava de processo e de arquivo) e **leitores ilimitados** lendo uma foto.
- **Registro do WAL:** LSN, id da transação, operação, dados, CRC32C e hash encadeado ao registro anterior.
- **Gravação em grupo:** junta as transações que chegam até `grupo_espera_max` e faz um só `fsync`.
- **Recuperação ao abrir:** relê o WAL desde o último ponto de controle, descarta o final incompleto (CRC ou hash inválido) e reaplica. Como nada é sobrescrito no lugar, a recuperação é muito mais simples que um ARIES completo.
- **Modos de sincronia:** `total | grupo | normal | nenhuma` (como o `synchronous` do SQLite), documentando o risco de cada um.
- **Versões:** toda mudança vira uma nova versão com `(início, fim)` em tempo de transação. `AS OF` = foto num ponto passado.
- **Liberação de memória:** por épocas (`crossbeam-epoch`), para versões antigas na memória. No disco, nada é apagado (é o livro-razão).

### 2.5 Armazenamento — `siouxdb-storage`
- **Camada quente:** log anexado de linhas, mais um índice primário em B+tree (ou ART em memória) que aponta para a versão mais nova.
- **Camada fria:** quando o log passa de `bloco_frio_linhas` (padrão 65.536), uma tarefa de fundo converte em **bloco em colunas**, imutável:
  - por coluna: dicionário (texto), frame-of-reference + bit-packing (inteiro), delta (data e sequência), RLE (repetição) e ALP (decimal) a partir da v0.4;
  - **resumo do bloco:** mínimo, máximo, contagem, nulos, soma, HyperLogLog e t-digest;
  - filtro por bloco (binary fuse) para igualdade.
- **Cache de páginas** com descarte **SIEVE**, tamanho em bytes ou em % da RAM, respeitando cgroups.
- **Compactação e conversão** com limite de velocidade (token bucket) e janela de horário.

### 2.6 Ledger — `siouxdb-ledger`
- **Cadeia:** cada bloco frio e cada segmento do WAL carregam `hash = BLAKE3(conteúdo || hash_anterior)`.
- **Merkle Mountain Range** sobre os blocos, com prova de inclusão (uma linha está no banco) e de consistência (o banco de hoje estende o de ontem).
- **Ponto de controle:** a cada N minutos ou M transações, `{raiz, altura, LSN, hora}` assinado com Ed25519. A chave de assinatura fica protegida pela KEK.
- **Âncora externa (opcional):** a raiz diária vai para um carimbo de tempo RFC 3161 ou para o OpenTimestamps. A prova fica guardada no próprio banco.
- **Testemunhas (opcional):** pontos de controle enviados a terceiros que coassinam.
- **`VERIFY`:** recalcula a cadeia e a MMR, confere as assinaturas e as âncoras e aponta o primeiro bloco divergente.
- **`spec/PROTOCOLO_LEDGER.md`:** como qualquer pessoa verifica um arquivo com uma ferramenta independente.

### 2.7 Índices — `siouxdb-index`
- **v0.1:** B+tree no disco (chave primária e índices secundários declarados) com compressão de prefixo.
- **v0.4:**
  - **ART** com travas otimistas em memória;
  - **índice adaptativo** (cracking), ligado por coluna com limite de memória;
  - **índice aprendido** (PGM) nos blocos frios ordenados.

### 2.8 SQL — `siouxdb-sql`
- **Parser:** `sqlparser` com dialeto próprio (extensões: `AS OF`, `CHANGES BETWEEN`, `CRIPTOGRAFADO`, `PESQUISAVEL`, `CHAVE_TITULAR`, `WITH ERRO`, `VERIFY`, `CONFIG`, `BACKUP`).
- **Subconjunto da v0.1:**
  - `CREATE/DROP TABLE`, `CREATE/DROP INDEX`, `INSERT`, `UPDATE`, `DELETE`, `SELECT`;
  - `WHERE`, `ORDER BY`, `LIMIT/OFFSET`, `INNER/LEFT JOIN`, `GROUP BY`, `HAVING`;
  - `COUNT/SUM/AVG/MIN/MAX`, `BEGIN/COMMIT/ROLLBACK`;
  - tipos: inteiro, decimal exato, texto, booleano, data/hora, bytes, JSON (texto validado).
- **Planejador:** regras simples, que levam o filtro para baixo, podam colunas, usam índice e pulam blocos pelos resumos. Custo por estatística a partir da v0.4.
- **Execução:**
  - **v0.1:** iteradores por lote, que já processam vetores de 1.024 linhas, para não reescrever depois;
  - **v0.4:** SIMD, materialização tardia, morsels com `rayon` ou executor próprio, hash join particionado, spill para disco.
- **Controle de admissão e orçamento** por consulta (tempo e memória) desde a v0.1, mesmo simples.

### 2.9 Catálogo, usuários e permissões — `siouxdb-catalog`
- **Tabelas do sistema `sys_*`:** tabelas, colunas, índices, usuários, papéis, permissões, chaves embrulhadas, configuração, sugestões do otimizador, registro de leitura de colunas sensíveis.
- **Modo sem usuários** (como o SQLite: uma senha abre tudo) e **modo com usuários** (cada um com KEK própria). A permissão de coluna é **criptográfica**: sem a chave embrulhada para o usuário, a coluna não abre.
- `GRANT LER_ABERTO / LER_MASCARADO / ESCREVER ... ON tabela(coluna) TO papel`. Revogar = rotacionar a chave da coluna e reembrulhar só para quem fica.

### 2.10 API pública — `siouxdb`
```rust
let db = siouxdb::Banco::abrir("loja.sdb")
    .senha_de(Fonte::Keyring("loja"))
    .perfil(Perfil::Desktop)
    .abrir()?;
let mut tx = db.escrever()?;
tx.executar("INSERT INTO clientes (nome, cpf) VALUES (?, ?)", params!["ANA", "123..."])?;
tx.confirmar()?;
for linha in db.ler()?.consultar("SELECT nome FROM clientes WHERE cpf = ?", params!["123..."])? { ... }
```
- Tipos de erro claros; nada de `panic` em caminho de usuário.
- `siouxdb-capi` expõe a mesma coisa em C (`sioux_open`, `sioux_exec`, `sioux_prepare`, `sioux_step`...), com cabeçalho gerado por `cbindgen`.

### 2.11 Modo servidor — `sioux-servidor`
O mesmo motor, o mesmo arquivo: em vez de cada programa abrir o arquivo, um processo `sioux servidor` fica dono dele (trava exclusiva) e atende pela rede. Embutido e servidor são **dois jeitos de usar o mesmo banco**: dá para começar embutido e virar servidor sem migrar nada.

- **Protocolo:** o do PostgreSQL (versão 3), com a crate `pgwire`. Assim `psql`, DBeaver, JDBC, npgsql, `sqlx`, `psycopg`, `node-postgres` e o próprio Studio conectam sem driver novo. Um `pg_catalog` mínimo responde às perguntas que essas ferramentas fazem ao conectar. Diferenças de dialeto ficam documentadas em `spec/SQL.md`.
- **Vários bancos:** uma pasta de dados com um arquivo por banco (`dados/loja.sdb`, `dados/clinica.sdb`); o nome do banco na conexão escolhe o arquivo.
- **Segurança da conexão (padrão ligado):**
  - TLS 1.3 obrigatório (`rustls`); sem TLS só em `127.0.0.1`/socket Unix, e só se configurado;
  - autenticação SCRAM-SHA-256 (nunca senha em claro); certificado do cliente (mTLS) opcional, por usuário;
  - regras de acesso por rede, usuário e banco em `sioux-acesso.toml` (equivalente ao `pg_hba.conf`, com recusa por padrão);
  - bloqueio progressivo depois de senhas erradas por usuário e por IP;
  - toda entrada, saída e recusa vai para a trilha encadeada do razão.
- **Administração:** `sioux servidor iniciar|parar|recarregar|estado`; configuração em `sioux-servidor.toml`, recarregável sem derrubar conexões; serviço systemd/Windows.
- **Monitoramento:** porta administrativa separada (padrão: só `127.0.0.1`) com `/metricas` (formato Prometheus) e `/painel/fluxo` (uma foto por segundo, usado pelo Studio); exige token.
- **Cluster:** quando o `sioux-cluster` existir, cada servidor é uma máquina do Raft; o cliente fala com qualquer uma (leituras locais com atraso máximo, escritas encaminhadas ao líder).

### 2.12 Gerência de conexões
Objetivo: muitas conexões baratas, nenhuma conexão capaz de travar o banco, e tudo visível.

- **Sessão leve:** uma tarefa `tokio` por conexão (poucos KB), não um processo como no PostgreSQL. Milhares de conexões ociosas custam pouco, então não é preciso PgBouncer na frente.
- **Limites (todos configuráveis):** total do servidor, por usuário, por banco e por IP de origem; vagas reservadas para administrador (para conseguir entrar e consertar quando estiver cheio); limite de novas conexões por segundo por IP.
- **Admissão com fila:** passou do limite de consultas **em execução**, a consulta espera numa fila curta (com tempo máximo e prioridade por papel) em vez de derrubar o servidor; fila cheia → recusa com erro claro. CoDel na fila evita espera infinita sob sobrecarga.
- **Tempos limite:**
  - autenticação (padrão 10 s);
  - conexão ociosa;
  - **ociosa dentro de transação** (a mais perigosa: segura foto antiga e, se for de escrita, o escritor único);
  - duração máxima de consulta e de transação;
  - espera pela vez do escritor.
- **Escritor único justo:** transações de escrita entram em fila por ordem de chegada, com tempo máximo de espera; leitores nunca esperam escritor. O painel mostra quem está escrevendo e quem está na fila.
- **Memória:** orçamento por sessão e global; consulta que estoura vai para disco (spill) ou é cancelada com erro explicando o limite.
- **Cancelar e derrubar:** `CancelRequest` do protocolo, `SELECT sys_cancelar(id)` (para a consulta) e `SELECT sys_derrubar(id)` (fecha a conexão), também pelo Studio.
- **Visibilidade:** `sys_conexoes` (id, usuário, banco, origem, aplicação, TLS, estado, desde, consulta atual, tempo nela, esperando o quê, bytes in/out) e `sys_esperas`.
- **Comandos preparados e planos:** por sessão com limite de quantidade; cache de planos compartilhado entre sessões.
- **Desligamento gracioso:** para de aceitar, espera as transações acabarem por N segundos, cancela o resto, faz ponto de controle e fecha. `recarregar` aplica nova configuração sem derrubar ninguém.
- **Proteções:** tamanho máximo de mensagem, de consulta e de resultado; keepalive TCP para descobrir cliente que sumiu.

### 2.13 Monitoramento e painel do Studio — `siouxdb-metricas`
- **Coleta sempre ligada e barata:** contadores e medidores atômicos, histogramas logarítmicos sem trava (latência p50/p95/p99 em nanossegundos de custo) e séries de 60 pontos (1 por segundo). Funciona igual embutido e servidor.
- **Saídas:** `SELECT * FROM sys_metricas`, `/metricas` (Prometheus) e `/painel/fluxo` (servidor), e uma `Foto` em Rust para quem embute.
- **O painel (uma tela só, sem rolagem num monitor comum):**
  1. **Seis números grandes**, cada um com gráfico dos últimos 60 s: conexões (ativas/limite), consultas por segundo, latência p99, acerto do cache, gravações no WAL por segundo, tamanho do banco;
  2. **Conexões agora:** quem, de onde, estado, consulta atual e há quanto tempo, com botões Cancelar/Derrubar;
  3. **Consultas lentas:** as 10 piores do período, com plano;
  4. **Escritor e fila:** quem escreve, quantos esperam, maior espera;
  5. **Saúde:** última verificação do razão, último ponto de controle, último backup e o próximo, atraso das réplicas (quando houver);
  6. **Alertas** em cores, com limites configuráveis (ex.: p99 > 50 ms, cache < 90 %, fila > 5).
- **Regra de desenho:** simples e objetivo. Números grandes, verde/amarelo/vermelho só quando muda algo, nada de 40 gráficos. Atualiza a cada segundo, pausa quando a janela não está visível.
- **Fontes:** arquivo local (o Studio abre o arquivo e lê as métricas do próprio processo) ou servidor (conecta em `/painel/fluxo` com token).

---

## 3. Testes e confiança (vale para todas as fases)

| Camada de teste | Ferramenta | Quando |
|---|---|---|
| Unidade e propriedades | `cargo test`, `proptest` | todo PR |
| **Simulação determinística** | `sim/` sobre o VFS simulado: sementes aleatórias, quedas em qualquer ponto, `fsync` mentiroso, relógio controlado; invariantes conferidos depois de reabrir | todo PR (rodada curta) + noturno (milhões de passos) |
| Fuzzing | `cargo-fuzz` no parser SQL, no leitor de página e WAL, na decifração e no `VERIFY`; inscrever no OSS-Fuzz | contínuo |
| Comparação de resultados | mesmas consultas no SiouxDB e no SQLite (`sqllogictest`), comparando resultados | todo PR |
| Formato estável | abrir os arquivos dourados de todas as versões anteriores | todo PR |
| `unsafe` | Miri | todo PR |
| Dependências | `cargo-deny`, `cargo-audit`, `cargo-vet` | todo PR |
| Desempenho | `criterion` (micro), YCSB A–F, TPC-H reduzido (SF 0,1 e 1), comparando com SQLite e DuckDB e publicando | noturno + antes de release |
| Cluster | testes no estilo Jepsen no simulador (partições, relógio, quedas) | a partir da v0.5 |

**Invariantes do simulador (mínimo):** transação confirmada nunca some depois de queda; não confirmada nunca aparece; `VERIFY` sempre passa num arquivo não adulterado e sempre falha num adulterado; `AS OF t` devolve sempre o mesmo resultado; o backup restaurado é idêntico (mesma raiz).

**Segurança do projeto:**
- `SECURITY.md` com canal privado e prazos;
- modelo de ameaças em `spec/AMEACAS.md`;
- releases assinados (Sigstore/cosign) com build reproduzível;
- revisão externa da parte de criptografia antes da v1.0.

---

## 4. Fases e marcos

Estimativas para **1 pessoa em tempo integral com apoio de agentes**. Cada marco termina com o CI verde e uma demonstração.

### v0.1 — Núcleo confiável (≈ 14–18 semanas)

| Marco | Entrega | Pronto quando |
|---|---|---|
| M0 (1 sem) | Repositório, workspace, CI (fmt, clippy, test, Miri, deny), licenças, `SECURITY.md`, `spec/FORMATO.md` v0 | CI verde no esqueleto |
| M1 (2 sem) ✅ | VFS real e simulado; páginas; cabeçalho duplo; cache SIEVE | arquivo cria, abre e fecha; teste de queda no cabeçalho passa — **feito 22/11** |
| M2 (2 sem) ✅ | Criptografia de página; Argon2id; keyring; chave-mestra embrulhada | arquivo ilegível sem chave; byte trocado é detectado — **feito 22/11** (falta só o keyring do sistema, que entra junto da API pública) |
| M3 (3 sem) ✅ | WAL, transações, escritor único, leitores com foto, gravação em grupo, recuperação | simulador: 1 milhão de passos com quedas sem violar invariante — **feito 22/11**: teste de modelo com 240 execuções por rodada (claro/cifrado × Total/Normal), mais de 100 quedas incluindo checkpoint pela metade; teste de mutação confirma que ele pega recuperação errada. Falta a rodada noturna longa (milhões de passos) |
| M4 (2 sem) ✅ | Camada quente + B+tree primária; catálogo `sys_*` | inserir, ler por chave e varrer 10 milhões de linhas — **feito 23/11** (medido com 1 milhão; ver README): log versionado com histórico e leitura por LSN, árvore B+ com teste de modelo e de mutação, catálogo com `sys_tabelas`/`sys_colunas`, testes com quedas nos 4 modos |
| M5 (3 sem) | SQL v0.1 (parser, planejador simples, execução em lotes) | `sqllogictest` básico igual ao SQLite |
| M6 (2 sem) | Ledger: cadeia, MMR, pontos de controle assinados, `VERIFY`; `AS OF` | adulteração detectada; `AS OF` estável no simulador |
| M7 (1 sem) | Colunas cifradas, índice cego, mascaramento, chave por titular | exclusão de titular torna os dados ilegíveis, inclusive no histórico |
| M8 (1 sem) | CLI `sioux`, `siouxdb-capi`, docs, benchmark inicial, release 0.1.0 assinado | instalação em 1 comando; README com números honestos |

### v0.2 — Servidor, Studio e Backup (≈ 14–18 semanas)
- **sioux-servidor:** seções 2.11 e 2.12 — protocolo do PostgreSQL, TLS 1.3, SCRAM, regras de acesso, gerência de conexões, `sys_conexoes`, desligamento gracioso, `/metricas`. Pronto quando: `psql` e `sqlx` conectam; teste de carga com 5.000 conexões ociosas + 200 ativas sem estourar memória; teste de cliente que some no meio da transação libera o escritor no tempo configurado.
- **siouxdb-metricas + painel do Studio:** seção 2.13. **Já feito em 22/11:** crate de métricas e o painel do Studio sobre arquivo local e demonstração; falta a fonte "servidor" (`/painel/fluxo`), conexões reais, consultas lentas e backup.
- **sioux-backup:**
  - cópia única (`BACKUP PARA`), incremental com deduplicação por hash e contínua com envio do WAL;
  - restauração até um instante, sempre num arquivo novo, verificado e só então trocado;
  - manifesto assinado gravado por último e gravação atômica no destino;
  - destinos local, SFTP e S3 (crate `object_store`);
  - retenção por período, varredura periódica e teste de restauração automático.
- **sioux-studio (Tauri):**
  - conexões (SiouxDB, PostgreSQL, MySQL, Firebird, SQLite), com senhas no keyring do sistema;
  - árvore de esquema, editor com autocompletar (CodeMirror), grade editável, exportar CSV/XLSX;
  - histórico e consultas salvas;
  - **somente leitura por padrão**, confirmação para `UPDATE`/`DELETE` e faixa de aviso em conexão marcada como produção;
  - diagrama das tabelas, comparação de esquemas e "de onde veio esse número".
### v0.3 — Migra, Sync e Ponte (≈ 14–18 semanas)
- **sioux-migra:**
  - leitores de origem: Firebird (`rsfbclient`), MySQL (`mysql_async`), PostgreSQL (`tokio-postgres`), SQL Server (`tiberius`), SQLite, DBF, CSV/planilhas (`calamine`) e XML de NF-e;
  - perfil local da origem (tipos, contagens, qualidade, dado pessoal detectado e mascarado);
  - **IA só propõe o mapeamento** a partir do perfil e do contrato do destino, e a pessoa revisa no Studio;
  - motor determinístico de extração em blocos, retomável e com limite de memória e velocidade;
  - validações (CPF, CNPJ, IE, NCM, EAN, duplicados);
  - saída num arquivo SiouxDB com **certificado de migração** (raiz e manifesto assinado) e delta por hash no dia da virada;
  - receitas reutilizáveis por sistema de origem e destinos plugáveis com contrato.

- **sioux-sync:**
  - captura de mudanças: PostgreSQL por replicação lógica (`pgoutput`), MySQL por binlog em modo de linha, Firebird por gatilhos + tabela de registro (instalados por receita, com remoção limpa);
  - **dono por tabela ou por parte dela**; conflito gera alerta e nunca é sobrescrito em silêncio;
  - atraso medido por tabela, retomada por posição salva e entrega pelo menos uma vez com idempotência.
- **sioux-ponte:**
  - API gerada do esquema e do mapeamento;
  - chave por sistema e permissão por tabela e coluna;
  - limite de requisições e trilha encadeada por hash;
  - **começa somente leitura**; escrita liberada tabela por tabela, só por receitas testadas antes em homologação;
  - roda na rede do cliente e abre um túnel cifrado para fora (sem porta exposta).
- **Painel no Studio:** atraso, conflitos, escritas feitas e partes já viradas, com botão para voltar uma parte.

### v0.4 — Análise e desempenho (≈ 12–16 semanas)
- **Armazenamento:** blocos frios em colunas com resumos e filtros; conversão e compactação em segundo plano com freio.
- **Execução:** vetorizada com SIMD, materialização tardia, morsels, hash join particionado, spill para disco e `WITH ERRO` (respostas pelos resumos e amostragem).
- **Índices:** ART, adaptativo (configurável por coluna) e aprendido (PGM).
- **Configuração:**
  - perfis (`Leve`, `Desktop`, `Servidor`, `Ingestao`, `Analise`, `BaixaLatencia`);
  - configuração por arquivo/código/SQL;
  - **modo automático por metas** (AIMD para concorrência, PID para lote e compactação, CoDel na fila), com limites rígidos, registro do motivo de cada ajuste e `CONFIG CONGELAR`.
- **Mapa de uso:** por tabela e coluna, que gera sugestões em `sys_sugestoes`.
- **Benchmarks públicos** TPC-H e YCSB contra SQLite e DuckDB.

### v0.5+ — Escala e IA
- **sioux-cluster:**
  - Raft com `openraft`;
  - máquinas que votam × que só copiam e testemunhas;
  - **regra de quórum plugável** com a conta `confirmar + eleger > total` conferida (recusa combinação insegura, a não ser com `aceito_perder_confirmadas`);
  - confirmação por transação, grupos por local, leitura do líder ou de réplica com atraso máximo;
  - snapshot = blocos frios;
  - testes no estilo Jepsen no simulador.
- **sioux-ia (opcional):**
  - modelos locais (llama.cpp/`candle`/`mistral.rs`), registrados como objeto com hash travado;
  - texto→SQL que recebe só a estrutura e os resumos, com trava (só leitura, permissão do usuário, plano mostrado);
  - funções de IA no SQL com memória por hash e teto de custo;
  - vetores (`VETOR`, índice HNSW/DiskANN, busca híbrida com BM25);
  - descoberta de dado pessoal e regras em português compiladas para restrição determinística.
- **Bindings:** Python, Node e WASM (VFS em OPFS no navegador).

---

## 5. Riscos e como reduzir

| Risco | Mitigação |
|---|---|
| Motor leva meses para ficar confiável; primeiros usuários com dado importante | Marcar 0.x como "não usar como única cópia"; backup e `VERIFY` desde a v0.1; simulação e fuzzing antes de qualquer release |
| Escopo grande demais (9 peças) | Núcleo primeiro; cada ferramenta só começa depois da v0.1 estável; peças opcionais por feature do Cargo |
| Erro sutil de criptografia | Só bibliotecas auditadas; revisão externa antes da 1.0; testes de vetores conhecidos |
| Formato mudar e quebrar arquivos | `spec/` versionado; arquivos dourados no CI; leitura de versões antigas obrigatória |
| Sync/Ponte gravando errado no sistema antigo | Somente leitura por padrão; escrita só por receita testada em homologação; dono por tabela |
| IA alucinando mapeamento | IA só propõe; motor determinístico executa; revisão humana obrigatória; prévia e validação antes de gravar |
| Desempenho abaixo do prometido | Benchmarks publicados desde a v0.1, incluindo onde perdemos; otimizar guiado por medição |
| Cliente do ERP Sioux com pressa de migrar antes da v0.2 madura | Armazém provisório atrás da mesma interface do `sioux-migra` (destino plugável), trocado depois |

---

## 6. Crates de terceiros (ponto de partida)

| Uso | Crates |
|---|---|
| SQL | `sqlparser`, `sqllogictest` |
| Criptografia | `chacha20poly1305`, `aes-gcm`, `argon2`, `hmac`, `sha2`, `blake3`, `ed25519-dalek`, `zeroize`, `secrecy`, `keyring` |
| Concorrência | `crossbeam-epoch`, `parking_lot`, `rayon` |
| Compressão e formatos | `zstd`, `bitpacking`, `xorf` (filtros), `roaring` |
| Memória | `bumpalo`, `mimalloc` |
| Disco | `io-uring` (opcional), `object_store` (S3) |
| Testes | `proptest`, `criterion`, `cargo-fuzz`, Miri |
| Bancos de origem | `tokio-postgres`, `mysql_async`, `rsfbclient`, `tiberius`, `rusqlite`, `calamine`, `quick-xml` |
| Cluster | `openraft` |
| IA | `candle`, `llama-cpp-2` ou `mistral.rs`, `hnsw_rs` |
| Interfaces | `cbindgen`, `pyo3` + `maturin`, `napi-rs`, `wasm-bindgen`, `tauri` |

Conferir licença e manutenção de cada uma no M0 (`cargo-deny`).

---

## 7. Primeira semana (M0), passo a passo
1. Confirmar D1–D6 com o dono.
2. Criar o repositório público com Cargo workspace, `rust-toolchain.toml`, licenças, `README` curto (link da página), `SECURITY.md` e `CONTRIBUTING.md`.
3. CI no GitHub Actions: `fmt`, `clippy -D warnings`, `test`, Miri (crates com `unsafe`), `cargo-deny`, matriz Linux/macOS/Windows.
4. Escrever `spec/FORMATO.md` v0 (cabeçalho, página, WAL) e `spec/AMEACAS.md` v0.
5. Criar `siouxdb-vfs` com o trait, a versão real e a simulada, mais o primeiro teste de queda.
6. Abrir as *issues* de M1–M8 com os critérios de "pronto" da tabela acima.

---

## 8. Glossário rápido
- **WAL:** registro em que cada mudança é anotada antes de ir para o arquivo principal, para sobreviver a queda de energia.
- **MVCC / foto:** cada leitor enxerga o banco como estava num instante, sem esperar quem grava.
- **Camada quente / fria:** linhas recém-gravadas num log rápido / blocos antigos reorganizados em colunas comprimidas.
- **Merkle Mountain Range:** árvore de hashes para dados que só crescem; prova que um item está no banco e que nada antigo mudou.
- **KEK / envelope:** chave que tranca outras chaves; trocar a de cima não exige recifrar os dados.
- **Índice cego:** HMAC do valor, para buscar por igualdade sem decifrar.
- **Quórum:** quantas máquinas precisam concordar para confirmar uma gravação ou eleger um líder.
