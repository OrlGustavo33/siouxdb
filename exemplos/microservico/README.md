# Microserviço de exemplo

API HTTP/JSON sobre um banco **SiouxDB embutido** (um arquivo, cifrado com senha). Serve para testar o SiouxDB num serviço de verdade enquanto o SQL (M5) e o modo servidor não chegam.

> Pré-alfa: não guarde dados que você não pode perder.

## Compilar

```bash
cd exemplos/microservico
cargo build --release          # binário em target/release/sioux-microservico (~3 MiB)
```

## Rodar

```bash
SIOUX_TOKEN='um-token-longo-e-aleatorio' \
SIOUX_SENHA='senha-do-banco' \
SIOUX_ARQUIVO=/var/lib/sioux-teste/dados.sdb \
SIOUX_ENDERECO=127.0.0.1:7878 \
./sioux-microservico
```

| Variável | Para quê |
|---|---|
| `SIOUX_TOKEN` | obrigatório (≥ 16 caracteres): `Authorization: Bearer <token>` em toda rota, menos `/saude` |
| `SIOUX_SENHA` | senha do banco; sem ela o arquivo fica **em claro** (aparece um aviso) |
| `SIOUX_ARQUIVO` | arquivo do banco (padrão `dados.sdb`; criado se não existir; o WAL fica ao lado, `-wal`) |
| `SIOUX_BACKUP_DIR` | pasta das cópias feitas por `POST /backup` (sem ela, a rota recusa) |
| `SIOUX_ENDERECO` | padrão `127.0.0.1:7878` — exponha por um proxy com HTTPS (nginx/Caddy), não direto |

Só **um processo** pode abrir o arquivo (o segundo recebe "em uso"). Ao receber SIGTERM/Ctrl+C ele termina as requisições, faz o checkpoint e sai.

## SQL

```bash
curl -H "$T" -H 'content-type: application/json' -X POST $U/sql -d '{
  "sql": "SELECT cidade, COUNT(*) AS n FROM clientes WHERE saldo > ? GROUP BY cidade ORDER BY n DESC",
  "params": [10]
}'
# {"lsn": 7, "resultados": [{"colunas": ["cidade","n"], "linhas": [["SP",2]], "afetadas": 0}]}
```

Vários comandos separados por `;` rodam numa transação só (tudo ou nada). Um SELECT sozinho roda numa foto do banco, sem esperar escritores.

## Rotas REST

```bash
T='Authorization: Bearer um-token-longo-e-aleatorio'; U=http://127.0.0.1:7878

curl $U/saude
curl -H "$T" -X POST $U/tabelas -H 'content-type: application/json' -d '{
  "nome": "clientes",
  "colunas": [{"nome":"id","tipo":"inteiro"}, {"nome":"nome","tipo":"texto","obrigatoria":true}, {"nome":"saldo","tipo":"real"}],
  "chave": ["id"]
}'
curl -H "$T" -X POST $U/tabelas/clientes/linhas -d '{"id":1,"nome":"ANA","saldo":10.5}' -H 'content-type: application/json'   # insere (409 se existe)
curl -H "$T" -X PUT  $U/tabelas/clientes/linhas -d '{"id":1,"nome":"ANA MARIA"}' -H 'content-type: application/json'          # cria ou substitui
curl -H "$T" "$U/tabelas/clientes/linhas?inicio=1&fim=100&limite=50"     # faixa pela chave
curl -H "$T" $U/tabelas/clientes/linhas/1                                 # pela chave
curl -H "$T" "$U/tabelas/clientes/linhas/1?lsn=2"                         # como estava na confirmação 2
curl -H "$T" $U/tabelas/clientes/linhas/1/historico                       # todas as versões
curl -H "$T" -X DELETE $U/tabelas/clientes/linhas/1
curl -H "$T" "$U/tabelas/clientes/linhas?onde=nome:ANA"                  # filtro por coluna (igual)
curl -H "$T" -X POST $U/tabelas/clientes/compactar                        # limpa o histórico (fica a versão atual)
curl -H "$T" -X POST "$U/tabelas/clientes/compactar?manter_lsn=500"       # mantém o histórico a partir do LSN 500
curl -H "$T" -X POST $U/backup                                            # cópia a quente em SIOUX_BACKUP_DIR
curl -H "$T" $U/tabelas                                                   # tabelas + contagem
curl -H "$T" -X DELETE $U/tabelas/clientes
```

Tipos: `inteiro`, `real`, `texto`, `bytes` (em base16), `booleano`. Linha de até 16 MiB (o corpo da requisição é limitado a 1 MiB).

**Cuidados:** toda alteração guarda uma versão nova — rode `compactar` de vez em quando, senão o arquivo só cresce. Faça `POST /backup` periódico (um cron com curl serve) e guarde as cópias em outro disco; a cópia abre com a mesma senha (`sioux verificar copia.sdb`). Chave composta na URL: valores separados por `|` (`/linhas/SP|SANTOS`). Toda escrita responde com o `lsn` da confirmação.

## Serviço systemd (exemplo)

```ini
[Unit]
Description=SiouxDB - microserviço de teste
After=network.target

[Service]
ExecStart=/opt/sioux-teste/sioux-microservico
EnvironmentFile=/etc/sioux-teste.env
WorkingDirectory=/var/lib/sioux-teste
User=sioux
Restart=on-failure
KillSignal=SIGTERM
TimeoutStopSec=30

[Install]
WantedBy=multi-user.target
```
