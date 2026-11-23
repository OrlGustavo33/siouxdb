// SiouxDB Studio — painel em tempo real.
// Dentro do Tauri fala com o motor; aberto direto no navegador mostra uma prévia com
// dados simulados (claramente marcada), útil para trabalhar no visual.
"use strict";

const tauri = window.__TAURI__ && window.__TAURI__.core;
const api = tauri ? (cmd, args) => tauri.invoke(cmd, args) : criarPrevia();

const $ = (id) => document.getElementById(id);
const num = new Intl.NumberFormat("pt-BR", { maximumFractionDigits: 0 });
const num1 = new Intl.NumberFormat("pt-BR", { maximumFractionDigits: 1 });

let atual = null;        // última resposta do painel
let modoArquivo = null;  // "abrir" | "novo"
let temporizador = null;

// ---------- formatação ----------
function tempo(us) {
  if (us == null) return { v: "—", u: "" };
  if (us < 1) return { v: num1.format(us * 1000), u: "ns" };
  if (us < 1000) return { v: num1.format(us), u: "µs" };
  if (us < 1e6) return { v: num1.format(us / 1000), u: "ms" };
  return { v: num1.format(us / 1e6), u: "s" };
}
function bytes(b) {
  const u = ["B", "KiB", "MiB", "GiB", "TiB"];
  let i = 0;
  while (b >= 1024 && i < u.length - 1) { b /= 1024; i++; }
  return { v: i === 0 ? num.format(b) : num1.format(b), u: u[i] };
}
function taxa(x) {
  if (x >= 1e6) return { v: num1.format(x / 1e6), u: "mi/s" };
  if (x >= 1e4) return { v: num1.format(x / 1e3), u: "mil/s" };
  return { v: num.format(x), u: "/s" };
}
function haQuanto(segundos) {
  const d = Math.max(0, Date.now() / 1000 - segundos);
  if (d < 60) return "agora há pouco";
  if (d < 3600) return `há ${Math.floor(d / 60)} min`;
  if (d < 86400) return `há ${Math.floor(d / 3600)} h`;
  return `há ${Math.floor(d / 86400)} dias`;
}
function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

// ---------- gráfico de linha (últimos 60 s) ----------
function grafico(serie, fixoMax) {
  const n = 60;
  const pts = serie.slice(-n);
  if (pts.length < 2) return `<svg viewBox="0 0 100 40" preserveAspectRatio="none" aria-hidden="true"></svg>`;
  const max = fixoMax ?? Math.max(...pts, 1e-9);
  const min = fixoMax != null ? 0 : 0;
  const x = (i) => ((n - pts.length + i) / (n - 1)) * 100;
  const y = (v) => 38 - ((v - min) / (max - min || 1)) * 34;
  const linha = pts.map((v, i) => `${x(i).toFixed(2)},${y(v).toFixed(2)}`).join(" ");
  const area = `M${x(0).toFixed(2)},40 L${linha.replaceAll(" ", " L")} L${x(pts.length - 1).toFixed(2)},40 Z`;
  return `<svg viewBox="0 0 100 40" preserveAspectRatio="none" aria-hidden="true">
    <path class="area" d="${area}"/><polyline class="linha" points="${linha}"/></svg>`;
}

// ---------- cartões ----------
const CARTOES = [
  {
    titulo: "Conexões", alerta: "conexoes_ativas", serie: "conexoes",
    valor: (i) => ({ v: num.format(i.conexoes_ativas), u: i.conexoes_limite ? `/ ${num.format(i.conexoes_limite)}` : "" }),
    sub: (i, r) => (r.origem === "servidor" ? "conectadas ao servidor" : "modo embutido"),
  },
  {
    titulo: "Operações", alerta: null, serie: "operacoes_por_s",
    valor: (i) => taxa(i.operacoes_por_s),
    sub: (i) => `${num1.format(i.confirmacoes_por_s)} confirmações/s`,
  },
  {
    titulo: "Latência p99", alerta: "latencia_p99_us", serie: "latencia_p99_us",
    valor: (i) => tempo(i.latencia_p99_us),
    sub: (i) => { const t = tempo(i.latencia_p50_us); return `p50 ${t.v} ${t.u}`; },
  },
  {
    titulo: "Acerto do cache", alerta: "acerto_cache_pct", serie: "acerto_cache_pct", max: 100,
    valor: (i) => (i.acerto_cache_pct == null ? { v: "—", u: "" } : { v: num1.format(i.acerto_cache_pct), u: "%" }),
    sub: (i) => (i.acerto_cache_pct == null ? "sem leituras agora" : "páginas servidas da memória"),
  },
  {
    titulo: "Gravações", alerta: null, serie: "gravacoes_por_s",
    valor: (i) => taxa(i.gravacoes_por_s),
    sub: () => "páginas gravadas",
  },
  {
    titulo: "Tamanho", alerta: null, serie: "bytes_banco",
    valor: (i) => bytes(i.bytes_banco),
    sub: (i, r) => `${num.format(r.paginas)} páginas de ${r.pagina_kb} KiB`,
  },
];

function nivelDe(alertas, indicador) {
  if (!indicador) return "";
  const a = alertas.filter((x) => x.indicador === indicador);
  if (a.some((x) => x.nivel === "Critico")) return "critico";
  return a.length ? "aviso" : "";
}

function desenhar(r) {
  const { resumo, foto } = r;
  const i = foto.indicadores;

  $("origem").hidden = false;
  const rotulo = { arquivo: "Arquivo", demonstracao: "Demonstração", servidor: "Servidor" }[resumo.origem] || resumo.origem;
  $("origem").innerHTML = `<b>${esc(rotulo)}</b>${esc(resumo.caminho)}`;
  $("origem").title = resumo.caminho;
  $("versao").textContent = `motor ${resumo.versao}${tauri ? "" : " · prévia no navegador (dados simulados)"}`;

  $("alertas").innerHTML = foto.alertas
    .map((a) => `<div class="alerta ${a.nivel === "Critico" ? "critico" : "aviso"}">${esc(a.mensagem)}</div>`)
    .join("");

  $("cartoes").innerHTML = CARTOES.map((c) => {
    const v = c.valor(i);
    const nivel = nivelDe(foto.alertas, c.alerta);
    return `<div class="cartao ${nivel}">
      <h3>${c.titulo}</h3>
      <div class="valor">${esc(v.v)}<small>${esc(v.u)}</small></div>
      <div class="sub">${esc(c.sub(i, resumo))}</div>
      ${grafico(foto.series[c.serie] || [], c.max)}
    </div>`;
  }).join("");

  // Conexões: no modo embutido a única sessão é o próprio Studio.
  $("conexoes").innerHTML = `<tr><td>1</td><td>este Studio</td><td>local</td>
    <td class="estado-ativo">ativa</td><td>${resumo.origem === "demonstracao" ? "carga de demonstração" : "—"}</td></tr>`;
  $("nota-conexoes").textContent =
    "Em modo servidor esta lista mostra todas as conexões (usuário, origem, consulta atual e há quanto tempo), com Cancelar e Derrubar.";

  const v = r.verificacao;
  let verif = `<dd>nunca</dd>`;
  if (v) {
    verif = v.problemas.length
      ? `<dd class="ruim">${v.problemas.length} problema(s) · ${haQuanto(v.quando)}</dd>`
      : `<dd class="ok">íntegro · ${haQuanto(v.quando)}</dd>`;
  }
  const filaNivel = nivelDe(foto.alertas, "fila_escritor");
  $("saude").innerHTML = `
    <dt>Fila do escritor</dt><dd class="${filaNivel ? "ruim" : ""}">${num.format(i.fila_escritor)}</dd>
    <dt>Verificação</dt>${verif}
    <dt>Confirmações (geração)</dt><dd>${num.format(resumo.geracao)}</dd>
    <dt>Último LSN</dt><dd>${num.format(resumo.ultimo_lsn)}</dd>
    <dt>Páginas a liberar</dt><dd>${num.format(resumo.liberadas_pendentes)}</dd>
    <dt>Backup</dt><dd>não configurado</dd>
    <dt>Réplicas</dt><dd>não configuradas</dd>`;
}

function mostrarSessao(temSessao) {
  $("vazio").hidden = temSessao;
  $("painel").hidden = !temSessao;
  if (!temSessao) { $("origem").hidden = true; }
  document.querySelectorAll("[data-precisa-sessao]").forEach((b) => (b.disabled = !temSessao));
}

async function atualizar() {
  try {
    const r = await api("painel");
    atual = r;
    mostrarSessao(!!r);
    if (r) desenhar(r);
  } catch (e) {
    avisar(String(e));
  }
}

function ciclo() {
  clearTimeout(temporizador);
  if (document.hidden) {
    $("rodape-estado").textContent = "Pausado (janela oculta)";
    return;
  }
  $("rodape-estado").textContent = "Atualiza a cada segundo · pausa com a janela oculta";
  atualizar().finally(() => (temporizador = setTimeout(ciclo, 1000)));
}
document.addEventListener("visibilitychange", ciclo);

// ---------- mensagens ----------
let tempoMensagem = null;
function avisar(texto, ok = false) {
  const m = $("mensagem");
  m.textContent = texto;
  m.classList.toggle("ok", ok);
  m.hidden = false;
  clearTimeout(tempoMensagem);
  tempoMensagem = setTimeout(() => (m.hidden = true), ok ? 4000 : 8000);
}

// ---------- ações ----------
function abrirBarra(modo) {
  modoArquivo = modo;
  $("barra-arquivo").hidden = false;
  $("rotulo-kb").hidden = modo !== "novo";
  $("confirmar-arquivo").textContent = modo === "novo" ? "Criar" : "Abrir";
  $("caminho").focus();
}

async function executar(acao) {
  try {
    switch (acao) {
      case "abrir": abrirBarra("abrir"); return;
      case "novo": abrirBarra("novo"); return;
      case "cancelar-arquivo": $("barra-arquivo").hidden = true; return;
      case "demonstracao":
        await api("demonstracao");
        avisar("Demonstração iniciada: banco temporário com carga gerada pelo Studio. Ele é apagado ao fechar.", true);
        break;
      case "fechar":
        await api("fechar");
        break;
      case "verificar": {
        const v = await api("verificar");
        if (v.problemas.length) avisar(`Verificação: ${v.problemas.length} problema(s). Primeiro: ${v.problemas[0]}`);
        else avisar(`Verificação concluída: ${num.format(v.paginas)} páginas íntegras em ${num.format(v.duracao_ms)} ms.`, true);
        break;
      }
      case "limites": abrirLimites(); return;
    }
    await atualizar();
  } catch (e) {
    avisar(String(e));
  }
}

document.addEventListener("click", (ev) => {
  const b = ev.target.closest("button[data-acao]");
  if (b && !b.disabled) executar(b.dataset.acao);
});

$("barra-arquivo").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const caminho = $("caminho").value;
  try {
    if (modoArquivo === "novo") await api("criar", { caminho, kb: Number($("kb").value) });
    else await api("abrir", { caminho });
    $("barra-arquivo").hidden = true;
    await atualizar();
  } catch (e) {
    avisar(String(e));
  }
});

function abrirLimites() {
  const l = (atual && atual.limites) || { latencia_p99_us: 50000, acerto_cache_min_pct: 90, fila_escritor_max: 5, conexoes_uso_max_pct: 90 };
  const f = $("form-limites");
  f.p99_ms.value = l.latencia_p99_us / 1000;
  f.cache_pct.value = l.acerto_cache_min_pct;
  f.fila.value = l.fila_escritor_max;
  f.conexoes_pct.value = l.conexoes_uso_max_pct;
  $("dialogo-limites").showModal();
}

$("dialogo-limites").addEventListener("close", async () => {
  if ($("dialogo-limites").returnValue !== "salvar") return;
  const f = $("form-limites");
  const anterior = (atual && atual.limites) || {};
  const limites = {
    ...anterior,
    latencia_p99_us: Number(f.p99_ms.value) * 1000,
    acerto_cache_min_pct: Number(f.cache_pct.value),
    fila_escritor_max: Math.round(Number(f.fila.value)),
    conexoes_uso_max_pct: Number(f.conexoes_pct.value),
  };
  try {
    await api("definir_limites", { limites });
    avisar("Limites salvos. Valem a partir da próxima atualização.", true);
  } catch (e) {
    avisar(String(e));
  }
});

// ---------- prévia no navegador (sem o motor) ----------
function criarPrevia() {
  let sessao = null;
  let limites = { latencia_p99_us: 50000, acerto_cache_min_pct: 90, leituras_minimas_cache: 100, fila_escritor_max: 5, conexoes_uso_max_pct: 90 };
  const series = { conexoes: [], operacoes_por_s: [], latencia_p99_us: [], acerto_cache_pct: [], gravacoes_por_s: [], bytes_banco: [] };
  let t = 0, paginas = 2, verificacao = null;
  const push = (k, v) => { series[k].push(v); if (series[k].length > 60) series[k].shift(); };
  function passo() {
    t++;
    const onda = 0.5 + 0.5 * Math.sin((t * 2 * Math.PI) / 40);
    const ops = 3000 + 27000 * onda * (Math.random() < 0.03 ? 3 : 1);
    paginas = Math.min(4000, paginas + Math.round(ops * 0.25 / 20));
    const ind = {
      conexoes_ativas: 1, conexoes_limite: 0, operacoes_por_s: ops,
      latencia_p50_us: 3 + Math.random() * 2, latencia_p99_us: 900 + 1500 * Math.random(),
      acerto_cache_pct: 88 + Math.random() * 10, gravacoes_por_s: ops * 0.25,
      confirmacoes_por_s: ops / 400, bytes_banco: paginas * 4096, fila_escritor: 0,
    };
    push("conexoes", 1); push("operacoes_por_s", ind.operacoes_por_s); push("latencia_p99_us", ind.latencia_p99_us);
    push("acerto_cache_pct", ind.acerto_cache_pct); push("gravacoes_por_s", ind.gravacoes_por_s); push("bytes_banco", ind.bytes_banco);
    const alertas = [];
    if (ind.latencia_p99_us > limites.latencia_p99_us) alertas.push({ nivel: "Aviso", indicador: "latencia_p99_us", mensagem: "latência p99 acima do limite" });
    if (ind.acerto_cache_pct < limites.acerto_cache_min_pct) alertas.push({ nivel: "Aviso", indicador: "acerto_cache_pct", mensagem: `cache acertando ${Math.round(ind.acerto_cache_pct)}% (mínimo ${limites.acerto_cache_min_pct}%)` });
    return { indicadores: ind, series: JSON.parse(JSON.stringify(series)), alertas, leituras: t };
  }
  let foto = null;
  setInterval(() => { if (sessao) foto = passo(); }, 1000);
  return async (cmd, args) => {
    switch (cmd) {
      case "demonstracao": sessao = { origem: "demonstracao", caminho: "/tmp/siouxdb-studio-demo.sdb", pagina_kb: 4 }; foto = passo(); return {};
      case "abrir": case "criar": throw "Prévia no navegador: abrir e criar arquivos só no aplicativo.";
      case "fechar": sessao = null; return null;
      case "verificar": verificacao = { quando: Date.now() / 1000, paginas, problemas: [], duracao_ms: 12 }; return verificacao;
      case "definir_limites": limites = args.limites; return null;
      case "painel":
        if (!sessao) return null;
        return {
          resumo: { ...sessao, paginas, geracao: t, ultimo_lsn: 0, liberadas_pendentes: 3, versao: "0.0.1" },
          foto, limites, verificacao,
        };
    }
    throw `comando desconhecido: ${cmd}`;
  };
}

// Na prévia do navegador, `#demonstracao` já abre o painel (útil para capturas de tela).
if (!tauri && location.hash === "#demonstracao") api("demonstracao").then(ciclo);
else ciclo();
