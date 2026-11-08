import type { Calendar, Metric, Window } from './compute';
import { compact, dateLong, dateShort, esc, hourRange, int, monthShort, usd } from './format';

const SVG = 'http://www.w3.org/2000/svg';

/* ---------------------------------------------------------------- tooltip */

let tip: HTMLDivElement | null = null;

export function showTip(html: string, x: number, y: number): void {
  if (!tip) {
    tip = document.createElement('div');
    tip.className = 'tooltip';
    tip.setAttribute('role', 'tooltip');
    document.body.appendChild(tip);
  }
  tip.innerHTML = html;
  tip.style.display = 'block';
  const r = tip.getBoundingClientRect();
  const pad = 12;
  let left = x + 14;
  let top = y + 14;
  if (left + r.width > window.innerWidth - pad) left = x - r.width - 14;
  if (top + r.height > window.innerHeight - pad) top = y - r.height - 14;
  tip.style.left = `${Math.max(pad, left)}px`;
  tip.style.top = `${Math.max(pad, top)}px`;
}

export function hideTip(): void {
  if (tip) tip.style.display = 'none';
}

function el<K extends keyof SVGElementTagNameMap>(tag: K, attrs: Record<string, string | number>): SVGElementTagNameMap[K] {
  const e = document.createElementNS(SVG, tag);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, String(v));
  return e;
}

/** Widths measured by the caller in one pass, so drawing several charts forces one layout, not one each. */
export const widthHint = new WeakMap<Element, number>();

function hostWidth(host: HTMLElement): number {
  return widthHint.get(host) ?? host.clientWidth;
}

export function metricValue(metric: Metric, v: number): string {
  if (metric === 'cost') return usd(v);
  if (metric === 'messages') return `${int(v)} messages`;
  return `${compact(v)} tokens`;
}

/* ---------------------------------------------------------------- heatmap */

export interface HeatmapOptions {
  cal: Calendar;
  window: Window;
  values: Float64Array;
  /** Extra per-day detail rendered in the tooltip. */
  detail: (day: number) => string;
  metric: Metric;
  weekStart: 0 | 1;
  /** First day index worth showing (start of data). */
  firstDay: number;
  /** Host width measured by the caller, so several heatmaps share one layout pass. */
  width?: number;
}

/** Quantile thresholds over the non-zero values; returns the level (0-4) for a value. */
function levelScale(values: number[]): (v: number) => number {
  const nz = values.filter((v) => v > 0).sort((a, b) => a - b);
  if (!nz.length) return () => 0;
  const q = (p: number) => nz[Math.min(nz.length - 1, Math.floor(p * (nz.length - 1)))];
  const t1 = q(0.25), t2 = q(0.5), t3 = q(0.75);
  return (v) => (v <= 0 ? 0 : v <= t1 ? 1 : v <= t2 ? 2 : v <= t3 ? 3 : 4);
}

export function heatmap(host: HTMLElement, o: HeatmapOptions): void {
  const measured = o.width ?? hostWidth(host);
  host.replaceChildren();
  const today = o.cal.today;
  const cell = 10, gap = 3, step = cell + gap, left = 28, top = 16;
  // Fill the card width at a fixed cell size (GitHub style), ending this week; extend
  // further back (cells shrink) only when the selected range needs more weeks.
  const fitWeeks = Math.max(8, Math.floor(((measured || 600) - left) / step));
  const needWeeks = Math.ceil((today - Math.max(0, Math.min(o.window.start, o.firstDay)) + 7) / 7);
  const weeks = Math.max(fitWeeks, Math.min(needWeeks, 106));
  const endDow = (o.cal.date(today).getDay() - o.weekStart + 7) % 7;
  const gridStart = today - endDow - (weeks - 1) * 7;
  const spanStart = gridStart;
  const width = left + weeks * step;
  const height = top + 7 * step;
  const svg = el('svg', { viewBox: `0 0 ${width} ${height}`, class: 'heatmap', role: 'img' });
  svg.setAttribute('aria-label', 'Daily activity heatmap');

  const visible: number[] = [];
  for (let d = Math.max(0, spanStart); d <= today; d++) visible.push(o.values[d] ?? 0);
  const level = levelScale(visible);

  // Markup is assembled as one string: far cheaper than one DOM call per cell.
  const parts: string[] = [];
  const names = o.weekStart === 1 ? ['Mon', '', 'Wed', '', 'Fri', '', ''] : ['', 'Mon', '', 'Wed', '', 'Fri', ''];
  names.forEach((n, i) => {
    if (n) parts.push(`<text x="0" y="${top + i * step + cell - 1.5}" class="hm-label">${n}</text>`);
  });

  let lastMonth = -1;
  let lastLabelX = -100;
  for (let w = 0; w < weeks; w++) {
    for (let r = 0; r < 7; r++) {
      const d = gridStart + w * 7 + r;
      if (d < spanStart || d > today) continue;
      const date = o.cal.date(d);
      if (r === 0 || d === spanStart) {
        const m = date.getMonth();
        if (m !== lastMonth && date.getDate() <= 7) {
          const x = left + w * step;
          if (x - lastLabelX > 26) {
            parts.push(`<text x="${x}" y="11" class="hm-label">${monthShort(m)}</text>`);
            lastLabelX = x;
          }
          lastMonth = m;
        }
      }
      const v = d >= 0 ? o.values[d] ?? 0 : 0;
      const inRange = d >= o.window.start && d <= o.window.end;
      parts.push(
        `<rect x="${left + w * step}" y="${top + r * step}" width="${cell}" height="${cell}" rx="2.5" class="hm-cell l${level(v)}${inRange ? '' : ' out'}${d === today ? ' today' : ''}" data-day="${d}"/>`,
      );
    }
  }
  svg.innerHTML = parts.join('');

  svg.addEventListener('mousemove', (e) => {
    const t = e.target as SVGElement;
    const d = t.dataset?.day;
    if (d === undefined) return hideTip();
    const day = Number(d);
    const v = o.values[day] ?? 0;
    showTip(
      `<div class="tt-title">${dateLong(o.cal.date(day))}</div>` +
        `<div class="tt-main">${v > 0 ? metricValue(o.metric, v) : 'No activity'}</div>` +
        (v > 0 ? o.detail(day) : ''),
      e.clientX,
      e.clientY,
    );
  });
  svg.addEventListener('mouseleave', hideTip);
  host.appendChild(svg);
}

/* ---------------------------------------------------------- shared helpers */

function niceMax(v: number): number {
  if (v <= 0) return 1;
  const p = Math.pow(10, Math.floor(Math.log10(v)));
  for (const m of [1, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10]) if (m * p >= v) return m * p;
  return 10 * p;
}

export interface Series {
  label: string;
  color: string;
  values: number[];
}

function xTicks(cal: Calendar, w: Window, n: number): number[] {
  const len = w.end - w.start;
  if (len <= 0) return [w.start];
  const count = Math.min(n, len + 1);
  const out: number[] = [];
  for (let i = 0; i < count; i++) out.push(Math.round(w.start + (len * i) / (count - 1)));
  return [...new Set(out)].filter((d) => d >= 0 && cal.date(d));
}

/* -------------------------------------------------- cumulative line chart */

export function cumulativeChart(host: HTMLElement, cal: Calendar, w: Window, series: Series[], format: (v: number) => string): void {
  host.replaceChildren();
  const width = Math.max(320, hostWidth(host) || 800);
  const height = 210;
  const m = { l: 52, r: 124, t: 24, b: 22 };
  const days = w.end - w.start + 1;
  const cum = series.map((s) => {
    let acc = 0;
    return { ...s, values: s.values.map((v) => (acc += v)) };
  });
  const max = niceMax(Math.max(0, ...cum.map((s) => s.values[s.values.length - 1] ?? 0)));
  const x = (i: number) => m.l + (days <= 1 ? 0 : (i / (days - 1)) * (width - m.l - m.r));
  const y = (v: number) => m.t + (1 - v / max) * (height - m.t - m.b);
  const svg = el('svg', { viewBox: `0 0 ${width} ${height}`, width, height, class: 'chart' });

  for (let i = 0; i <= 4; i++) {
    const v = (max * i) / 4;
    svg.appendChild(el('line', { x1: m.l, x2: width - m.r, y1: y(v), y2: y(v), class: i === 0 ? 'axis' : 'grid' }));
    const t = el('text', { x: m.l - 8, y: y(v) + 4, class: 'tick', 'text-anchor': 'end' });
    t.textContent = format(v);
    svg.appendChild(t);
  }
  for (const d of xTicks(cal, w, 6)) {
    const t = el('text', { x: x(d - w.start), y: height - 6, class: 'tick', 'text-anchor': 'middle' });
    t.textContent = dateShort(cal.date(d));
    svg.appendChild(t);
  }

  // Lines, then direct end labels de-overlapped vertically.
  const ends: { y: number; s: (typeof cum)[number] }[] = [];
  for (const s of cum) {
    if (!s.values.some((v) => v > 0)) continue;
    const pts = s.values.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(' ');
    // Soft gradient area under each line.
    const gid = `cum-fill-${ends.length}`;
    const grad = el('linearGradient', { id: gid, x1: 0, x2: 0, y1: 0, y2: 1 });
    grad.append(el('stop', { offset: '0%', 'stop-color': s.color, 'stop-opacity': 0.22 }), el('stop', { offset: '100%', 'stop-color': s.color, 'stop-opacity': 0 }));
    const defs = el('defs', {});
    defs.appendChild(grad);
    svg.appendChild(defs);
    const base = y(0).toFixed(1);
    svg.appendChild(el('polygon', { points: `${x(0).toFixed(1)},${base} ${pts} ${x(s.values.length - 1).toFixed(1)},${base}`, fill: `url(#${gid})`, stroke: 'none' }));
    svg.appendChild(el('polyline', { points: pts, fill: 'none', stroke: s.color, 'stroke-width': 2, 'stroke-linejoin': 'round', 'stroke-linecap': 'round' }));
    ends.push({ y: y(s.values[s.values.length - 1]), s });
  }
  ends.sort((a, b) => a.y - b.y);
  if (ends.length) ends[0].y = Math.max(ends[0].y, m.t + 10);
  for (let i = 1; i < ends.length; i++) if (ends[i].y - ends[i - 1].y < 24) ends[i].y = ends[i - 1].y + 24;
  const overflow = ends.length ? ends[ends.length - 1].y - (height - m.b) : 0;
  if (overflow > 0) ends.forEach((e) => (e.y -= overflow));
  for (const e of ends) {
    const lx = width - m.r + 10;
    svg.appendChild(el('circle', { cx: x(days - 1), cy: y(e.s.values[e.s.values.length - 1]), r: 3.5, fill: e.s.color, class: 'end-dot' }));
    const name = el('text', { x: lx, y: e.y - 2, class: 'end-label' });
    name.textContent = e.s.label;
    const val = el('text', { x: lx, y: e.y + 11, class: 'end-value' });
    val.textContent = format(e.s.values[e.s.values.length - 1]);
    svg.append(name, val);
  }

  const cross = el('line', { y1: m.t, y2: height - m.b, class: 'crosshair', visibility: 'hidden' });
  svg.appendChild(cross);
  const hit = el('rect', { x: m.l, y: m.t, width: width - m.l - m.r, height: height - m.t - m.b, fill: 'transparent' });
  svg.appendChild(hit);
  hit.addEventListener('mousemove', (ev) => {
    const box = svg.getBoundingClientRect();
    const px = ((ev.clientX - box.left) / box.width) * width;
    const i = Math.max(0, Math.min(days - 1, Math.round(((px - m.l) / (width - m.l - m.r)) * (days - 1))));
    cross.setAttribute('x1', String(x(i)));
    cross.setAttribute('x2', String(x(i)));
    cross.setAttribute('visibility', 'visible');
    const rows = cum
      .filter((s) => s.values[s.values.length - 1] > 0)
      .map((s) => `<div class="tt-row"><span class="sw" style="background:${s.color}"></span>${esc(s.label)}<b>${format(s.values[i])}</b></div>`)
      .join('');
    const total = cum.reduce((a, s) => a + s.values[i], 0);
    showTip(`<div class="tt-title">Through ${dateLong(cal.date(w.start + i))}</div>${rows}<div class="tt-row tt-total">Total<b>${format(total)}</b></div>`, ev.clientX, ev.clientY);
  });
  hit.addEventListener('mouseleave', () => {
    cross.setAttribute('visibility', 'hidden');
    hideTip();
  });
  host.appendChild(svg);
}

/* ------------------------------------------------------ stacked bar chart */

export function stackedBars(host: HTMLElement, cal: Calendar, w: Window, series: Series[], format: (v: number) => string): void {
  host.replaceChildren();
  const width = Math.max(320, hostWidth(host) || 800);
  const height = 170;
  const m = { l: 52, r: 8, t: 8, b: 22 };
  const days = w.end - w.start + 1;
  // Bucket by week when the window is long, so bars stay legible.
  const bucket = days > 120 ? 7 : 1;
  const n = Math.ceil(days / bucket);
  const sums = series.map((s) => {
    const out = new Array(n).fill(0);
    s.values.forEach((v, i) => (out[Math.floor(i / bucket)] += v));
    return out;
  });
  const totals = Array.from({ length: n }, (_, i) => sums.reduce((a, s) => a + s[i], 0));
  const max = niceMax(Math.max(0, ...totals));
  const plotW = width - m.l - m.r;
  const bw = plotW / n;
  const barW = Math.max(1, Math.min(28, bw - (bw > 6 ? 2 : 1)));
  const y = (v: number) => m.t + (1 - v / max) * (height - m.t - m.b);
  const svg = el('svg', { viewBox: `0 0 ${width} ${height}`, width, height, class: 'chart' });
  for (let i = 0; i <= 4; i++) {
    const v = (max * i) / 4;
    svg.appendChild(el('line', { x1: m.l, x2: width - m.r, y1: y(v), y2: y(v), class: i === 0 ? 'axis' : 'grid' }));
    const t = el('text', { x: m.l - 8, y: y(v) + 4, class: 'tick', 'text-anchor': 'end' });
    t.textContent = format(v);
    svg.appendChild(t);
  }
  for (const d of xTicks(cal, w, 6)) {
    const i = Math.floor((d - w.start) / bucket);
    const t = el('text', { x: m.l + (i + 0.5) * bw, y: height - 6, class: 'tick', 'text-anchor': 'middle' });
    t.textContent = dateShort(cal.date(d));
    svg.appendChild(t);
  }
  for (let i = 0; i < n; i++) {
    let acc = 0;
    const cx = m.l + (i + 0.5) * bw - barW / 2;
    const segs = series.map((s, k) => ({ s, v: sums[k][i] })).filter((x) => x.v > 0);
    segs.forEach((seg, k) => {
      const y0 = y(acc), y1 = y(acc + seg.v);
      acc += seg.v;
      const top = k === segs.length - 1;
      const h = Math.max(0.5, y0 - y1 - (top ? 0 : 1));
      const r = top ? Math.min(3, barW / 2, h) : 0;
      const path = top
        ? `M${cx},${y0} V${y1 + r} Q${cx},${y1} ${cx + r},${y1} H${cx + barW - r} Q${cx + barW},${y1} ${cx + barW},${y1 + r} V${y0} Z`
        : `M${cx},${y0} V${y0 - h} H${cx + barW} V${y0} Z`;
      svg.appendChild(el('path', { d: path, fill: seg.s.color }));
    });
    const hit = el('rect', { x: m.l + i * bw, y: m.t, width: bw, height: height - m.t - m.b, fill: 'transparent', class: 'bar-hit' });
    hit.addEventListener('mousemove', (ev) => {
      const from = w.start + i * bucket;
      const to = Math.min(w.end, from + bucket - 1);
      const title = bucket === 1 ? dateLong(cal.date(from)) : `${dateShort(cal.date(from))} – ${dateShort(cal.date(to))}`;
      const rows = series
        .map((s, k) => ({ s, v: sums[k][i] }))
        .filter((x) => x.v > 0)
        .map((x) => `<div class="tt-row"><span class="sw" style="background:${x.s.color}"></span>${esc(x.s.label)}<b>${format(x.v)}</b></div>`)
        .join('');
      showTip(`<div class="tt-title">${title}</div>${rows || '<div class="tt-row">No activity</div>'}${segs.length > 1 ? `<div class="tt-row tt-total">Total<b>${format(totals[i])}</b></div>` : ''}`, ev.clientX, ev.clientY);
    });
    hit.addEventListener('mouseleave', hideTip);
    svg.appendChild(hit);
  }
  host.appendChild(svg);
}

/* ------------------------------------------------------------ hour profile */

export function hourBars(host: HTMLElement, hours: number[], peak: number | null, color: string): void {
  host.replaceChildren();
  const width = Math.max(260, hostWidth(host) || 400);
  const height = 178;
  const m = { l: 8, r: 8, t: 8, b: 20 };
  const max = Math.max(1, ...hours);
  const total = hours.reduce((a, b) => a + b, 0);
  const bw = (width - m.l - m.r) / 24;
  const svg = el('svg', { viewBox: `0 0 ${width} ${height}`, width, height, class: 'chart' });
  svg.appendChild(el('line', { x1: m.l, x2: width - m.r, y1: height - m.b, y2: height - m.b, class: 'axis' }));
  hours.forEach((v, h) => {
    const bh = ((height - m.t - m.b) * v) / max;
    const x = m.l + h * bw + 1.5;
    const wdt = Math.max(2, bw - 3);
    const y0 = height - m.b;
    const r = Math.min(3, wdt / 2, bh);
    if (v > 0) {
      svg.appendChild(
        el('path', {
          d: `M${x},${y0} V${y0 - bh + r} Q${x},${y0 - bh} ${x + r},${y0 - bh} H${x + wdt - r} Q${x + wdt},${y0 - bh} ${x + wdt},${y0 - bh + r} V${y0} Z`,
          fill: color,
          'fill-opacity': h === peak ? 1 : 0.38,
        }),
      );
    }
    if (h % 6 === 0) {
      const t = el('text', { x: x + wdt / 2, y: height - 6, class: 'tick', 'text-anchor': 'middle' });
      t.textContent = h === 0 ? '12a' : h === 12 ? '12p' : h < 12 ? `${h}a` : `${h - 12}p`;
      svg.appendChild(t);
    }
    const hit = el('rect', { x: m.l + h * bw, y: m.t, width: bw, height: height - m.t - m.b, fill: 'transparent' });
    hit.addEventListener('mousemove', (ev) =>
      showTip(`<div class="tt-title">${hourRange(h)}</div><div class="tt-main">${int(v)} messages</div><div class="tt-row">${total ? ((v / total) * 100).toFixed(1) : 0}% of activity</div>`, ev.clientX, ev.clientY),
    );
    hit.addEventListener('mouseleave', hideTip);
    svg.appendChild(hit);
  });
  host.appendChild(svg);
}
