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

## Página com cifra (planejado para o M2)
- Tamanho padrão **8 KiB**, escolhido na criação (4, 8, 16 ou 32 KiB) e fixo depois.
- Toda leitura e gravação é alinhada ao tamanho da página.
- Layout lógico de uma página gravada:

| Campo | Tamanho | Observação |
|---|---|---|
| nonce | 24 bytes | aleatório a cada gravação (XChaCha20-Poly1305) |
| conteúdo cifrado | página − 24 − 16 | inclui o cabeçalho da página |
| tag | 16 bytes | autenticação |

- **Dados associados (AAD)** da cifra: número da página e versão do formato. Uma página copiada para outra posição falha na autenticação.
- Cabeçalho da página (dentro da parte cifrada): tipo, número, LSN da última alteração, CRC32C do conteúdo.

## Cabeçalho do arquivo
Duas cópias (A na página 0, B na página 1), gravadas alternadamente; vale a de maior **geração** com CRC válido. Contém:
- assinatura mágica `SIOUXDB\0` e versão do formato (em claro, para identificar o arquivo);
- tamanho da página;
- sal e parâmetros do Argon2id;
- chave do arquivo embrulhada pela chave do usuário (envelope);
- raiz do catálogo, raiz do ledger, último LSN confirmado.

## Tipos de página (planejados)
catálogo · log da camada quente · nó de B+tree · bloco frio em colunas · resumo de bloco · lista de páginas livres · ledger.

## A definir antes da v0.1
- Formato exato dos registros do WAL e do encadeamento de hashes.
- Codificação das colunas da camada fria.
- Arquivos de exemplo em `spec/exemplos/`, abertos por todo CI.
