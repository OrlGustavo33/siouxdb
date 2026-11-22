# Política de segurança

O SiouxDB existe para guardar dados sensíveis. Falhas de segurança têm prioridade máxima.

## Como reportar

**Não abra issue pública.** Use o reporte privado do GitHub:
aba **Security** → **Report a vulnerability** neste repositório.

Inclua, se puder: versão ou commit, como reproduzir, o impacto esperado e uma sugestão de correção.

## O que acontece depois

| Prazo | Ação |
|---|---|
| até 3 dias úteis | confirmamos o recebimento |
| até 14 dias | avaliação e plano de correção |
| até 90 dias | correção publicada e aviso de segurança (CVE quando couber), com crédito a quem reportou, se quiser |

## Escopo

Tudo neste repositório: formato do arquivo, criptografia e gerência de chaves, verificação (ledger), parser SQL, ferramentas (backup, migra, sync, ponte, studio, cluster).

## Estado atual

O projeto está em **pré-alfa**: nenhuma versão é considerada segura para dados reais ainda. Mesmo assim, reportes são muito bem-vindos.

## Princípios

- Criptografia só com bibliotecas auditadas; nenhuma cifra própria.
- `unsafe` proibido por padrão; onde for necessário, documentado e testado com Miri.
- Fuzzing contínuo do parser, do formato do arquivo e da verificação.
- Revisão externa da parte de criptografia antes da versão 1.0.
