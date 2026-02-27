use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use std::sync::Arc;
use tower_http::cors::CorsLayer;

use crate::db::Database;

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub dry_run: bool,
    /// Initial balance is surfaced to the dashboard UI and future `/api/config` endpoints.
    #[allow(dead_code)]
    pub initial_balance: f64,
}

/// Build the Axum router for the dashboard.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index_handler))
        .route("/api/stats", get(stats_handler))
        .route("/api/positions", get(positions_handler))
        .route("/api/markets", get(markets_handler))
        .route("/api/score-events", get(score_events_handler))
        .route("/api/balance-history", get(balance_history_handler))
        .layer(CorsLayer::permissive())
        .with_state(Arc::new(state))
}

/// Serve the dashboard HTML page, injecting the dry_run flag.
async fn index_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let html = DASHBOARD_HTML.replace(
        r#"<body>"#,
        &format!(r#"<body data-dryrun="{}">"#, state.dry_run),
    );
    Html(html)
}

/// GET /api/stats
async fn stats_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .db
        .get_stats()
        .map(|s| Json(s))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// GET /api/positions?limit=50&offset=0
async fn positions_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .db
        .list_positions(50, 0)
        .map(|p| Json(p))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// GET /api/markets
async fn markets_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .db
        .list_active_markets()
        .map(|m| Json(m))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// GET /api/score-events
async fn score_events_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .db
        .list_recent_score_events(50)
        .map(|e| Json(e))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// GET /api/balance-history
async fn balance_history_handler(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    state
        .db
        .get_balance_history(200)
        .map(|h| Json(h))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// Embedded single-file dashboard (HTML + CSS + JS)
const DASHBOARD_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>LiveSports Bot Dashboard</title>
<style>
  :root {
    --bg: #0f1117;
    --card: #1a1d27;
    --border: #2a2d3a;
    --accent: #6c63ff;
    --green: #00c896;
    --red: #ff4f6a;
    --text: #e0e0e0;
    --muted: #8888aa;
  }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { background: var(--bg); color: var(--text); font-family: 'Segoe UI', system-ui, sans-serif; }
  header { display: flex; align-items: center; gap: 1rem; padding: 1rem 2rem; border-bottom: 1px solid var(--border); }
  header h1 { font-size: 1.4rem; font-weight: 700; }
  .badge { padding: .2rem .6rem; border-radius: 4px; font-size: .75rem; font-weight: 700; text-transform: uppercase; }
  .badge.dryrun { background: #ff9800; color: #000; }
  .badge.live { background: var(--green); color: #000; }
  .status-dot { width: 10px; height: 10px; border-radius: 50%; background: var(--green); display: inline-block; animation: pulse 1.5s infinite; }
  @keyframes pulse { 0%,100% { opacity: 1; } 50% { opacity: .3; } }
  main { padding: 1.5rem 2rem; display: grid; gap: 1.5rem; }
  .stats-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(180px, 1fr)); gap: 1rem; }
  .stat-card { background: var(--card); border: 1px solid var(--border); border-radius: 10px; padding: 1.2rem; }
  .stat-card .label { color: var(--muted); font-size: .8rem; text-transform: uppercase; letter-spacing: .06em; margin-bottom: .4rem; }
  .stat-card .value { font-size: 1.7rem; font-weight: 700; }
  .value.pos { color: var(--green); }
  .value.neg { color: var(--red); }
  .pos { color: var(--green); }
  .neg { color: var(--red); }
  .panel { background: var(--card); border: 1px solid var(--border); border-radius: 10px; overflow: hidden; }
  .panel-header { padding: .9rem 1.2rem; border-bottom: 1px solid var(--border); font-weight: 600; display: flex; justify-content: space-between; align-items: center; }
  table { width: 100%; border-collapse: collapse; }
  th { padding: .7rem 1rem; text-align: left; font-size: .75rem; text-transform: uppercase; color: var(--muted); border-bottom: 1px solid var(--border); }
  td { padding: .65rem 1rem; font-size: .88rem; border-bottom: 1px solid #1e2130; }
  tr:last-child td { border-bottom: none; }
  .pill { display: inline-block; padding: .15rem .55rem; border-radius: 20px; font-size: .75rem; font-weight: 600; }
  .pill.open { background: rgba(108,99,255,.2); color: var(--accent); }
  .pill.profit { background: rgba(0,200,150,.15); color: var(--green); }
  .pill.loss { background: rgba(255,79,106,.15); color: var(--red); }
  .pill.stoploss { background: rgba(255,152,0,.15); color: #ff9800; }
  #chart-container { padding: 1rem; height: 200px; position: relative; }
  canvas { width: 100% !important; }
  .two-col { display: grid; grid-template-columns: 1fr 1fr; gap: 1.5rem; }
  @media (max-width: 768px) { .two-col { grid-template-columns: 1fr; } }
  .empty { color: var(--muted); text-align: center; padding: 2rem; font-size: .9rem; }
  .refresh-btn { background: none; border: 1px solid var(--border); color: var(--muted); padding: .3rem .8rem; border-radius: 6px; cursor: pointer; font-size: .8rem; }
  .refresh-btn:hover { border-color: var(--accent); color: var(--accent); }
</style>
</head>
<body>
<header>
  <span class="status-dot" id="dot"></span>
  <h1>⚡ LiveSports Bot</h1>
  <span class="badge" id="mode-badge">…</span>
  <span style="margin-left:auto;color:var(--muted);font-size:.8rem;" id="last-updated"></span>
</header>

<main>
  <!-- Stats row -->
  <div class="stats-grid" id="stats-grid">
    <div class="stat-card"><div class="label">Balance</div><div class="value" id="s-balance">–</div></div>
    <div class="stat-card"><div class="label">Open Positions</div><div class="value" id="s-open">–</div></div>
    <div class="stat-card"><div class="label">Total Trades</div><div class="value" id="s-trades">–</div></div>
    <div class="stat-card"><div class="label">Win Rate</div><div class="value" id="s-winrate">–</div></div>
    <div class="stat-card"><div class="label">Total P&L</div><div class="value" id="s-pnl">–</div></div>
    <div class="stat-card"><div class="label">WS Entry Rate</div><div class="value" id="s-ws-entry-rate">–</div></div>
    <div class="stat-card"><div class="label">REST Fallback Rate</div><div class="value" id="s-rest-fallback-rate">–</div></div>
    <div class="stat-card"><div class="label">Avg Last WS Age</div><div class="value" id="s-avg-ws-age">–</div></div>
    <div class="stat-card"><div class="label">Avg Entry WS Age</div><div class="value" id="s-avg-entry-ws-age">–</div></div>
    <div class="stat-card"><div class="label">Avg Closed CLV</div><div class="value" id="s-avg-clv-bps">–</div></div>
    <div class="stat-card"><div class="label">Calib Models</div><div class="value" id="s-cal-models">–</div></div>
    <div class="stat-card"><div class="label">Last Calibration</div><div class="value" id="s-cal-last">–</div></div>
  </div>

  <div class="panel">
    <div class="panel-header">Quote Quality by Sport</div>
    <table>
      <thead><tr><th>Sport</th><th>WS Marks</th><th>REST Fallbacks</th><th>Fallback Rate</th><th>Avg WS Age</th></tr></thead>
      <tbody id="sport-quote-tbody"><tr><td colspan="5" class="empty">Loading…</td></tr></tbody>
    </table>
  </div>

  <div class="panel">
    <div class="panel-header">CLV by Sport (Closed Trades)</div>
    <table>
      <thead><tr><th>Sport</th><th>Trades</th><th>Avg CLV</th><th>Win Rate</th></tr></thead>
      <tbody id="sport-clv-tbody"><tr><td colspan="4" class="empty">Loading…</td></tr></tbody>
    </table>
  </div>

  <!-- Balance chart -->
  <div class="panel">
    <div class="panel-header">Balance History <button class="refresh-btn" onclick="loadAll()">↻ Refresh</button></div>
    <div id="chart-container">
      <canvas id="balance-chart"></canvas>
    </div>
  </div>

  <div class="two-col">
    <!-- Positions -->
    <div class="panel">
      <div class="panel-header">Recent Positions</div>
      <table>
        <thead><tr><th>Market</th><th>Side</th><th>Size</th><th>Entry</th><th>P&L</th><th>Status</th></tr></thead>
        <tbody id="positions-tbody"><tr><td colspan="6" class="empty">Loading…</td></tr></tbody>
      </table>
    </div>

    <!-- Score Events -->
    <div class="panel">
      <div class="panel-header">Live Score Events</div>
      <table>
        <thead><tr><th>Time</th><th>Match</th><th>Score</th><th>Event</th></tr></thead>
        <tbody id="events-tbody"><tr><td colspan="4" class="empty">Loading…</td></tr></tbody>
      </table>
    </div>
  </div>

  <!-- Active Markets -->
  <div class="panel">
    <div class="panel-header">Monitored Markets</div>
    <table>
      <thead><tr><th>Question</th><th>League</th><th>YES</th><th>NO</th><th>Spread</th><th>Volume</th><th>Liquidity</th><th>Ends</th><th>Status</th></tr></thead>
      <tbody id="markets-tbody"><tr><td colspan="7" class="empty">Loading…</td></tr></tbody>
    </table>
  </div>
</main>

<script>
const fmt = new Intl.NumberFormat('en-US', { style:'currency', currency:'USD', minimumFractionDigits:2 });
const pct = v => (v*100).toFixed(1)+'%';
const ms = v => Number.isFinite(v) ? Math.round(v) + ' ms' : '–';
const bps = v => Number.isFinite(v) ? (v >= 0 ? '+' : '') + v.toFixed(1) + ' bps' : '–';
const timeAgo = ts => {
  const d = (Date.now() - new Date(ts).getTime()) / 1000;
  if (d < 60) return Math.round(d)+'s ago';
  if (d < 3600) return Math.round(d/60)+'m ago';
  return new Date(ts).toLocaleTimeString();
};

async function loadStats() {
  const r = await fetch('/api/stats');
  if (!r.ok) return;
  const s = await r.json();
  document.getElementById('s-balance').textContent = fmt.format(s.current_balance);
  document.getElementById('s-balance').className = 'value';
  document.getElementById('s-open').textContent = s.open_positions;
  document.getElementById('s-trades').textContent = s.total_trades;
  const wr = s.total_trades > 0 ? pct(s.winning_trades / s.total_trades) : '–';
  document.getElementById('s-winrate').textContent = wr;
  const pnlEl = document.getElementById('s-pnl');
  pnlEl.textContent = (s.total_pnl >= 0 ? '+' : '') + fmt.format(s.total_pnl);
  pnlEl.className = 'value ' + (s.total_pnl >= 0 ? 'pos' : 'neg');
  document.getElementById('s-ws-entry-rate').textContent = pct(s.ws_entry_rate || 0);
  document.getElementById('s-rest-fallback-rate').textContent = pct(s.rest_fallback_rate || 0);
  document.getElementById('s-avg-ws-age').textContent = ms(s.avg_last_ws_age_ms || 0);
  document.getElementById('s-avg-entry-ws-age').textContent = ms(s.avg_entry_ws_age_ms || 0);
  const clvEl = document.getElementById('s-avg-clv-bps');
  clvEl.textContent = bps(s.avg_closed_clv_bps || 0);
  clvEl.className = 'value ' + ((s.avg_closed_clv_bps || 0) >= 0 ? 'pos' : 'neg');
  document.getElementById('s-cal-models').textContent = s.calibration_models_active ?? 0;
  document.getElementById('s-cal-last').textContent = s.calibration_last_fit_at ? timeAgo(s.calibration_last_fit_at) : '–';

  const tbody = document.getElementById('sport-quote-tbody');
  const rows = Array.isArray(s.sport_quote_stats) ? s.sport_quote_stats : [];
  if (!rows.length) {
    tbody.innerHTML = '<tr><td colspan="5" class="empty">No quote telemetry yet</td></tr>';
  } else {
    tbody.innerHTML = rows.map(r => `<tr>
      <td>${r.sport}</td>
      <td>${r.ws_marks}</td>
      <td>${r.rest_fallback_marks}</td>
      <td>${pct(r.rest_fallback_rate || 0)}</td>
      <td>${ms(r.avg_ws_age_ms || 0)}</td>
    </tr>`).join('');
  }

  const clvTbody = document.getElementById('sport-clv-tbody');
  const clvRows = Array.isArray(s.sport_clv_stats) ? s.sport_clv_stats : [];
  if (!clvRows.length) {
    clvTbody.innerHTML = '<tr><td colspan="4" class="empty">No closed trades yet</td></tr>';
  } else {
    clvTbody.innerHTML = clvRows.map(r => `<tr>
      <td>${r.sport}</td>
      <td>${r.trades}</td>
      <td class="${(r.avg_clv_bps || 0) >= 0 ? 'pos' : 'neg'}">${bps(r.avg_clv_bps || 0)}</td>
      <td>${pct(r.win_rate || 0)}</td>
    </tr>`).join('');
  }
}

async function loadPositions() {
  const r = await fetch('/api/positions');
  if (!r.ok) return;
  const positions = await r.json();
  const tbody = document.getElementById('positions-tbody');
  if (!positions.length) { tbody.innerHTML = '<tr><td colspan="6" class="empty">No positions yet</td></tr>'; return; }
  tbody.innerHTML = positions.slice(0,20).map(p => {
    const pnl = p.pnl != null ? (p.pnl >= 0 ? '+' : '') + fmt.format(p.pnl) : '–';
    const pnlClass = p.pnl != null ? (p.pnl >= 0 ? 'pos' : 'neg') : '';
    const statusClass = { open:'open', closed_profit:'profit', closed_stop_loss:'stoploss', closed_loss:'loss', closed_feed_health:'stoploss', closed_time_exit:'stoploss' }[p.status] || 'open';
    const statusLabel = { open:'Open', closed_profit:'Profit', closed_stop_loss:'Stop Loss', closed_loss:'Loss', closed_feed_health:'Feed Flatten', closed_time_exit:'Time Exit' }[p.status] || p.status;
    const label = p.event_name || p.market_id.slice(0,12)+'…';
    const marketCell = p.market_slug
      ? `<a href="https://polymarket.com/event/${p.market_slug}" target="_blank" rel="noopener" style="color:var(--accent);text-decoration:none;" title="${p.market_id}">${label}</a>`
      : `<span title="${p.market_id}">${label}</span>`;
    return `<tr>
      <td>${marketCell}</td>
      <td>${p.outcome}</td>
      <td>${fmt.format(p.size_usd)}</td>
      <td>${(p.entry_price*100).toFixed(1)}¢</td>
      <td class="${pnlClass}">${pnl}</td>
      <td><span class="pill ${statusClass}">${statusLabel}</span></td>
    </tr>`;
  }).join('');
}

async function loadScoreEvents() {
  const r = await fetch('/api/score-events');
  if (!r.ok) return;
  const events = await r.json();
  const tbody = document.getElementById('events-tbody');
  if (!events.length) { tbody.innerHTML = '<tr><td colspan="4" class="empty">No events detected yet</td></tr>'; return; }
  tbody.innerHTML = events.slice(0,20).map(e => `<tr>
    <td>${timeAgo(e.detected_at)}</td>
    <td>${e.home_team} vs ${e.away_team}</td>
    <td>${e.home_score}–${e.away_score}</td>
    <td>${e.event_type.replace(/_/g,' ')}</td>
  </tr>`).join('');
}

async function loadMarkets() {
  const r = await fetch('/api/markets');
  if (!r.ok) return;
  const markets = await r.json();
  const tbody = document.getElementById('markets-tbody');
  if (!markets.length) { tbody.innerHTML = '<tr><td colspan="9" class="empty">No markets tracked yet</td></tr>'; return; }
  tbody.innerHTML = markets.slice(0,20).map(m => {
    const link = m.slug
      ? `<a href="https://polymarket.com/event/${m.slug}" target="_blank" rel="noopener" style="color:var(--accent);text-decoration:none;" title="${m.id}">${m.question}</a>`
      : `<span title="${m.id}">${m.question}</span>`;
    const ends = m.end_date ? new Date(m.end_date).toLocaleDateString() : '–';
    const liq  = m.liquidity != null ? fmt.format(m.liquidity) : '–';
    return `<tr>
      <td>${link}</td>
      <td>${m.league || m.sport || '–'}</td>
      <td>${m.yes_price != null ? pct(m.yes_price) : '–'}</td>
      <td>${m.no_price  != null ? pct(m.no_price)  : '–'}</td>
      <td>${m.volume    != null ? fmt.format(m.volume) : '–'}</td>
      <td>${liq}</td>
      <td>${ends}</td>
    </tr>`;
  }).join('');
}

let chartCtx, chartData = { labels: [], datasets: [] };
async function loadBalanceHistory() {
  const r = await fetch('/api/balance-history');
  if (!r.ok) return;
  const history = await r.json();
  if (!history.length) return;

  // Reverse so oldest first
  const sorted = history.slice().reverse();
  const labels = sorted.map(h => new Date(h.recorded_at).toLocaleTimeString());
  const data = sorted.map(h => h.balance);

  drawChart(labels, data);
}

function drawChart(labels, data) {
  const canvas = document.getElementById('balance-chart');
  const ctx = canvas.getContext('2d');
  const W = canvas.parentElement.clientWidth - 32;
  const H = 160;
  canvas.width = W;
  canvas.height = H;

  if (data.length < 2) return;
  const min = Math.min(...data) * 0.98;
  const max = Math.max(...data) * 1.02;
  const range = max - min || 1;

  ctx.clearRect(0, 0, W, H);

  // Grid lines
  ctx.strokeStyle = '#2a2d3a';
  ctx.lineWidth = 1;
  for (let i = 0; i <= 4; i++) {
    const y = H - (i / 4) * H;
    ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(W, y); ctx.stroke();
  }

  // Line
  const step = W / (data.length - 1);
  const toY = v => H - ((v - min) / range) * H;

  // Fill gradient
  const grad = ctx.createLinearGradient(0, 0, 0, H);
  grad.addColorStop(0, 'rgba(108,99,255,0.4)');
  grad.addColorStop(1, 'rgba(108,99,255,0)');
  ctx.fillStyle = grad;
  ctx.beginPath();
  ctx.moveTo(0, toY(data[0]));
  data.forEach((v, i) => ctx.lineTo(i * step, toY(v)));
  ctx.lineTo(W, H); ctx.lineTo(0, H); ctx.closePath(); ctx.fill();

  // Stroke
  ctx.strokeStyle = '#6c63ff';
  ctx.lineWidth = 2;
  ctx.beginPath();
  data.forEach((v, i) => i === 0 ? ctx.moveTo(0, toY(v)) : ctx.lineTo(i * step, toY(v)));
  ctx.stroke();
}

async function loadMode() {
  // Detect mode from stats (if balance == initial default, likely dry-run)
  const r = await fetch('/api/stats');
  if (!r.ok) return;
  const s = await r.json();
  const badge = document.getElementById('mode-badge');
  // We don't have a direct /api/mode endpoint; use a heuristic
  // The server embeds the dry_run flag in the HTML via template, but here we use a data attribute
  const isDryRun = document.body.dataset.dryrun === 'true';
  badge.textContent = isDryRun ? 'Dry Run' : 'Live';
  badge.className = 'badge ' + (isDryRun ? 'dryrun' : 'live');
}

async function loadAll() {
  await Promise.all([loadStats(), loadPositions(), loadScoreEvents(), loadMarkets(), loadBalanceHistory()]);
  document.getElementById('last-updated').textContent = 'Updated ' + new Date().toLocaleTimeString();
}

// Auto-refresh every 5 seconds
loadAll();
setInterval(loadAll, 5000);

// Set mode badge from server-injected data attribute
document.addEventListener('DOMContentLoaded', () => {
  const isDryRun = document.body.dataset.dryrun === 'true';
  const badge = document.getElementById('mode-badge');
  badge.textContent = isDryRun ? 'Dry Run' : 'Live';
  badge.className = 'badge ' + (isDryRun ? 'dryrun' : 'live');
});
</script>
</body>
</html>"#;
