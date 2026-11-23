//! Microserviço de exemplo: API HTTP/JSON sobre um banco SiouxDB embutido.
//!
//! Variáveis de ambiente:
//! - `SIOUX_TOKEN` (obrigatória): token exigido em `Authorization: Bearer <token>`;
//! - `SIOUX_ARQUIVO` (padrão `dados.sdb`): arquivo do banco (criado se não existir);
//! - `SIOUX_SENHA` (opcional): senha do banco — sem ela o arquivo fica **em claro**;
//! - `SIOUX_ENDERECO` (padrão `127.0.0.1:7878`);
//! - `SIOUX_BACKUP_DIR` (opcional): pasta das cópias de `POST /backup`.
//!
//! Rotas (JSON):
//! - `GET  /saude`
//! - `GET  /tabelas` · `POST /tabelas` `{"nome","colunas":[{"nome","tipo","obrigatoria"}],"chave":["col"]}`
//! - `DELETE /tabelas/{t}`
//! - `GET  /tabelas/{t}/linhas?inicio=&fim=&limite=&lsn=` (faixa pela chave simples)
//! - `POST /tabelas/{t}/linhas` (insere; erro se a chave existe) · `PUT` (grava: cria ou substitui)
//! - `GET  /tabelas/{t}/linhas/{chave}?lsn=` · `DELETE /tabelas/{t}/linhas/{chave}`
//! - `GET  /tabelas/{t}/linhas/{chave}/historico`
//! - `POST /tabelas/{t}/compactar?manter_lsn=` (sem `manter_lsn`: fica só a versão atual)
//! - `POST /backup` (cópia a quente em `SIOUX_BACKUP_DIR`)
//! - `POST /sql` `{"sql": "...", "params": [...]}`: um ou mais comandos numa transação só
//! - filtro na listagem: `?onde=coluna:valor` (compara igual; varre a faixa)
//!
//! Tipos: `inteiro`, `real`, `texto`, `bytes` (base16), `booleano`. Chave composta: use a
//! rota de faixa ou junte os valores com `|` em `{chave}`.

use axum::extract::{Path, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Map, Value as J, json};
use siouxdb::BancoArquivo;
use siouxdb::catalogo::catalogo;
use siouxdb::catalogo::{Coluna, ErroCatalogo, Tabela, Tipo, Valor};
use siouxdb::transacoes::{Config, ErroBanco};
use std::collections::HashMap;
use std::sync::Arc;

struct App {
    banco: BancoArquivo,
    token: String,
    pasta_backup: Option<std::path::PathBuf>,
}

type Estado = State<Arc<App>>;

struct Erro(StatusCode, String);

impl IntoResponse for Erro {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "erro": self.1 }))).into_response()
    }
}

impl From<ErroCatalogo> for Erro {
    fn from(e: ErroCatalogo) -> Self {
        let st = match &e {
            ErroCatalogo::TabelaNaoExiste(_) => StatusCode::NOT_FOUND,
            ErroCatalogo::TabelaJaExiste(_) | ErroCatalogo::ChaveDuplicada => StatusCode::CONFLICT,
            ErroCatalogo::NomeInvalido(_)
            | ErroCatalogo::DefinicaoInvalida(_)
            | ErroCatalogo::LinhaInvalida(_)
            | ErroCatalogo::LinhaGrandeDemais { .. } => StatusCode::BAD_REQUEST,
            ErroCatalogo::Banco(ErroBanco::EsperaEsgotada(_)) => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Erro(st, e.to_string())
    }
}

impl From<ErroBanco> for Erro {
    fn from(e: ErroBanco) -> Self {
        ErroCatalogo::Banco(e).into()
    }
}

fn ruim(m: impl Into<String>) -> Erro {
    Erro(StatusCode::BAD_REQUEST, m.into())
}

type R<T> = Result<T, Erro>;

/// O banco é síncrono (e o `fsync` bloqueia): cada operação roda fora das tarefas assíncronas.
async fn no_banco<T: Send + 'static>(
    app: &Arc<App>,
    f: impl FnOnce(&BancoArquivo) -> R<T> + Send + 'static,
) -> R<T> {
    let app = Arc::clone(app);
    tokio::task::spawn_blocking(move || f(&app.banco))
        .await
        .map_err(|e| Erro(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

fn tipo_de(s: &str) -> R<Tipo> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "inteiro" => Tipo::Inteiro,
        "real" => Tipo::Real,
        "texto" => Tipo::Texto,
        "bytes" => Tipo::Bytes,
        "booleano" => Tipo::Booleano,
        "decimal" => Tipo::Decimal,
        "datahora" => Tipo::DataHora,
        o => return Err(ruim(format!("tipo desconhecido: {o}"))),
    })
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn de_hex(s: &str) -> R<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return Err(ruim("bytes em base16 com tamanho ímpar"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| ruim("base16 inválido")))
        .collect()
}

fn para_json(v: &Valor) -> J {
    match v {
        Valor::Nulo => J::Null,
        Valor::Inteiro(i) => json!(i),
        Valor::Real(r) => json!(r),
        Valor::Texto(t) => json!(t),
        Valor::Bytes(b) => json!(hex(b)),
        Valor::Booleano(b) => json!(b),
        // Decimal vai como texto para não perder casas no JSON.
        Valor::Decimal(d) => json!(d.to_string()),
        Valor::DataHora(t) => json!(siouxdb::catalogo::datahora::para_texto(*t)),
    }
}

fn de_json(v: &J, tipo: Tipo, coluna: &str) -> R<Valor> {
    let erro = || ruim(format!("coluna \"{coluna}\": esperava {}", tipo.nome()));
    Ok(match (v, tipo) {
        (J::Null, _) => Valor::Nulo,
        (J::Number(n), Tipo::Inteiro) => Valor::Inteiro(n.as_i64().ok_or_else(erro)?),
        (J::Number(n), Tipo::Real) => Valor::Real(n.as_f64().ok_or_else(erro)?),
        (J::String(s), Tipo::Texto) => Valor::Texto(s.clone()),
        (J::String(s), Tipo::Bytes) => Valor::Bytes(de_hex(s)?),
        (J::Bool(b), Tipo::Booleano) => Valor::Booleano(*b),
        (J::Number(n), Tipo::Decimal) => {
            Valor::Decimal(siouxdb::catalogo::Decimal::de_texto(&n.to_string()).ok_or_else(erro)?)
        }
        (J::String(t), Tipo::Decimal) => {
            Valor::Decimal(siouxdb::catalogo::Decimal::de_texto(t).ok_or_else(erro)?)
        }
        (J::String(t), Tipo::DataHora) => {
            Valor::DataHora(siouxdb::catalogo::datahora::de_texto(t).ok_or_else(erro)?)
        }
        _ => return Err(erro()),
    })
}

/// Valor de texto (da URL) para o tipo da coluna.
fn de_texto(s: &str, tipo: Tipo, coluna: &str) -> R<Valor> {
    let erro = || {
        ruim(format!(
            "coluna \"{coluna}\": \"{s}\" não é {}",
            tipo.nome()
        ))
    };
    Ok(match tipo {
        Tipo::Inteiro => Valor::Inteiro(s.parse().map_err(|_| erro())?),
        Tipo::Real => Valor::Real(s.parse().map_err(|_| erro())?),
        Tipo::Texto => Valor::Texto(s.to_string()),
        Tipo::Bytes => Valor::Bytes(de_hex(s)?),
        Tipo::Booleano => Valor::Booleano(s.parse().map_err(|_| erro())?),
        Tipo::Decimal => Valor::Decimal(siouxdb::catalogo::Decimal::de_texto(s).ok_or_else(erro)?),
        Tipo::DataHora => {
            Valor::DataHora(siouxdb::catalogo::datahora::de_texto(s).ok_or_else(erro)?)
        }
    })
}

fn linha_json(t: &Tabela, linha: &[Valor]) -> J {
    let mut m = Map::new();
    for (c, v) in t.def().colunas.iter().zip(linha) {
        m.insert(c.nome.clone(), para_json(v));
    }
    J::Object(m)
}

fn linha_de_json(t: &Tabela, corpo: &J) -> R<Vec<Valor>> {
    let obj = corpo
        .as_object()
        .ok_or_else(|| ruim("o corpo precisa ser um objeto JSON"))?;
    for k in obj.keys() {
        if !t.def().colunas.iter().any(|c| &c.nome == k) {
            return Err(ruim(format!("a tabela não tem a coluna \"{k}\"")));
        }
    }
    t.def()
        .colunas
        .iter()
        .map(|c| de_json(obj.get(&c.nome).unwrap_or(&J::Null), c.tipo, &c.nome))
        .collect()
}

/// `{chave}` da URL → valores da chave primária (composta separada por `|`).
fn chave_de_texto(t: &Tabela, s: &str) -> R<Vec<Valor>> {
    let partes: Vec<&str> = s.split('|').collect();
    if partes.len() != t.def().chave.len() {
        return Err(ruim(format!(
            "a chave tem {} parte(s), separe com |",
            t.def().chave.len()
        )));
    }
    partes
        .iter()
        .zip(&t.def().chave)
        .map(|(p, &i)| {
            let c = &t.def().colunas[i];
            de_texto(p, c.tipo, &c.nome)
        })
        .collect()
}

fn def_json(t: &Tabela) -> J {
    let d = t.def();
    json!({
        "nome": d.nome,
        "id": d.id,
        "colunas": d.colunas.iter().map(|c| json!({"nome": c.nome, "tipo": c.tipo.nome().to_lowercase(), "obrigatoria": c.nao_nulo})).collect::<Vec<_>>(),
        "chave": d.chave.iter().map(|&i| d.colunas[i].nome.clone()).collect::<Vec<_>>(),
    })
}

async fn saude(State(app): Estado) -> Json<J> {
    let e = app.banco.estatisticas();
    Json(json!({
        "ok": true,
        "versao": siouxdb::VERSAO,
        "cifrado": app.banco.paginador().cifrado(),
        "lsn": e.lsn_escrito,
        "lsn_checkpoint": e.lsn_checkpoint,
        "paginas_no_wal": e.paginas_no_wal,
        "confirmacoes": e.confirmacoes,
        "fsyncs": e.sincronizacoes,
        "fila_escritor": e.fila_escritor,
    }))
}

async fn listar_tabelas(State(app): Estado) -> R<Json<J>> {
    no_banco(&app, |b| {
        let l = b.ler()?;
        let mut v = Vec::new();
        for t in catalogo::tabelas(&l)? {
            let mut d = def_json(&t);
            d["linhas"] = json!(t.contagem(&l)?.linhas);
            v.push(d);
        }
        Ok(Json(J::Array(v)))
    })
    .await
}

async fn criar_tabela(State(app): Estado, Json(corpo): Json<J>) -> R<(StatusCode, Json<J>)> {
    let nome = corpo["nome"]
        .as_str()
        .ok_or_else(|| ruim("falta \"nome\""))?
        .to_string();
    let mut colunas = Vec::new();
    for c in corpo["colunas"]
        .as_array()
        .ok_or_else(|| ruim("falta \"colunas\""))?
    {
        let n = c["nome"]
            .as_str()
            .ok_or_else(|| ruim("coluna sem \"nome\""))?;
        let t = tipo_de(
            c["tipo"]
                .as_str()
                .ok_or_else(|| ruim("coluna sem \"tipo\""))?,
        )?;
        let obrigatoria = c["obrigatoria"].as_bool().unwrap_or(false);
        colunas.push(if obrigatoria {
            Coluna::obrigatoria(n, t)
        } else {
            Coluna::nova(n, t)
        });
    }
    let chave: Vec<String> = corpo["chave"]
        .as_array()
        .ok_or_else(|| ruim("falta \"chave\""))?
        .iter()
        .map(|k| {
            k.as_str()
                .map(str::to_string)
                .ok_or_else(|| ruim("\"chave\" é uma lista de nomes"))
        })
        .collect::<R<_>>()?;
    no_banco(&app, move |b| {
        let mut tx = b.escrever()?;
        let chave: Vec<&str> = chave.iter().map(String::as_str).collect();
        let t = catalogo::criar_tabela(&mut tx, &nome, &colunas, &chave)?;
        tx.confirmar()?;
        Ok((StatusCode::CREATED, Json(def_json(&t))))
    })
    .await
}

async fn apagar_tabela(State(app): Estado, Path(t): Path<String>) -> R<StatusCode> {
    no_banco(&app, move |b| {
        let mut tx = b.escrever()?;
        catalogo::remover_tabela(&mut tx, &t)?;
        tx.confirmar()?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

fn lsn_de(q: &HashMap<String, String>) -> R<u64> {
    q.get("lsn").map_or(Ok(u64::MAX), |s| {
        s.parse().map_err(|_| ruim("lsn inválido"))
    })
}

async fn listar_linhas(
    State(app): Estado,
    Path(t): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> R<Json<J>> {
    no_banco(&app, move |b| {
        let l = b.ler()?;
        let tab = catalogo::tabela_obrigatoria(&l, &t)?;
        let primeira = &tab.def().colunas[tab.def().chave[0]];
        let limite: usize = q
            .get("limite")
            .map_or(Ok(100), |s| s.parse().map_err(|_| ruim("limite inválido")))?
            .min(10_000);
        let limite_de = |k: &str| {
            q.get(k)
                .map(|s| de_texto(s, primeira.tipo, &primeira.nome).map(|v| vec![v]))
                .transpose()
        };
        let (inicio, fim) = (limite_de("inicio")?, limite_de("fim")?);
        let filtro = match q.get("onde") {
            None => None,
            Some(f) => {
                let (col, val) = f
                    .split_once(':')
                    .ok_or_else(|| ruim("use onde=coluna:valor"))?;
                let i = tab
                    .def()
                    .colunas
                    .iter()
                    .position(|c| c.nome == col.to_ascii_lowercase())
                    .ok_or_else(|| ruim(format!("a tabela não tem a coluna \"{col}\"")))?;
                let c = &tab.def().colunas[i];
                Some((i, de_texto(val, c.tipo, &c.nome)?))
            }
        };
        let mut v = tab.varrer_em(&l, inicio.as_deref(), fim.as_deref(), lsn_de(&q)?)?;
        let mut linhas = Vec::new();
        while linhas.len() < limite {
            match v.proxima()? {
                Some(linha) => {
                    if filtro.as_ref().is_none_or(|(i, val)| &linha[*i] == val) {
                        linhas.push(linha_json(&tab, &linha));
                    }
                }
                None => break,
            }
        }
        Ok(Json(json!({ "lsn": l.lsn(), "linhas": linhas })))
    })
    .await
}

async fn escrever_linha(
    app: Arc<App>,
    t: String,
    corpo: J,
    substituir: bool,
) -> R<(StatusCode, Json<J>)> {
    no_banco(&app, move |b| {
        let mut tx = b.escrever()?;
        let tab = catalogo::tabela_obrigatoria(&tx, &t)?;
        let linha = linha_de_json(&tab, &corpo)?;
        let status = if substituir {
            if tab.gravar(&mut tx, &linha)? {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            }
        } else {
            tab.inserir(&mut tx, &linha)?;
            StatusCode::CREATED
        };
        let lsn = tx.confirmar()?;
        Ok((
            status,
            Json(json!({ "lsn": lsn, "linha": linha_json(&tab, &linha) })),
        ))
    })
    .await
}

async fn inserir_linha(
    State(app): Estado,
    Path(t): Path<String>,
    Json(c): Json<J>,
) -> R<(StatusCode, Json<J>)> {
    escrever_linha(app, t, c, false).await
}

async fn gravar_linha(
    State(app): Estado,
    Path(t): Path<String>,
    Json(c): Json<J>,
) -> R<(StatusCode, Json<J>)> {
    escrever_linha(app, t, c, true).await
}

async fn obter_linha(
    State(app): Estado,
    Path((t, k)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> R<Json<J>> {
    no_banco(&app, move |b| {
        let l = b.ler()?;
        let tab = catalogo::tabela_obrigatoria(&l, &t)?;
        let chave = chave_de_texto(&tab, &k)?;
        match tab.obter_em(&l, &chave, lsn_de(&q)?)? {
            Some(linha) => Ok(Json(linha_json(&tab, &linha))),
            None => Err(Erro(StatusCode::NOT_FOUND, "linha não encontrada".into())),
        }
    })
    .await
}

async fn apagar_linha(State(app): Estado, Path((t, k)): Path<(String, String)>) -> R<StatusCode> {
    no_banco(&app, move |b| {
        let mut tx = b.escrever()?;
        let tab = catalogo::tabela_obrigatoria(&tx, &t)?;
        let chave = chave_de_texto(&tab, &k)?;
        if !tab.remover(&mut tx, &chave)? {
            return Err(Erro(StatusCode::NOT_FOUND, "linha não encontrada".into()));
        }
        tx.confirmar()?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

async fn historico(State(app): Estado, Path((t, k)): Path<(String, String)>) -> R<Json<J>> {
    no_banco(&app, move |b| {
        let l = b.ler()?;
        let tab = catalogo::tabela_obrigatoria(&l, &t)?;
        let chave = chave_de_texto(&tab, &k)?;
        let v: Vec<J> = tab
            .historico(&l, &chave)?
            .into_iter()
            .map(|h| json!({ "lsn": h.lsn, "instante_us": h.instante, "linha": h.linha.map(|x| linha_json(&tab, &x)) }))
            .collect();
        Ok(Json(J::Array(v)))
    })
    .await
}

async fn compactar(
    State(app): Estado,
    Path(t): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> R<Json<J>> {
    let manter = q.get("manter_lsn").map_or(Ok(u64::MAX), |s| {
        s.parse().map_err(|_| ruim("manter_lsn inválido"))
    })?;
    no_banco(&app, move |b| {
        let mut tx = b.escrever()?;
        let tab = catalogo::tabela_obrigatoria(&tx, &t)?;
        let r = tab.compactar(&mut tx, manter)?;
        tx.confirmar()?;
        b.checkpoint()?;
        Ok(Json(json!({
            "versoes_antes": r.versoes_antes,
            "versoes_depois": r.versoes_depois,
            "chaves_apagadas": r.chaves_apagadas,
        })))
    })
    .await
}

async fn backup(State(app): Estado) -> R<Json<J>> {
    let pasta = app
        .pasta_backup
        .clone()
        .ok_or_else(|| ruim("defina SIOUX_BACKUP_DIR para usar o backup"))?;
    no_banco(&app, move |b| {
        let agora = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let destino = pasta.join(format!("dados-{agora}.sdb"));
        let bytes = b.copiar_para(&siouxdb::vfs::VfsReal::novo(), &destino)?;
        Ok(Json(
            json!({ "arquivo": destino.display().to_string(), "bytes": bytes }),
        ))
    })
    .await
}

fn valor_de_json(v: &J) -> R<Valor> {
    Ok(match v {
        J::Null => Valor::Nulo,
        J::Bool(b) => Valor::Booleano(*b),
        J::Number(n) => match n.as_i64() {
            Some(i) => Valor::Inteiro(i),
            None => Valor::Real(n.as_f64().ok_or_else(|| ruim("número inválido"))?),
        },
        J::String(s) => Valor::Texto(s.clone()),
        _ => {
            return Err(ruim(
                "parâmetro precisa ser número, texto, booleano ou null",
            ));
        }
    })
}

fn erro_sql(e: siouxdb::sql::ErroSql) -> Erro {
    match e {
        siouxdb::sql::ErroSql::Catalogo(c) => c.into(),
        outro => Erro(StatusCode::BAD_REQUEST, outro.to_string()),
    }
}

async fn sql(State(app): Estado, Json(corpo): Json<J>) -> R<Json<J>> {
    let texto = corpo["sql"]
        .as_str()
        .ok_or_else(|| ruim("falta \"sql\""))?
        .to_string();
    let params: Vec<Valor> = match &corpo["params"] {
        J::Null => Vec::new(),
        J::Array(a) => a.iter().map(valor_de_json).collect::<R<_>>()?,
        _ => return Err(ruim("\"params\" é uma lista")),
    };
    no_banco(&app, move |b| {
        let saida_json = |s: siouxdb::sql::Saida| {
            json!({
                "colunas": s.colunas,
                "linhas": s.linhas.iter().map(|l| l.iter().map(para_json).collect::<Vec<_>>()).collect::<Vec<_>>(),
                "afetadas": s.afetadas,
            })
        };
        // Só leitura e um comando: numa foto, sem esperar a vez de escrever.
        if siouxdb::sql::somente_leitura(&texto).map_err(erro_sql)? && !texto.trim().trim_end_matches(';').contains(';') {
            let l = b.ler()?;
            let s = siouxdb::sql::consultar(&l, &texto, &params).map_err(erro_sql)?;
            return Ok(Json(json!({ "lsn": l.lsn(), "resultados": [saida_json(s)] })));
        }
        let mut tx = b.escrever()?;
        let saidas = siouxdb::sql::executar(&mut tx, &texto, &params).map_err(erro_sql)?;
        let lsn = tx.confirmar()?;
        Ok(Json(json!({ "lsn": lsn, "resultados": saidas.into_iter().map(saida_json).collect::<Vec<_>>() })))
    })
    .await
}

/// Token em tempo constante (não vaza, pelo tempo de resposta, quantos caracteres batem).
fn iguais(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn exigir_token(State(app): Estado, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| iguais(t.as_bytes(), app.token.as_bytes()));
    if ok {
        next.run(req).await
    } else {
        Erro(StatusCode::UNAUTHORIZED, "token ausente ou inválido".into()).into_response()
    }
}

#[tokio::main]
async fn main() {
    let token = std::env::var("SIOUX_TOKEN").unwrap_or_default();
    if token.len() < 16 {
        eprintln!("defina SIOUX_TOKEN com pelo menos 16 caracteres");
        std::process::exit(2);
    }
    let arquivo = std::env::var("SIOUX_ARQUIVO").unwrap_or_else(|_| "dados.sdb".into());
    let senha = std::env::var("SIOUX_SENHA").ok().filter(|s| !s.is_empty());
    if senha.is_none() {
        eprintln!("aviso: SIOUX_SENHA não definida — o banco fica em claro no disco");
    }
    let endereco = std::env::var("SIOUX_ENDERECO").unwrap_or_else(|_| "127.0.0.1:7878".into());
    let banco = siouxdb::abrir_ou_criar(
        std::path::Path::new(&arquivo),
        senha.as_deref().map(str::as_bytes),
        Config::default(),
    )
    .unwrap_or_else(|e| {
        eprintln!("não abriu {arquivo}: {e}");
        std::process::exit(1);
    });
    let pasta_backup = std::env::var("SIOUX_BACKUP_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from);
    let app = Arc::new(App {
        banco,
        token,
        pasta_backup,
    });
    let rotas = Router::new()
        .route("/tabelas", get(listar_tabelas).post(criar_tabela))
        .route("/tabelas/:t", axum::routing::delete(apagar_tabela))
        .route(
            "/tabelas/:t/linhas",
            get(listar_linhas).post(inserir_linha).put(gravar_linha),
        )
        .route(
            "/tabelas/:t/linhas/:k",
            get(obter_linha).delete(apagar_linha),
        )
        .route("/tabelas/:t/linhas/:k/historico", get(historico))
        .route("/tabelas/:t/compactar", axum::routing::post(compactar))
        .route("/backup", axum::routing::post(backup))
        .route("/sql", axum::routing::post(sql))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&app),
            exigir_token,
        ))
        .route("/saude", get(saude))
        .layer(axum::extract::DefaultBodyLimit::max(1 << 20))
        .with_state(Arc::clone(&app));
    let ouvinte = tokio::net::TcpListener::bind(&endereco)
        .await
        .unwrap_or_else(|e| {
            eprintln!("não abriu {endereco}: {e}");
            std::process::exit(1);
        });
    eprintln!("sioux-microservico ouvindo em {endereco}, banco {arquivo}");
    axum::serve(ouvinte, rotas)
        .with_graceful_shutdown(sinal_de_parada())
        .await
        .expect("servidor");
    // Ao sair, passa o WAL para o arquivo principal.
    if let Ok(app) = Arc::try_unwrap(app)
        && let Err(e) = app.banco.checkpoint()
    {
        eprintln!("checkpoint ao sair falhou: {e}");
    }
}

/// Ctrl+C ou SIGTERM (o que o systemd e o pm2 mandam).
async fn sinal_de_parada() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("sinal");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
