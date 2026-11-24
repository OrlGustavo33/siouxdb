# SiouxDB e SQLite: o que é igual e o que é diferente

O SQL do SiouxDB é comparado com o do SQLite a cada mudança: o teste
`crates/siouxdb-sql/src/testes_sqlite.rs` gera milhares de consultas aleatórias
(determinísticas pela semente), roda nos dois bancos com os mesmos dados e exige
resultados iguais.

Cobertura: `WHERE` com `AND`/`OR`/`NOT`, comparações, `BETWEEN`, `IN` (inclusive
com `NULL`), `IS NULL`, `LIKE`; aritmética de inteiros e reais (`+ - * / %`, inteiro
com real); `ORDER BY` (expressão, posição, apelido, `DESC`), `LIMIT`/`OFFSET`;
`GROUP BY` e `HAVING` com `COUNT`/`SUM`/`AVG`/`MIN`/`MAX`; `JOIN` e `LEFT JOIN`;
as funções `ABS`, `COALESCE`, `IFNULL`, `NULLIF`, `LENGTH`, `UPPER`, `LOWER`,
`SUBSTR`, `TRIM`/`LTRIM`/`RTRIM`, `REPLACE`, `INSTR`, `ROUND` e `||`; consultas que
o planejador resolve pela chave, por índice ou por faixa.

Com `SIOUX_SIM_FATOR=10`: 30 mil consultas, 73% delas com linhas no resultado, todas
iguais.

## Diferenças propositais

O SiouxDB é mais rígido. Onde o SQLite "dá um jeito", o SiouxDB dá erro, porque num
sistema de gestão um valor silenciosamente errado é pior que um erro.

| Situação | SQLite | SiouxDB |
|---|---|---|
| Divisão ou resto por zero (`1 / 0`) | `NULL` | erro "divisão por zero" |
| Texto em conta (`'ab' + 1`) | converte (`1`) | erro |
| Comparar tipos diferentes (`a = '3'`, `1 < 'x'`) | regra de afinidade | erro |
| Gravar texto em coluna inteira | aceita | erro |
| Estouro de inteiro (`9223372036854775807 + 1`) | vira real | erro |
| `LIMIT -1` | sem limite | erro |
| `LIKE` | ignora maiúsculas (A–Z) | diferencia maiúsculas |
| `UPPER`/`LOWER` | só A–Z | Unicode inteiro (`ç` → `Ç`) |
| Booleano | inteiro 0/1 | tipo próprio (`VERDADEIRO`/`FALSO`) |
| `ROUND` em `DECIMAL` | não tem `DECIMAL` | exato, meio para longe do zero |

## O que ainda não existe no SiouxDB

Subconsultas, `UNION`, `DISTINCT`, `CASE`, `RIGHT/FULL JOIN`, funções de janela,
`CAST`, gatilhos e visões.
