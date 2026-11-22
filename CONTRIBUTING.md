# Como contribuir

Obrigado pelo interesse! O SiouxDB está no começo, e toda ajuda conta.

## Antes de começar
- Leia o [plano de implementação](docs/PLANO_IMPLEMENTACAO.md) e as [especificações](spec/).
- Para mudanças grandes, abra uma *issue* antes e combine a abordagem.
- Falhas de segurança seguem o [`SECURITY.md`](SECURITY.md), nunca issue pública.

## Regras do projeto
1. **Correção antes de velocidade.** Toda mudança no caminho de gravação precisa de teste de queda no disco simulado (`siouxdb-vfs`).
2. **Criptografia só com bibliotecas auditadas.** Nada de cifra própria.
3. **`unsafe` proibido** (o workspace usa `forbid`). Exceções precisam de justificativa e medição.
4. **O formato do arquivo é contrato público.** Mudou o formato? Atualize `spec/FORMATO.md` e a versão do formato.
5. **Desempenho se prova com benchmark**, inclusive quando perde.

## Fluxo
```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```
O CI roda o mesmo em Linux, macOS e Windows. Commits pequenos, com mensagem clara do que muda e por quê.

## Idioma
Código, documentação e discussões em **português do Brasil**, com resumo em inglês no README. Issues e PRs em inglês também são bem-vindos.

## Licença das contribuições
Ao contribuir, você concorda que sua contribuição seja licenciada em MIT OU Apache-2.0, como o resto do projeto.
