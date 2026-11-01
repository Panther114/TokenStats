import type { HarnessKey, Report } from './types';

export const HARNESS_KEYS: HarnessKey[] = ['claude', 'codex', 'opencode', 'dsh'];

export type RangeKey = '7d' | '30d' | '90d' | '1y' | 'all';
export const RANGES: { key: RangeKey; label: string; days: number }[] = [
  { key: '7d', label: '7D', days: 7 },
  { key: '30d', label: '30D', days: 30 },
  { key: '90d', label: '90D', days: 90 },
  { key: '1y', label: '1Y', days: 365 },
  { key: 'all', label: 'All', days: Infinity },
];

export type Metric = 'tokens' | 'cost' | 'messages';

export interface Totals {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  reasoning: number;
  tokens: number;
  cost: number;
  responses: number;
  prompts: number;
  messages: number;
  sessions: number;
  activeDays: number;
  rangeDays: number;
  peakHour: number | null;
  hours: number[];
  favoriteModel: string | null;
  favoriteShare: number;
  currentStreak: number;
  longestStreak: number;
  longestSession: number;
  avgSessionMessages: number;
}

/** Per-day series values for one harness (or all). Index = day index. */
export interface DaySeries {
  tokens: Float64Array;
  cost: Float64Array;
  messages: Float64Array;
}

export interface ModelRow {
  name: string;
  harnesses: number[];
  providers: string[];
  priceKey: string | null;
  price: [number, number, number, number] | null;
  long: [number, number, number] | null;
  storedCost: boolean;
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  tokens: number;
  cost: number;
  responses: number;
}

/** Local-date helpers anchored on the report's epoch day. */
export class Calendar {
  readonly epoch: Date;
  readonly today: number;

  constructor(epochDay: string) {
    const [y, m, d] = epochDay.split('-').map(Number);
    this.epoch = new Date(y, m - 1, d);
    this.today = this.indexOf(new Date());
  }

  indexOf(date: Date): number {
    const a = Date.UTC(this.epoch.getFullYear(), this.epoch.getMonth(), this.epoch.getDate());
    const b = Date.UTC(date.getFullYear(), date.getMonth(), date.getDate());
    return Math.round((b - a) / 86_400_000);
  }

  date(index: number): Date {
    return new Date(this.epoch.getFullYear(), this.epoch.getMonth(), this.epoch.getDate() + index);
  }
}

export interface Window {
  start: number;
  end: number;
}

export function rangeWindow(range: RangeKey, cal: Calendar): Window {
  const r = RANGES.find((x) => x.key === range)!;
  const end = cal.today;
  const start = r.days === Infinity ? 0 : Math.max(0, end - r.days + 1);
  return { start: Math.min(start, end), end };
}

export class Analytics {
  readonly cal: Calendar;
  readonly length: number;
  private series = new Map<number, DaySeries>();

  constructor(readonly report: Report) {
    this.cal = new Calendar(report.epoch_day);
    this.length = Math.max(this.cal.today + 1, 1);
  }

  /** Daily series for a harness index, or -1 for all harnesses. */
  daySeries(h: number): DaySeries {
    const cached = this.series.get(h);
    if (cached) return cached;
    const n = this.length;
    const s: DaySeries = { tokens: new Float64Array(n), cost: new Float64Array(n), messages: new Float64Array(n) };
    for (const [d, hh, , t, c, resp] of this.report.days) {
      if ((h !== -1 && hh !== h) || d >= n) continue;
      s.tokens[d] += t[0] + t[1] + t[2] + t[3];
      s.cost[d] += c;
      s.messages[d] += resp;
    }
    for (const [d, hh, , prompts] of this.report.activity) {
      if ((h !== -1 && hh !== h) || d >= n) continue;
      s.messages[d] += prompts;
    }
    this.series.set(h, s);
    return s;
  }

  totals(h: number, w: Window): Totals {
    const t: Totals = {
      input: 0, output: 0, cacheRead: 0, cacheWrite: 0, reasoning: 0, tokens: 0, cost: 0,
      responses: 0, prompts: 0, messages: 0, sessions: 0, activeDays: 0, rangeDays: w.end - w.start + 1,
      peakHour: null, hours: new Array(24).fill(0), favoriteModel: null, favoriteShare: 0,
      currentStreak: 0, longestStreak: 0, longestSession: 0, avgSessionMessages: 0,
    };
    const byModel = new Map<string, number>();
    for (const [d, hh, m, tok, c, resp] of this.report.days) {
      if ((h !== -1 && hh !== h) || d < w.start || d > w.end) continue;
      t.input += tok[0];
      t.output += tok[1];
      t.cacheRead += tok[2];
      t.cacheWrite += tok[3];
      t.reasoning += tok[4];
      const sum = tok[0] + tok[1] + tok[2] + tok[3];
      t.tokens += sum;
      t.cost += c;
      t.responses += resp;
      const name = this.report.models[m]?.name ?? 'unknown';
      byModel.set(name, (byModel.get(name) ?? 0) + sum);
    }
    const active = new Set<number>();
    for (const [d, hh, hours, prompts] of this.report.activity) {
      if ((h !== -1 && hh !== h) || d < w.start || d > w.end) continue;
      t.prompts += prompts;
      let any = false;
      for (let i = 0; i < 24; i++) {
        t.hours[i] += hours[i];
        if (hours[i] > 0) any = true;
      }
      if (any) active.add(d);
    }
    t.messages = t.responses + t.prompts;
    t.activeDays = active.size;
    let peak = -1;
    t.hours.forEach((v, i) => {
      if (v > 0 && (peak === -1 || v > t.hours[peak])) peak = i;
    });
    t.peakHour = peak === -1 ? null : peak;
    let best = 0;
    for (const [name, v] of byModel) {
      if (v > best) {
        best = v;
        t.favoriteModel = name;
      }
    }
    t.favoriteShare = t.tokens > 0 ? best / t.tokens : 0;

    let sessionMessages = 0;
    for (const [hh, , start, end, msgs, , , days] of this.report.sessions) {
      if (h !== -1 && hh !== h) continue;
      if (!days.some((d) => d >= w.start && d <= w.end)) continue;
      t.sessions++;
      sessionMessages += msgs;
      t.longestSession = Math.max(t.longestSession, end - start);
    }
    t.avgSessionMessages = t.sessions ? sessionMessages / t.sessions : 0;

    // Streaks: longest within the window; current = run ending today (or yesterday).
    let run = 0;
    for (let d = w.start; d <= w.end; d++) {
      run = active.has(d) ? run + 1 : 0;
      t.longestStreak = Math.max(t.longestStreak, run);
    }
    const all = this.activeDaySet(h);
    let cur = all.has(this.cal.today) ? this.cal.today : this.cal.today - 1;
    while (all.has(cur)) {
      t.currentStreak++;
      cur--;
    }
    return t;
  }

  private activeCache = new Map<number, Set<number>>();
  activeDaySet(h: number): Set<number> {
    const c = this.activeCache.get(h);
    if (c) return c;
    const s = new Set<number>();
    for (const [d, hh, hours] of this.report.activity) {
      if ((h === -1 || hh === h) && hours.some((x) => x > 0)) s.add(d);
    }
    this.activeCache.set(h, s);
    return s;
  }

  models(w: Window, harness: number | null): ModelRow[] {
    const rows = new Map<string, ModelRow>();
    for (const [d, hh, m, tok, c, resp] of this.report.days) {
      if (d < w.start || d > w.end) continue;
      if (harness !== null && hh !== harness) continue;
      const info = this.report.models[m];
      const key = info.name.toLowerCase();
      let r = rows.get(key);
      if (!r) {
        r = {
          name: info.name, harnesses: [], providers: [], priceKey: info.price_key, price: info.price, long: info.long,
          storedCost: false, input: 0, output: 0, cacheRead: 0, cacheWrite: 0, tokens: 0, cost: 0, responses: 0,
        };
        rows.set(key, r);
      }
      if (!r.harnesses.includes(hh)) r.harnesses.push(hh);
      if (info.provider && !r.providers.includes(info.provider)) r.providers.push(info.provider);
      if (!r.price && info.price) {
        r.price = info.price;
        r.priceKey = info.price_key;
        r.long = info.long;
      }
      r.storedCost ||= info.stored_cost;
      r.input += tok[0];
      r.output += tok[1];
      r.cacheRead += tok[2];
      r.cacheWrite += tok[3];
      r.tokens += tok[0] + tok[1] + tok[2] + tok[3];
      r.cost += c;
      r.responses += resp;
    }
    for (const r of rows.values()) r.harnesses.sort();
    return [...rows.values()];
  }
}
