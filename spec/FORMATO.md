# Formato do arquivo — rascunho v0

> **Instável.** Enquanto `VERSAO_FORMATO = 0`, o formato pode mudar sem compatibilidade. A partir da 1.0, toda versão 1.x abre arquivos 1.0.

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
| 8..12 | versão do formato (hoje 0) |
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
- **Reversão de página**: trocar uma página pela versão antiga **da mesma posição** (de uma cópia antiga do arquivo) ainda passa. A cadeia de hashes do livro-razão (M6) fecha isso.
- O cabeçalho revela, em claro, tamanho de página, número de páginas e raízes (não o conteúdo).

## WAL e transações (implementado no M3, `siouxdb-transacoes`)

O banco são dois arquivos: `banco.sdb` (principal) e `banco.sdb-wal` (registro de gravações). O arquivo principal fica **travado com exclusividade** enquanto o banco está aberto (`flock` / `LockFile`): outro processo recebe "em uso".

### Cabeçalho do WAL (64 bytes)
| Bytes | Campo |
|---|---|
| 0..8 | `SIOUXWAL` |
| 8..12 | versão do WAL (0) |
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

## Tipos de página (planejados)
catálogo · log da camada quente · nó de B+tree · bloco frio em colunas · resumo de bloco · lista de páginas livres · ledger.

## A definir antes da v0.1
- Encadeamento de hashes do livro-razão (M6).
- Codificação das colunas da camada fria.
- Arquivos de exemplo em `spec/exemplos/`, abertos por todo CI.
