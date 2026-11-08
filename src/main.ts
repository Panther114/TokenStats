import './styles.css';
import { appReady, appVersion, getProviders, loadReport, refreshPricing, resetProviders, setProviders } from './api';
import { cumulativeChart, heatmap, hideTip, hourBars, stackedBars, widthHint, type Series } from './charts';
import { Analytics, RANGES, isoDay, parseDay, rangeWindow, type CustomRange, type Metric, type ModelRow, type RangeKey, type Totals, type Window } from './compute';
import { compact, dateShort, duration, esc, hourLabel, hourRange, int, pct, pct2, rate, relativeTime, usd } from './format';
import { icon } from './icons';
import type { CatalogEntry, ProviderConfig, Report } from './types';

/* --------------------------------------------------------------- settings */

interface Settings {
  theme: 'system' | 'light' | 'dark';
  range: RangeKey;
  metric: Metric;
  dailyMetric: 'tokens' | 'cost';
  custom: CustomRange | null;
  weekStart: 0 | 1;
  heatmapColor: string | null;
  pricingCheckedAt: number;
}

const DEFAULTS: Settings = { theme: 'system', range: '30d', metric: 'tokens', dailyMetric: 'tokens', custom: null, weekStart: 1, heatmapColor: null, pricingCheckedAt: 0 };
const STORE_KEY = 'tokenstats.settings.v2';

function loadSettings(): Settings {
  try {
    return { ...DEFAULTS, ...JSON.parse(localStorage.getItem(STORE_KEY) || '{}') };
  } catch {
    return { ...DEFAULTS };
  }
}

function saveSettings(): void {
  try {
    localStorage.setItem(STORE_KEY, JSON.stringify(settings));
  } catch {
    /* storage unavailable: settings last for this session only */
  }
}

const settings = loadSettings();

const HEATMAP_PRESETS: { name: string; color: string | null }[] = [
  { name: 'Provider colors', color: null },
  { name: 'Contribution green', color: '#2da44e' },
  { name: 'Ember', color: '#eb6834' },
  { name: 'Ocean', color: '#2a78d6' },
  { name: 'Iris', color: '#7c5cff' },
  { name: 'Rose', color: '#e5487d' },
  { name: 'Graphite', color: '#7a7a80' },
];

/* ------------------------------------------------------------------ state */

let report: Report | null = null;
let A: Analytics | null = null;
let loading = false;
let lastLoad = 0;
let loadError: string | null = null;
let modelFilter: number | null = null;
let providers: ProviderConfig[] = [];
let catalog: CatalogEntry[] = [];
let version = '';
let providerError = '';
let editing: string | null = null;
let modelSort: { key: keyof ModelRow; dir: 1 | -1 } = { key: 'cost', dir: -1 };

const app = document.getElementById('app')!;

function color(h: number): string {
  return report?.harnesses[h]?.color ?? '#888888';
}

function applyTheme(): void {
  const root = document.documentElement;
  if (settings.theme === 'system') delete root.dataset.theme;
  else root.dataset.theme = settings.theme;
}

function providerIcon(id: string, c: string): string {
  return `<span class="picon" style="--c:${c}">${icon(id)}</span>`;
}

/* ------------------------------------------------------------------ icons */

const ICON = {
  logo: `<svg viewBox="0 0 64 64" aria-hidden="true"><rect x="2" y="2" width="60" height="60" rx="15" fill="currentColor" opacity=".07"/><g fill="#e07a52"><rect x="13" y="13" width="10" height="10" rx="2.6" opacity=".3"/><rect x="27" y="13" width="10" height="10" rx="2.6" opacity=".6"/><rect x="41" y="13" width="10" height="10" rx="2.6"/><rect x="13" y="27" width="10" height="10" rx="2.6" opacity=".6"/><rect x="27" y="27" width="10" height="10" rx="2.6"/><rect x="41" y="27" width="10" height="10" rx="2.6" opacity=".3"/><rect x="13" y="41" width="10" height="10" rx="2.6"/><rect x="27" y="41" width="10" height="10" rx="2.6" opacity=".3"/><rect x="41" y="41" width="10" height="10" rx="2.6" opacity=".6"/></g></svg>`,
  refresh: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M21 12a9 9 0 1 1-2.64-6.36"/><path d="M21 3v6h-6"/></svg>`,
  settings: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 7h10M18 7h2M4 17h4M12 17h8"/><circle cx="16" cy="7" r="2"/><circle cx="10" cy="17" r="2"/></svg>`,
  close: `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18"/></svg>`,
};

/* ---------------------------------------------------------------- render */

function currentWindow(): Window {
  return rangeWindow(settings.range, A!.cal, settings.custom ?? undefined);
}

function render(): void {
  hideTip();
  if (!report || !A) {
    app.innerHTML = `${topbar()}<main>${
      loadError ? `<div class="error-box"><b>Data could not be loaded.</b><div class="muted">${esc(loadError)}</div></div>` : `<div class="skeleton">Scanning local usage logs…</div>`
    }</main>${drawer()}`;
    bindChrome();
    return;
  }
  const w = currentWindow();
  const all = A.totals(-1, w);
  const per = report.harnesses.map((_, i) => A!.totals(i, w));
  app.innerHTML = `${topbar()}<main>
    ${overview(all, w)}
    ${harnessSection(per)}
    <section class="chart-row">
      <div class="card card-pad">
        <div class="card-title"><h3>Cumulative API-equivalent spend</h3><span class="hint">List prices for the recorded token counts</span></div>
        <div class="chart-host" id="cum-chart"></div>
      </div>
      <div class="card card-pad">
        <div class="card-title"><h3>Activity by hour</h3><span class="hint">Messages per local hour</span></div>
        <div class="chart-host" id="hour-chart"></div>
        <div class="hour-meta"><span>Peak <b>${all.peakHour === null ? '–' : hourRange(all.peakHour)}</b></span><span class="num">${int(all.messages)} messages</span></div>
      </div>
    </section>
    ${dailySection(w)}
    ${modelsSection(w)}
    ${footer()}
  </main>${drawer()}`;
  bindChrome();
  drawCharts(w, all);
}

function dailySection(w: Window): string {
  return `<section class="card card-pad" id="daily-section">
      <div class="card-title"><h3>${settings.dailyMetric === 'cost' ? 'Daily API-equivalent cost by harness' : 'Daily tokens by harness'}</h3><span class="hint">${w.end - w.start + 1 > 120 ? 'Weekly totals' : 'Daily totals'}</span>
        <div class="spacer"></div>
        <div class="segmented small" role="group" aria-label="Daily chart metric">${(['tokens', 'cost'] as const)
          .map((m) => `<button data-daily="${m}" aria-pressed="${settings.dailyMetric === m}">${m === 'cost' ? 'Cost' : 'Tokens'}</button>`)
          .join('')}</div></div>
      <div class="legend">${report!.harnesses.map((h, i) => `<span><i style="background:${color(i)}"></i>${esc(h.label)}</span>`).join('')}</div>
      <div class="chart-host" id="daily-chart" style="margin-top:8px"></div>
    </section>`;
}

/** Replaces one section in place, so a local toggle does not rebuild the whole page. */
function swap(id: string, html: string): void {
  const old = document.getElementById(id);
  if (!old) return render();
  const tpl = document.createElement('template');
  tpl.innerHTML = html.trim();
  hideTip();
  old.replaceWith(tpl.content);
}

function topbar(): string {
  const status = loading
    ? `<span class="status busy"><span class="dot"></span>Scanning…</span>`
    : report
      ? `<span class="status" title="${report.parsed_files} files parsed, ${report.cached_files} from cache, ${report.scan_ms} ms"><span class="dot"></span>Updated ${relativeTime(lastLoad)} · ${int(report.parsed_files + report.cached_files)} files · ${report.scan_ms} ms</span>`
      : '';
  return `<header class="topbar">
    <div class="brand">${ICON.logo}<span>TokenStats</span>${version ? `<span class="version num">v${esc(version)}</span>` : ''}</div>
    ${status}
    <div class="spacer"></div>
    <div class="segmented" role="group" aria-label="Time range">${RANGES.map(
      (r) => `<button data-range="${r.key}" aria-pressed="${settings.range === r.key}">${r.label}</button>`,
    ).join('')}</div>
    ${settings.range === 'custom' ? customInputs() : ''}
    <button class="icon-btn ${loading ? 'spin' : ''}" id="refresh" title="Refresh (rescan changed files)" aria-label="Refresh">${ICON.refresh}</button>
    <button class="icon-btn" id="open-settings" title="Settings" aria-label="Settings">${ICON.settings}</button>
  </header>`;
}

function customInputs(): string {
  const w = A ? currentWindow() : null;
  const from = w ? isoDay(A!.cal.date(w.start)) : settings.custom?.from ?? '';
  const to = w ? isoDay(A!.cal.date(w.end)) : settings.custom?.to ?? '';
  const max = A ? isoDay(A.cal.date(A.cal.today)) : '';
  return `<div class="date-range" role="group" aria-label="Custom date range">
    <input type="date" id="range-from" value="${from}" max="${max}" aria-label="From">
    <span class="muted">–</span>
    <input type="date" id="range-to" value="${to}" max="${max}" aria-label="To">
  </div>`;
}

function rangeText(w: Window): string {
  const r = RANGES.find((x) => x.key === settings.range) ?? RANGES[1];
  const days = w.end - w.start + 1;
  const label = r.key === 'all' ? 'All time' : r.key === 'custom' ? `Custom · ${days} ${days === 1 ? 'day' : 'days'}` : `Last ${r.days === 365 ? '12 months' : `${r.days} days`}`;
  return `${label} · ${dateShort(A!.cal.date(w.start))} – ${dateShort(A!.cal.date(w.end))}`;
}

function overview(t: Totals, w: Window): string {
  const parts = [
    { label: 'Input', v: t.input, c: 'var(--text)' },
    { label: 'Output', v: t.output, c: 'var(--text-2)' },
    { label: 'Cache read', v: t.cacheRead, c: 'var(--text-3)' },
    { label: 'Cache write', v: t.cacheWrite, c: 'color-mix(in oklab, var(--text-3) 45%, var(--surface-3))' },
  ];
  const sum = Math.max(1, t.tokens);
  const perDay = t.activeDays ? t.cost / t.activeDays : 0;
  const tile = (label: string, value: string, sub: string, extra = '', cls = '') =>
    `<div class="kpi"><div class="label">${label}</div><div class="value ${cls} num">${value}</div><div class="sub">${sub}</div>${extra}</div>`;
  return `<section>
    <div class="section-head">
      <div><h2>Overview</h2><p class="sub">All harnesses · ${rangeText(w)}</p></div>
    </div>
    <div class="card kpis">
      ${tile(
        'Total tokens',
        compact(t.tokens),
        `<span title="${parts.map((p) => `${p.label}: ${int(p.v)}`).join(' · ')}">In ${compact(t.input)} · Out ${compact(t.output)} · Cache ${compact(t.cacheRead + t.cacheWrite)}</span>`,
        `<div class="split-bar" title="${parts.map((p) => `${p.label}: ${int(p.v)}`).join(' · ')}">${parts.map((p) => `<span style="flex:${p.v / sum};background:${p.c}"></span>`).join('')}</div>`,
      )}
      ${tile('API-equivalent cost', usd(t.cost), t.activeDays ? `${usd(perDay)} per active day` : 'No activity')}
      ${tile('Sessions', int(t.sessions), t.sessions ? `Longest ${duration(t.longestSession)} · ${t.avgSessionMessages.toFixed(0)} msgs avg` : 'No sessions')}
      ${tile('Messages', compact(t.messages), `${int(t.prompts)} prompts · ${int(t.responses)} responses`)}
      ${tile('Active days', `${t.activeDays}<small> / ${t.rangeDays}</small>`, `${pct(t.activeDays / t.rangeDays)} of days in range`)}
      ${tile('Streak', `${t.currentStreak}<small> days</small>`, `Longest in range ${t.longestStreak} days`)}
      ${tile('Peak hour', t.peakHour === null ? '–' : hourLabel(t.peakHour), t.peakHour === null ? 'No activity' : `${hourRange(t.peakHour)} · ${pct(t.hours[t.peakHour] / Math.max(1, t.messages))} of messages`)}
      ${tile('Favorite model', esc(t.favoriteModel ?? '–'), t.favoriteModel ? `${pct(t.favoriteShare)} of tokens` : 'No activity', '', 'text')}
    </div>
  </section>`;
}

function harnessSection(per: Totals[]): string {
  const metricSel = `<div class="segmented small" role="group" aria-label="Heatmap metric">${(['tokens', 'cost', 'messages'] as Metric[])
    .map((m) => `<button data-metric="${m}" aria-pressed="${settings.metric === m}">${m === 'cost' ? 'Cost' : m === 'tokens' ? 'Tokens' : 'Messages'}</button>`)
    .join('')}</div>`;
  const cards = report!.harnesses.map((info, i) => {
    const t = per[i];
    const c = settings.heatmapColor ?? info.color;
    const path = info.roots[0] ?? 'Not found';
    const stat = (label: string, v: string, title = '') => `<div><div class="k">${label}</div><div class="v num" title="${esc(title || v)}">${v}</div></div>`;
    return `<article class="card harness ${info.found ? '' : 'empty'}" style="--c:${c}">
      <div class="harness-head">
        <div class="harness-name">
          ${providerIcon(info.icon, info.color)}
          <div style="min-width:0"><h3>${esc(info.label)}</h3><div class="path" title="${esc(info.roots.join('\n'))}">${esc(path)}</div></div>
        </div>
        <div class="harness-total"><div class="big num">${compact(t.tokens)}</div><div class="small num">${usd(t.cost)} · ${compact(t.messages)} msgs</div></div>
      </div>
      <div class="heatmap-wrap" id="hm-${i}"></div>
      <div class="harness-foot">
        <span>${info.found ? `${int(info.files)} ${info.files === 1 ? 'source' : 'files'} · ${int(info.responses)} responses total` : 'No data directory found'}${info.errors.length ? ` · ${info.errors.length} read errors` : ''}</span>
        <span class="legend-scale">Less<span></span><i class="lv0"></i><i class="lv1"></i><i class="lv2"></i><i class="lv3"></i><i class="lv4"></i><span></span>More</span>
      </div>
      <div class="mini-stats">
        ${stat('Sessions', int(t.sessions))}
        ${stat('Messages', compact(t.messages), `${int(t.prompts)} prompts, ${int(t.responses)} responses`)}
        ${stat('Cache hit rate', pct2(t.cacheHitRate), 'Average share of prompt tokens read from cache: cache read / (input + cache read + cache write)')}
        ${stat('Active days', `${t.activeDays}`)}
        ${stat('Peak hour', t.peakHour === null ? '–' : hourLabel(t.peakHour))}
        ${stat('Top model', esc(t.favoriteModel ?? '–'))}
      </div>
    </article>`;
  }).join('');
  return `<section id="providers-section">
    <div class="section-head">
      <div><h2>Providers</h2><p class="sub">Daily ${settings.metric} per provider · cells outside the selected range are dimmed</p></div>
      ${metricSel}
    </div>
    <div class="harness-grid">${cards || '<div class="card card-pad notice">No providers are enabled. Add one in Settings.</div>'}</div>
  </section>`;
}

function modelsSection(w: Window): string {
  const rows = A!.models(w, modelFilter);
  const totalTokens = rows.reduce((a, r) => a + r.tokens, 0) || 1;
  const k = modelSort.key;
  rows.sort((a, b) => {
    const nul = (v: unknown) => (v === null ? -1 : v);
    const x = nul(a[k]), y = nul(b[k]);
    if (typeof x === 'number' && typeof y === 'number') return (x - y) * modelSort.dir;
    return String(x).localeCompare(String(y)) * modelSort.dir;
  });
  const th = (label: string, key: keyof ModelRow | null, title = '') =>
    key
      ? `<th class="sortable" data-sort="${key}" ${modelSort.key === key ? `aria-sort="${modelSort.dir === 1 ? 'ascending' : 'descending'}"` : ''} title="${title}">${label}${modelSort.key === key ? (modelSort.dir === 1 ? ' ↑' : ' ↓') : ''}</th>`
      : `<th title="${title}">${label}</th>`;
  const filters = [`<button data-mfilter="" aria-pressed="${modelFilter === null}">All providers</button>`]
    .concat(report!.harnesses.map((h, i) => `<button data-mfilter="${i}" aria-pressed="${modelFilter === i}" style="--c:${h.color}"><i></i>${esc(h.label)}</button>`))
    .join('');
  const body = rows.length
    ? rows
        .map((r) => {
          const p = r.price;
          const sub = r.providers.join(', ');
          const pricedAs = r.priceKey ? `Priced as ${r.priceKey}` : 'No public API price';
          const rates = p
            ? `<div class="rates num" title="Input ${rate(p[0])} · Output ${rate(p[1])} · Cache read ${rate(p[3])} · Cache write ${rate(p[2])}${r.long ? ` · Above ${compact(r.long[2])} context: input ${rate(r.long[0])}, output ${rate(r.long[1])}` : ''}">${rate(p[0])} / ${rate(p[1])} <span class="muted">· ${rate(p[3])} / ${rate(p[2])}${r.long ? ' · tiered' : ''}</span></div>`
            : r.storedCost
              ? `<span class="muted">Recorded by harness</span>`
              : `<span class="muted" title="No public API list price is published for this model (free, preview or local model).">No public price</span>`;
          return `<tr>
            <td title="${esc(pricedAs)}"><div class="model-name">${esc(r.name)}</div>${sub ? `<div class="model-sub">${esc(sub)}</div>` : ''}</td>
            <td style="text-align:left"><span class="chips">${r.harnesses.map((h) => `<span class="chip" style="--c:${color(h)}">${esc(report!.harnesses[h].label)}</span>`).join('')}</span></td>
            <td>${int(r.responses)}</td>
            <td title="${int(r.input)}">${compact(r.input)}</td>
            <td title="${int(r.output)}">${compact(r.output)}</td>
            <td title="${int(r.cacheRead)}">${compact(r.cacheRead)}</td>
            <td title="${int(r.cacheWrite)}">${compact(r.cacheWrite)}</td>
            <td>${pct2(r.cacheHitRate)}</td>
            <td><div class="share" title="${int(r.tokens)} tokens"><span>${compact(r.tokens)}</span><span class="bar"><span style="width:${((r.tokens / totalTokens) * 100).toFixed(1)}%"></span></span></div></td>
            <td style="text-align:left">${rates}</td>
            <td><b>${usd(r.cost)}</b></td>
          </tr>`;
        })
        .join('')
    : `<tr><td colspan="11" class="muted" style="text-align:center;padding:28px">No model usage in this range.</td></tr>`;
  return `<section id="models-section">
    <div class="section-head">
      <div><h2>Models</h2><p class="sub">Token usage and API-equivalent cost per model · rates in USD per million tokens</p></div>
      <div class="filter-row">${filters}</div>
    </div>
    <div class="card">
      <div class="table-scroll"><table class="models">
        <thead><tr>
          ${th('Model', 'name')}<th style="text-align:left">Provider</th>${th('Responses', 'responses')}${th('Input', 'input', 'Fresh (uncached) input tokens')}${th('Output', 'output', 'Output tokens, including reasoning')}${th('Cache read', 'cacheRead')}${th('Cache write', 'cacheWrite')}${th('Cache hit rate', 'cacheHitRate', 'Cache read / (input + cache read + cache write)')}${th('Total', 'tokens')}<th style="text-align:left" title="USD per million tokens: input / output · cache read / cache write">Price / 1M</th>${th('Cost', 'cost')}
        </tr></thead>
        <tbody>${body}</tbody>
      </table></div>
      <div class="table-foot">Cost is computed from public list prices (LiteLLM, snapshot ${esc(report!.pricing_updated)}), including cache-read and cache-write rates and long-context tiers. A cost recorded by the harness itself is used where present. Free promotional variants are priced as their paid counterparts.</div>
    </div>
  </section>`;
}

function footer(): string {
  return `<footer class="sources">${report!.harnesses
    .map(
      (h) => `<div><b>${esc(h.label)}</b>${h.roots.length ? h.roots.map((r) => `<code>${esc(r)}</code>`).join('<br>') : 'Not detected'}</div>`,
    )
    .join('')}</footer>`;
}

function drawer(): string {
  const themes: [Settings['theme'], string][] = [['system', 'System'], ['light', 'Light'], ['dark', 'Dark']];
  return `<div class="scrim" id="scrim"></div>
  <aside class="drawer" id="drawer" aria-label="Settings" aria-hidden="true">
    <header><h2>Settings</h2><button class="icon-btn" id="close-settings" aria-label="Close settings">${ICON.close}</button></header>
    <div class="body">
      <div class="setting"><h4>Appearance</h4><p>Follows the Windows theme unless set explicitly.</p>
        <div class="segmented" role="group">${themes.map(([v, l]) => `<button data-theme-opt="${v}" aria-pressed="${settings.theme === v}">${l}</button>`).join('')}</div>
      </div>
      <div class="setting"><h4>Heatmap color</h4><p>One color for every heatmap, or each harness in its own color.</p>
        <div class="presets">${HEATMAP_PRESETS.map(
          (p) => `<button class="preset" data-preset="${p.color ?? ''}" aria-pressed="${settings.heatmapColor === p.color}"><span class="swatches">${
            p.color ? [0.3, 0.55, 0.8, 1].map((o) => `<i style="background:${p.color};opacity:${o}"></i>`).join('') : (report?.harnesses ?? []).slice(0, 4).map((h) => `<i style="background:${h.color}"></i>`).join('')
          }</span>${p.name}</button>`,
        ).join('')}
          <label class="preset" title="Custom heatmap color"><input type="color" id="custom-heat" value="${settings.heatmapColor ?? '#2da44e'}">Custom</label>
        </div>
      </div>
      ${providersSetting()}
      <div class="setting"><h4>Week starts on</h4>
        <div class="segmented" role="group"><button data-week="1" aria-pressed="${settings.weekStart === 1}">Monday</button><button data-week="0" aria-pressed="${settings.weekStart === 0}">Sunday</button></div>
      </div>
      <div class="setting"><h4>Pricing</h4><p>List prices from LiteLLM, with long-context tiers from models.dev. Refreshed automatically once a day.</p>
        <div class="btn-row"><button class="text-btn" id="update-pricing">Update prices now</button><span class="small-note" id="pricing-note">Snapshot ${esc(report?.pricing_updated ?? '–')}</span></div>
      </div>
      <div class="setting"><h4>Data</h4><p>Only new or changed log files are parsed on refresh. A full rescan rebuilds the cache.</p>
        <div class="btn-row"><button class="text-btn" id="full-rescan">Full rescan</button></div>
      </div>
      <div class="setting"><h4>About</h4><p>TokenStats ${esc(version)} · token usage from local coding-agent logs, read-only. Prices from LiteLLM and models.dev; icons from lobe-icons.</p></div>
    </div>
  </aside>`;
}

function toHex(c: string): string {
  if (/^#[0-9a-f]{6}$/i.test(c)) return c;
  const ctx = document.createElement('canvas').getContext('2d')!;
  ctx.fillStyle = c;
  return ctx.fillStyle as string;
}

/* ------------------------------------------------------------------ charts */

function drawCharts(w: Window, all: Totals): void {
  if (!report || !A) return;
  // One layout pass for every chart host, before any of them is written.
  for (const id of ['cum-chart', 'daily-chart', 'hour-chart']) {
    const h = document.getElementById(id);
    if (h) widthHint.set(h, h.clientWidth);
  }
  drawHeatmaps(w);
  drawDaily(w);
  const cum = document.getElementById('cum-chart');
  if (cum) {
    const series: Series[] = report.harnesses.map((h, i) => ({ label: h.label, color: color(i), values: Array.from(A!.daySeries(i).cost.subarray(w.start, w.end + 1)) }));
    cumulativeChart(cum, A.cal, w, series, (v) => usd(v));
  }
  const hours = document.getElementById('hour-chart');
  if (hours) hourBars(hours, all.hours, all.peakHour, 'var(--text-2)');
}

function drawDaily(w: Window): void {
  const daily = document.getElementById('daily-chart');
  if (!daily || !report || !A) return;
  const m = settings.dailyMetric;
  const series: Series[] = report.harnesses.map((h, i) => ({ label: h.label, color: color(i), values: Array.from(A!.daySeries(i)[m].subarray(w.start, w.end + 1)) }));
  stackedBars(daily, A.cal, w, series, m === 'cost' ? (v) => usd(v) : (v) => compact(v));
}

function drawHeatmaps(w: Window): void {
  if (!report || !A) return;
  const firstDay = report.days.length ? Math.min(...report.days.map((d) => d[0])) : A.cal.today;
  // Measure every host before drawing any, so the heatmaps cost one layout instead of one each.
  const hosts = report.harnesses.map((_, i) => document.getElementById(`hm-${i}`));
  const widths = hosts.map((h) => h?.clientWidth ?? 0);
  report.harnesses.forEach((_, i) => {
    const host = hosts[i];
    if (!host) return;
    const s = A!.daySeries(i);
    const values = s[settings.metric];
    heatmap(host, {
      cal: A!.cal,
      window: w,
      values,
      metric: settings.metric,
      weekStart: settings.weekStart,
      firstDay,
      width: widths[i],
      detail: (d) => {
        const rows: string[] = [];
        if (settings.metric !== 'tokens') rows.push(`<div class="tt-row">Tokens<b>${compact(s.tokens[d])}</b></div>`);
        if (settings.metric !== 'cost') rows.push(`<div class="tt-row">Cost<b>${usd(s.cost[d])}</b></div>`);
        if (settings.metric !== 'messages') rows.push(`<div class="tt-row">Messages<b>${int(s.messages[d])}</b></div>`);
        return rows.join('');
      },
    });
  });
}

/* ------------------------------------------------------------------ events */

function openDrawer(open: boolean): void {
  document.getElementById('drawer')?.classList.toggle('open', open);
  document.getElementById('drawer')?.setAttribute('aria-hidden', String(!open));
  document.getElementById('scrim')?.classList.toggle('open', open);
}

function bindChrome(): void {
  const onDate = () => {
    const f = (document.getElementById('range-from') as HTMLInputElement | null)?.value ?? '';
    const t = (document.getElementById('range-to') as HTMLInputElement | null)?.value ?? '';
    if (!parseDay(f) || !parseDay(t)) return;
    settings.custom = f <= t ? { from: f, to: t } : { from: t, to: f };
    saveSettings();
    render();
  };
  document.getElementById('range-from')?.addEventListener('change', onDate);
  document.getElementById('range-to')?.addEventListener('change', onDate);
  document.getElementById('refresh')?.addEventListener('click', () => refresh(false));
  document.getElementById('open-settings')?.addEventListener('click', () => openDrawer(true));
  document.getElementById('close-settings')?.addEventListener('click', () => openDrawer(false));
  document.getElementById('scrim')?.addEventListener('click', () => openDrawer(false));

  const rerenderKeepingDrawer = () => {
    render();
    openDrawer(true);
  };
  app.querySelectorAll<HTMLButtonElement>('[data-theme-opt]').forEach((b) =>
    b.addEventListener('click', () => {
      settings.theme = b.dataset.themeOpt as Settings['theme'];
      saveSettings();
      applyTheme();
      rerenderKeepingDrawer();
    }),
  );
  app.querySelectorAll<HTMLButtonElement>('[data-preset]').forEach((b) =>
    b.addEventListener('click', () => {
      settings.heatmapColor = b.dataset.preset || null;
      saveSettings();
      rerenderKeepingDrawer();
    }),
  );
  document.getElementById('custom-heat')?.addEventListener('change', (e) => {
    settings.heatmapColor = (e.target as HTMLInputElement).value;
    saveSettings();
    rerenderKeepingDrawer();
  });
  bindProviders(rerenderKeepingDrawer);
  app.querySelectorAll<HTMLButtonElement>('[data-week]').forEach((b) =>
    b.addEventListener('click', () => {
      settings.weekStart = Number(b.dataset.week) as 0 | 1;
      saveSettings();
      rerenderKeepingDrawer();
    }),
  );
  document.getElementById('update-pricing')?.addEventListener('click', async (e) => {
    const btn = e.currentTarget as HTMLButtonElement;
    const note = document.getElementById('pricing-note')!;
    btn.disabled = true;
    note.textContent = 'Downloading…';
    try {
      await updatePricing();
      note.textContent = `Updated · snapshot ${report?.pricing_updated ?? ''}`;
    } catch (err) {
      note.textContent = `Update failed: ${String(err)}`;
    } finally {
      btn.disabled = false;
    }
  });
  document.getElementById('full-rescan')?.addEventListener('click', () => {
    openDrawer(false);
    refresh(true);
  });
}

/** One delegated listener for the dashboard toggles; each updates only what it affects. */
app.addEventListener('click', (e) => {
  const t = (e.target as HTMLElement).closest<HTMLElement>('[data-range],[data-daily],[data-metric],[data-mfilter],[data-sort]');
  if (!t || !report || !A) return;
  const d = t.dataset;
  if (d.range !== undefined) {
    const next = d.range as RangeKey;
    // Entering custom mode starts from the range currently shown.
    if (next === 'custom' && !settings.custom) {
      const w = currentWindow();
      settings.custom = { from: isoDay(A.cal.date(w.start)), to: isoDay(A.cal.date(w.end)) };
    }
    settings.range = next;
    saveSettings();
    render();
  } else if (d.daily !== undefined) {
    settings.dailyMetric = d.daily as Settings['dailyMetric'];
    saveSettings();
    const w = currentWindow();
    swap('daily-section', dailySection(w));
    drawDaily(w);
  } else if (d.metric !== undefined) {
    settings.metric = d.metric as Metric;
    saveSettings();
    const w = currentWindow();
    swap('providers-section', harnessSection(report.harnesses.map((_, i) => A!.totals(i, w))));
    drawHeatmaps(w);
  } else if (d.mfilter !== undefined) {
    modelFilter = d.mfilter === '' ? null : Number(d.mfilter);
    swap('models-section', modelsSection(currentWindow()));
  } else if (d.sort !== undefined) {
    const key = d.sort as keyof ModelRow;
    modelSort = modelSort.key === key ? { key, dir: (modelSort.dir * -1) as 1 | -1 } : { key, dir: key === 'name' ? 1 : -1 };
    swap('models-section', modelsSection(currentWindow()));
  }
});

/* ----------------------------------------------------------------- loading */

async function refresh(full: boolean, quiet = false): Promise<void> {
  if (loading) return;
  loading = true;
  if (!quiet || !report) render();
  try {
    report = await loadReport(full);
    A = new Analytics(report);
    loadError = null;
    lastLoad = Date.now();
  } catch (e) {
    loadError = String(e);
  } finally {
    loading = false;
    render();
  }
}

async function updatePricing(): Promise<void> {
  await refreshPricing();
  settings.pricingCheckedAt = Date.now();
  saveSettings();
  await refresh(false, true);
}

let resizeTimer = 0;
let observedWidth = 0;
new ResizeObserver((entries) => {
  // Only a width change alters chart geometry; height changes come from re-renders.
  const wNow = Math.round(entries[0].contentRect.width);
  if (wNow === observedWidth) return;
  observedWidth = wNow;
  clearTimeout(resizeTimer);
  resizeTimer = window.setTimeout(() => {
    if (report && A) drawCharts(currentWindow(), A.totals(-1, currentWindow()));
  }, 120);
}).observe(document.body);

// Rescan when the window regains focus, at most once a minute.
window.addEventListener('focus', () => {
  if (Date.now() - lastLoad > 60_000) refresh(false, true);
});
window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => render());
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') openDrawer(false);
  if ((e.key === 'r' && (e.ctrlKey || e.metaKey)) || e.key === 'F5') {
    e.preventDefault();
    refresh(false);
  }
});
// The "Updated N min ago" label stays current without rescanning.
setInterval(() => {
  const s = document.querySelector('.status:not(.busy)');
  if (s && report) s.innerHTML = `<span class="dot"></span>Updated ${relativeTime(lastLoad)} · ${int(report.parsed_files + report.cached_files)} files · ${report.scan_ms} ms`;
}, 30_000);

applyTheme();
render();
requestAnimationFrame(() => appReady());
appVersion().then((v) => {
  version = v;
  render();
});
loadProviders();
refresh(false).then(async () => {
  if (Date.now() - settings.pricingCheckedAt > 86_400_000) {
    updatePricing().catch(() => {
      /* offline: keep the current snapshot */
    });
  }
});

/* --------------------------------------------------------------- providers */

async function loadProviders(): Promise<void> {
  try {
    const st = await getProviders();
    providers = st.providers;
    catalog = st.catalog;
  } catch (e) {
    providerError = String(e);
  }
}

function providersSetting(): string {
  const kindOf = (k: string) => catalog.find((c) => c.key === k);
  const rows = providers
    .map((p) => {
      const k = kindOf(p.kind);
      const c = p.color ?? k?.color ?? '#888888';
      return `<div class="prov-row" data-pid="${esc(p.id)}">
        ${providerIcon(k?.icon ?? 'custom', c)}
        <div class="prov-main"><input class="prov-name" data-pname value="${esc(p.name)}" aria-label="Provider name">
          <input class="prov-path" data-ppath value="${esc(p.paths.join('; '))}" placeholder="${esc(k?.location ?? 'Default location')}" aria-label="Custom data path"></div>
        <input type="color" data-pcolor value="${toHex(c)}" title="Color">
        <label class="switch" title="Enabled"><input type="checkbox" data-penabled ${p.enabled ? 'checked' : ''}></label>
        <button class="text-btn" data-pdelete title="Remove provider">Remove</button>
      </div>`;
    })
    .join('');
  const options = catalog
    .map((c) => `<option value="${esc(c.key)}">${esc(c.name)}${c.detected ? ' (detected)' : ''}</option>`)
    .join('');
  return `<div class="setting"><h4>Providers</h4><p>Each provider reads one harness's local logs. Leave the path empty to use the default location.</p>
    <div class="prov-list">${rows || '<p class="muted">No providers configured.</p>'}</div>
    <div class="btn-row" style="margin-top:10px"><select id="add-kind">${options}</select><button class="text-btn" id="add-provider">Add</button>
      <button class="text-btn primary" id="save-providers">Save and rescan</button><button class="text-btn" id="reset-providers">Reset</button></div>
    ${providerError ? `<div class="small-note">${esc(providerError)}</div>` : ''}
  </div>`;
}

function readProviderRows(): void {
  document.querySelectorAll<HTMLElement>('.prov-row').forEach((row) => {
    const p = providers.find((x) => x.id === row.dataset.pid);
    if (!p) return;
    p.name = (row.querySelector('[data-pname]') as HTMLInputElement).value.trim() || p.name;
    const path = (row.querySelector('[data-ppath]') as HTMLInputElement).value;
    p.paths = path.split(';').map((x) => x.trim()).filter(Boolean);
    p.color = (row.querySelector('[data-pcolor]') as HTMLInputElement).value;
    p.enabled = (row.querySelector('[data-penabled]') as HTMLInputElement).checked;
  });
}

function bindProviders(rerender: () => void): void {
  document.querySelectorAll<HTMLButtonElement>('[data-pdelete]').forEach((b) =>
    b.addEventListener('click', () => {
      readProviderRows();
      const id = (b.closest('.prov-row') as HTMLElement).dataset.pid;
      providers = providers.filter((p) => p.id !== id);
      rerender();
    }),
  );
  document.getElementById('add-provider')?.addEventListener('click', () => {
    readProviderRows();
    const kind = (document.getElementById('add-kind') as HTMLSelectElement).value;
    const k = catalog.find((c) => c.key === kind);
    if (!k) return;
    let id = kind;
    for (let n = 2; providers.some((p) => p.id === id); n++) id = `${kind}-${n}`;
    providers.push({ id, kind, name: k.name, paths: [], enabled: true, color: null });
    rerender();
  });
  document.getElementById('save-providers')?.addEventListener('click', async () => {
    readProviderRows();
    try {
      await setProviders(providers);
      providerError = '';
      openDrawer(false);
      refresh(false);
    } catch (e) {
      providerError = String(e);
      rerender();
    }
  });
  document.getElementById('reset-providers')?.addEventListener('click', async () => {
    providers = await resetProviders();
    await loadProviders();
    refresh(false);
  });
  void editing;
}
