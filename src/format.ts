const compactFmt = new Intl.NumberFormat('en-US', { notation: 'compact', maximumFractionDigits: 1 });
const intFmt = new Intl.NumberFormat('en-US');

export function compact(n: number): string {
  if (!Number.isFinite(n)) return '–';
  if (Math.abs(n) < 1000) return intFmt.format(Math.round(n));
  return compactFmt.format(n);
}

export function int(n: number): string {
  return intFmt.format(Math.round(n));
}

export function usd(n: number, precise = false): string {
  if (!Number.isFinite(n)) return '–';
  if (n === 0) return '$0';
  if (n < 0.01) return '<$0.01';
  if (!precise && n >= 10_000) return '$' + compactFmt.format(n);
  const digits = n >= 1000 ? 0 : 2;
  return '$' + n.toLocaleString('en-US', { minimumFractionDigits: digits, maximumFractionDigits: digits });
}

/** Rate in USD per million tokens. */
export function rate(n: number | null | undefined): string {
  if (n === null || n === undefined) return '–';
  if (n === 0) return '$0';
  if (n < 0.01) return '$' + n.toPrecision(2);
  return '$' + (Math.round(n * 1000) / 1000).toString();
}

export function hourLabel(h: number): string {
  const suffix = h < 12 ? 'AM' : 'PM';
  const hr = h % 12 === 0 ? 12 : h % 12;
  return `${hr} ${suffix}`;
}

export function hourRange(h: number): string {
  return `${hourLabel(h)} – ${hourLabel((h + 1) % 24)}`;
}

export function duration(ms: number): string {
  const m = Math.round(ms / 60000);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ${m % 60}m`;
  const d = Math.floor(h / 24);
  return `${d}d ${h % 24}h`;
}

export function relativeTime(ms: number): string {
  const s = Math.round((Date.now() - ms) / 1000);
  if (s < 10) return 'just now';
  if (s < 60) return `${s}s ago`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m} min ago`;
  const h = Math.round(m / 60);
  return `${h} h ago`;
}

export function pct(n: number): string {
  if (!Number.isFinite(n)) return '–';
  if (n > 0 && n < 0.001) return '<0.1%';
  return (n * 100).toFixed(n >= 0.1 ? 0 : 1) + '%';
}

export function esc(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!);
}

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
const WEEKDAYS = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];

export function monthShort(m: number): string {
  return MONTHS[m];
}

export function dateLong(d: Date): string {
  return `${WEEKDAYS[d.getDay()]}, ${MONTHS[d.getMonth()]} ${d.getDate()}, ${d.getFullYear()}`;
}

export function dateShort(d: Date): string {
  return `${MONTHS[d.getMonth()]} ${d.getDate()}`;
}
