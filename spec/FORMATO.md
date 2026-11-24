# Formato do arquivo — versão 1

> **Congelado em 23/11/2026.** O que está descrito aqui é a versão 1 do formato (arquivo principal e WAL). Mudanças futuras só acrescentam (campos reservados, tipos de página e de valor novos) ou sobem o número da versão — e o programa continua abrindo a versão 1. Os arquivos de `spec/exemplos/` foram gravados por esta versão e são abertos e conferidos em todo CI (`crates/siouxdb/tests/formato.rs`).

## Arquivos
- `banco.sdb` — arquivo principal.
- `banco.sdb-wal` — registro de gravações (WAL), cifrado igual ao principal.

## O que já está implementado (M1, `siouxdb-armazenamento`)

Números em **little-endian**. Página `n` começa no byte `n × tamanho_da_página`.

### Cabeçalho do arquivo (512 bytes, duas cópias)
Cópia A no byte 0 (página 0), cópia B no byte `tamanho_da_página` (página 1). A geração `g` é gravada na cópia `g % 2`, ou seja, sempre por cima da mais antiga. Ao abrir, vale a cópia íntegra de **maior geração**; se a A estiver ilegível, a B é procurada nas quatro posições possíveis.

| Bytes | Campo |
|---|---|
| 0..8 | assinatura `SIOUXDB\0` |
| 8..12 | versão do formato (1) |
| 12..16 | tamanho da página em bytes |
| 16..24 | geração |
| 24..32 | total de páginas (inclui as 2 reservadas) |
| 32..40 | primeira página livre (0 = nenhuma) |
| 40..48 | raiz do catálogo (0 = nenhuma) |
| 48..56 | raiz do livro-razão (0 = nenhuma) |
| 56..64 | último LSN confirmado |
| 64..508 | reservado, zerado (parâmetros de cifra entram no M2) |
| 508..512 | CRC32C dos bytes 0..508 |

### Página (em claro, até o M2)
| Bytes | Campo |
|---|---|
| 0..4 | CRC32C dos bytes 4..fim |
| 4 | tipo: 0 livre, 1 catálogo, 2 log quente, 3 nó de árvore, 4 bloco frio, 5 resumo, 6 razão |
| 5..8 | reservado |
| 8..16 | número da página (confere com a posição: detecta página fora do lugar) |
| 16..24 | LSN da última alteração |
| 24.. | conteúdo |

Página **livre**: os 8 primeiros bytes do conteúdo apontam para a próxima livre (0 = fim). Páginas liberadas só entram na lista **depois** da confirmação seguinte. Se uma queda deixar a lista inconsistente, ela é cortada no ponto do problema (perde espaço, nunca entrega página em uso).

### Confirmação
1. `fsync` das páginas; 2. cabeçalho da geração seguinte na cópia mais antiga; 3. `fsync`. Se um `fsync` falhar, o paginador recusa novas operações até o arquivo ser reaberto.

## Arquivo cifrado (implementado no M2, `siouxdb-cripto`)

Cifrado é o padrão da CLI (`sioux criar`); `--sem-cifra` cria em claro.

### Campos extras do cabeçalho
| Bytes | Campo |
|---|---|
| 64 | cifra: 0 nenhuma, 1 XChaCha20-Poly1305 |
| 65..68 | reservado |
| 68..72 | Argon2id: memória (KiB) |
| 72..76 | Argon2id: iterações |
| 76..80 | Argon2id: paralelismo |
| 80..96 | sal (16 bytes) |
| 96..168 | chave do arquivo embrulhada: nonce 24 + chave cifrada 32 + etiqueta 16 |
| 168..184 | identificador do arquivo (sorteado na criação; o WAL guarda o mesmo) |
| 184..224 | **autenticação do cabeçalho**: nonce 24 + etiqueta 16 sobre os bytes 0..184, com a chave do arquivo |

- A chave do arquivo (256 bits) é sorteada na criação. A chave da senha vem do **Argon2id** (padrão 64 MiB, 3 iterações, 1 linha) e embrulha a do arquivo com XChaCha20-Poly1305.
- Os **dados associados** do embrulho são assinatura, versão, tamanho da página, tipo de cifra, parâmetros do Argon2id e sal: alterar qualquer um deles impede desembrulhar.
- O cabeçalho fica legível (é preciso para derivar a chave), mas a **cópia em vigor precisa autenticar**; se não autenticar, o arquivo não abre (não se volta para a cópia anterior por isso).
- **Trocar a senha** reembrulha a chave e grava as **duas** cópias; a senha antiga deixa de abrir.

### Página cifrada
| Bytes | Campo |
|---|---|
| 0..24 | nonce, sorteado a cada gravação |
| 24..n−16 | página lógica cifrada (o layout em claro descrito acima, com n−40 bytes) |
| n−16..n | etiqueta Poly1305 |

Dados associados: `"SIOUXPAG"` + número da página (u64) + versão do formato (u32). Página copiada para outra posição, alterada, cortada por queda ou de outro arquivo **não decifra** e é tratada como corrompida.

### Limites conhecidos (resolvidos em marcos seguintes)
- **Reversão de página**: trocar uma página pela versão antiga **da mesma posição** (de uma cópia antiga do arquivo) ainda decifra. Quem pega é o `VERIFY` do livro-razão (seção abaixo); a leitura comum não confere o razão.
- O cabeçalho revela, em claro, tamanho de página, número de páginas e raízes (não o conteúdo).

## WAL e transações (implementado no M3, `siouxdb-transacoes`)

O banco são dois arquivos: `banco.sdb` (principal) e `banco.sdb-wal` (registro de gravações). O arquivo principal fica **travado com exclusividade** enquanto o banco está aberto (`flock` / `LockFile`): outro processo recebe "em uso".

### Cabeçalho do WAL (64 bytes)
| Bytes | Campo |
|---|---|
| 0..8 | `SIOUXWAL` |
| 8..12 | versão do WAL (1) |
| 12..16 | tamanho da página |
| 16..32 | identificador do arquivo principal (bytes 168..184 do cabeçalho dele) — WAL de outro banco é ignorado |
| 32..40 | sal (sorteado a cada reinício do WAL) |
| 40..48 | sequência (conta os reinícios) |
| 60..64 | CRC32C dos bytes 0..60 |

### Quadros
Cada quadro tem 40 bytes de cabeçalho — tipo (u8), reservado (7), número da página (u64), LSN (u64), sal (u64), **CRC encadeado** (u32), reservado (4) — e o conteúdo:
- **tipo 1, página:** a página no formato do disco (cifrada se o banco for);
- **tipo 2, confirmação (88 bytes):** meta (total de páginas, primeira livre, raiz do catálogo, raiz do razão, LSN; 5 × u64), quantidade de páginas da transação (u32), reservado (4), autenticação (40 bytes, só em banco cifrado: nonce + etiqueta sobre os campos do quadro, a meta, a quantidade e o CRC encadeado até o quadro anterior).

O CRC de cada quadro **continua** o do anterior (`crc32c(anterior ++ campos ++ conteúdo)`).

**Transação grande:** quando passa de `paginas_sujas_max` páginas na memória, a transação grava quadros de página no WAL antes da confirmação (mesmo LSN, CRC encadeado). A mesma página pode aparecer mais de uma vez na transação: **vale o último quadro**. A quantidade no quadro de confirmação conta todos os quadros da transação. Sem a confirmação, a recuperação descarta tudo; se a transação é desfeita, o fim do WAL não anda e a próxima grava por cima (os quadros que sobrarem depois quebram o encadeamento do CRC e são ignorados).

### Recuperação
1. Lê o cabeçalho do WAL; se não for deste banco (identificador, tamanho de página) ou estiver ilegível, o WAL recomeça vazio.
2. Lê os quadros em ordem; **para** no primeiro com sal diferente, CRC que não confere ou que não cabe no arquivo (gravação cortada por queda).
3. Vale tudo até a **última confirmação íntegra**. Uma confirmação com CRC certo e autenticação errada (ou quantidade/LSN incoerentes) não é queda: é alteração, e o banco não abre (`WalAdulterado`).
4. Quadros de confirmações com LSN ≤ o do arquivo principal já foram copiados num checkpoint e são ignorados.

### Transação e confirmação
- Uma transação junta as páginas alteradas em memória; ao confirmar, grava os quadros das páginas e o de confirmação **numa gravação só** no fim do WAL. Sem a confirmação, nada vale: tudo ou nada, inclusive sobrescrita de página existente.
- **Sincronia Total:** a confirmação só retorna depois do `fsync` do WAL. O escritor solta a vez antes do `fsync`, então confirmações próximas dividem o mesmo `fsync` (confirmação em grupo).
- **Sincronia Normal:** sem `fsync` por confirmação; um `fsync` a cada 256 páginas (fora da vez) e no checkpoint. Numa queda podem sumir as últimas confirmações, nunca uma parte de uma.

### Leituras e checkpoint
- Uma leitura fixa o LSN visível (o durável, na sincronia Total) e lê cada página na última versão ≤ esse LSN: do WAL, se houver, senão do arquivo principal.
- O **checkpoint** copia para o arquivo principal a última versão de cada página até `min(LSN durável, foto mais antiga aberta)`, grava o cabeçalho (meta) e, se tudo foi copiado e nenhuma foto aberta é anterior ao checkpoint, reinicia o WAL (sal novo, sequência + 1). Roda sozinho a cada `checkpoint_apos_paginas` páginas, na vez de quem confirmou.
- Uma queda no meio do checkpoint é segura: as páginas já copiadas também estão no WAL, que só é reiniciado depois de o cabeçalho principal estar em disco.

## Tabelas (implementado no M4, `siouxdb-indices` + `siouxdb-catalogo`)

Tipos de página novos: **3 nó de árvore**, **2 log quente**, **7 cabeçalho de tabela**.

### Nó da árvore B+ (dentro do conteúdo da página)
`0` nível (0 = folha) · `2..4` quantidade de células · `4..6` bytes das células · `6..8` bytes mortos · `8..16` filho da direita (nó interno) · `16..` vetor de posições (u16) · células gravadas do fim da página para o começo.
- célula de folha: tamanho da chave u16 · tamanho do valor u16 · chave · valor;
- célula de nó interno: tamanho da chave u16 · filho u64 · chave (o filho tem as chaves menores que ela).

A raiz nunca muda de página. Chaves são comparadas como bytes; a separadora que sobe é o menor prefixo que separa as duas folhas.

### Chaves e linhas
- **Chave** (ordenável como bytes): por coluna, um byte de tipo (0 nulo, 1 booleano, 2 inteiro, 3 real, 4 texto, 5 bytes) e o valor — inteiro em big-endian com o bit de sinal invertido; real com o truque de bits do IEEE 754; texto e bytes com `00` → `00 FF` e terminador `00 00`.
- **Linha**: quantidade u16 e, por valor, um byte de tipo (0 nulo, 1 inteiro, 2 real, 3 texto, 4 bytes, 5 booleano, 6 decimal, 7 data/hora) e o valor em little-endian (texto/bytes com tamanho u32). Uma linha pode ter **menos** valores que a tabela tem colunas (foi gravada antes de um `ALTER TABLE ADD COLUMN`): as que faltam valem o padrão da coluna.
- **Decimal**: inteiro de 128 bits = valor × 10¹⁰ (ponto fixo, 10 casas). Na chave: tipo 6 + 16 bytes big-endian com o bit de sinal invertido.
- **Data/hora**: inteiro de 64 bits, microssegundos desde 1970-01-01 UTC. Na chave: tipo 7 + 8 bytes big-endian com o bit de sinal invertido.

### Log quente (livro-razão das versões)
Página: `0..8` próxima página · `8..10` bytes usados · registros a partir do byte 16. Registro: tamanho u16 · marcas (bit 0 = remoção) · tamanho da chave u16 · versão anterior da mesma chave (ponteiro = página << 16 | posição) · LSN · instante (µs) · chave · linha. Nada é sobrescrito: cada gravação anexa uma versão.

### Valores grandes (transbordo, página tipo 8)
Quando chave + linha passam de metade do espaço de um registro, a linha vai para uma corrente de páginas de transbordo — `0..8` próxima página, `8..12` bytes nesta página, depois os bytes — e o registro guarda só `tamanho total (u64) · primeira página (u64)`, com o bit 1 das marcas ligado. Linha até 16 MiB; chave até ~1/4 da página.

### Compactação
Reescreve o log de uma tabela guardando, por chave, as versões posteriores ao LSN de corte e a que valia nele; chaves removidas até o corte saem do índice. O log antigo e os transbordos voltam para a lista de livres.

### Índices secundários
Árvore B+ por índice; chave = valores das colunas indexadas (codificação de chave) + chave primária codificada; valor = chave primária codificada. Guarda só o estado atual (não é versionado): consultas `AS OF` usam a tabela. A lista de índices (nome, único, raiz, colunas) fica na definição da tabela no catálogo.

### Tabela e catálogo
- Cabeçalho de tabela: raiz do índice, primeira e última página do log, versões, linhas, último número da numeração automática (6 × u64).
- O índice da chave primária leva à versão mais nova; remoção também é versão (o histórico continua alcançável).
- Catálogo: árvore B+ na `raiz_catalogo` do cabeçalho do banco — `00 "proximo_id"` → contador; `01 + nome` → definição, codificada como uma linha: id, nome, cabeçalho, nº de colunas, (nome, tipo, não-nulo) por coluna, nº de colunas da chave e suas posições, nº de índices e (nome, único, raiz, nº de colunas, posições) por índice, coluna automática (−1 = nenhuma) e, por coluna, o padrão (0 nenhum; 1 + valor; 2 = agora). `sys_tabelas` e `sys_colunas` são visões calculadas.

## Livro-razão (página tipo 6, `siouxdb-transacoes::razao`)

Cada confirmação acrescenta uma entrada, **no mesmo quadro do WAL** das páginas dela (a entrada existe se e só se a confirmação existe). Hashes: BLAKE3 em modo `derive_key`, com contextos `"SiouxDB razao pagina v1"`, `"SiouxDB razao estado v1"` e `"SiouxDB razao cadeia v1"`.

- **Soma das páginas (LtHash 1024 × u16):** cada página de número `n` em `2..total_paginas`, exceto as do tipo 6, contribui com `XOF(pagina; n u64 ‖ tipo u8 ‖ conteúdo)` de 2048 bytes, lidos como 1024 u16 little-endian; a soma é faixa a faixa, módulo 2¹⁶. Na confirmação só as páginas alteradas mudam a soma (sai a versão de antes, entra a nova). O LSN e o CRC do cabeçalho da página não entram.
- **Estado:** `BLAKE3(estado; soma (2048 bytes) ‖ meta de 40 bytes, com ultimo_lsn = LSN da confirmação)`.
- **Elo:** `BLAKE3(cadeia; elo anterior (32, zeros na primeira) ‖ lsn u64 ‖ instante u64 ‖ estado)`. Instante em µs desde 1970, UTC.
- **Entrada (80 bytes):** lsn u64 · instante u64 · estado (32) · elo (32).
- **Cabeça** (página em `raiz_razao`; conteúdo): `0..4` `"SXRZ"` · `4..8` entradas nesta página (u32) · `8..16` última página arquivada (0 = nenhuma) · `16..24` total de entradas · `24..56` último elo · `56..64` reservado · `64..2112` soma · entradas a partir de `2112`.
- **Página arquivada:** quando a cabeça enche, as entradas dela vão para uma página nova: `0..4` `"SXRA"` · `4..8` quantidade · `8..16` arquivada anterior · entradas a partir de `16`. Nunca é reescrita.
- **Criação:** bancos sem razão (`raiz_razao = 0`, inclusive os gravados antes dele) ganham a cabeça na próxima confirmação, que soma todas as páginas uma vez.

**`VERIFY`** recalcula a cadeia do começo (ordem dos LSNs e cada elo), confere o último elo e o total com a cabeça, soma todas as páginas de novo e compara com a soma guardada e com o estado da última entrada, que tem de ser do LSN atual da meta. Com uma **âncora** (`lsn:hash` do elo, guardada fora do banco), confere também que aquela confirmação está na cadeia com o mesmo elo: é o que pega um arquivo que voltou inteiro no tempo (cópia antiga) ou um passado reescrito por quem tem a chave.

## Tipos de página (planejados)
bloco frio em colunas · resumo de bloco (os demais já existem).

## A definir antes da v0.1
- Pontos de controle assinados e MMR no livro-razão (hoje: cadeia + âncora).
- Codificação das colunas da camada fria.
- Arquivos de exemplo em `spec/exemplos/`, abertos por todo CI.
