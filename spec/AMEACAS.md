# Modelo de ameaças — rascunho v0

## O que protegemos
1. **Confidencialidade** do conteúdo do arquivo, do WAL e dos backups.
2. **Integridade:** qualquer alteração fora do motor é detectada.
3. **Histórico:** o passado não pode ser reescrito sem detecção.
4. **Separação de acesso:** um usuário não lê colunas para as quais não recebeu chave.

## Quem consideramos atacante
| Atacante | Pode | Proteção |
|---|---|---|
| Quem copia o arquivo (furto de disco, backup vazado) | ler os bytes | arquivo cifrado por página; sem a chave é ruído |
| Quem edita o arquivo | trocar bytes ou páginas, reverter para uma versão antiga | tag por página, AAD com número da página, cadeia de hashes, pontos de controle assinados e âncora externa |
| Usuário legítimo com acesso parcial | usar outro programa para ler o arquivo | permissão criptográfica: sem a chave embrulhada da coluna, não decifra |
| Processo malicioso na mesma máquina, sem a chave | ler a memória ou o disco | chaves zeradas após uso, `mlock` opcional; fora do escopo se ele tiver privilégio de root/admin |
| Dono do servidor que quer adulterar o passado | reescrever e reassinar | âncora diária externa (RFC 3161 / OpenTimestamps) e testemunhas |

## Estado da implementação (22/11/2026)
- ✅ Arquivo cifrado por página (XChaCha20-Poly1305), AAD com número da página e versão; senha por Argon2id; chave do arquivo em envelope; cabeçalho autenticado; chaves apagadas da memória (`zeroize`); mensagens de erro não dizem se foi a senha ou adulteração.
- ⏳ Reversão de página para versão antiga da mesma posição: detectada só com a cadeia de hashes (M6).
- ✅ Trava de arquivo: dois processos (ou duas aberturas) não usam o mesmo banco ao mesmo tempo.
- ✅ WAL: confirmação com autenticação no banco cifrado; WAL alterado sem a chave não abre; WAL de outro banco é ignorado.
- ⏳ `mlock` das chaves, chave por coluna e por titular: M7.

## Fora do escopo
- Atacante com a chave do usuário (ele **é** o usuário).
- Atacante com controle total da máquina durante o uso (pode ler a memória do processo).
- Ataques de canal lateral físico.

## Decisões que decorrem disso
- Nunca guardar chave em claro no arquivo.
- Senha sempre passa por Argon2id.
- Toda decifração autenticada (AEAD); nenhum modo sem autenticação.
