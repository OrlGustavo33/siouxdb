# Formato do arquivo — rascunho v0

> **Instável.** Enquanto `VERSAO_FORMATO = 0`, o formato pode mudar sem compatibilidade. A partir da 1.0, toda versão 1.x abre arquivos 1.0.

## Arquivos
- `banco.sdb` — arquivo principal.
- `banco.sdb-wal` — registro de gravações (WAL), cifrado igual ao principal.

## Página
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
