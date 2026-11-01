import type { Report } from './types';
import { compactLiteLLM, compactModelsDev } from './pricing-compact.js';

const inTauri = '__TAURI_INTERNALS__' in window;

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const core = await import('@tauri-apps/api/core');
  return core.invoke<T>(cmd, args);
}

/** Loads the report; outside Tauri (browser preview) it reads /report.json. */
export async function loadReport(full = false): Promise<Report> {
  if (!inTauri) {
    const res = await fetch(`/report.json?t=${Date.now()}`);
    return res.json();
  }
  return JSON.parse(await invoke<string>('get_report', { full }));
}

export async function appReady(): Promise<void> {
  if (inTauri) await invoke('app_ready');
}

export async function onTrayRefresh(cb: () => void): Promise<void> {
  if (!inTauri) return;
  const ev = await import('@tauri-apps/api/event');
  await ev.listen('tokenstats://refresh', cb);
}

const LITELLM_URL = 'https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json';
const MODELSDEV_URL = 'https://models.dev/api.json';

/**
 * Downloads current LiteLLM prices (and models.dev tiers when reachable), compacts them
 * and hands the snapshot to the backend. Returns the snapshot date.
 */
export async function refreshPricing(): Promise<string> {
  if (!inTauri) throw new Error('Pricing refresh is only available in the desktop app');
  const res = await fetch(LITELLM_URL, { cache: 'no-store' });
  if (!res.ok) throw new Error(`LiteLLM: HTTP ${res.status}`);
  const litellm = compactLiteLLM(await res.json());
  let modelsdev = {};
  try {
    const md = await fetch(MODELSDEV_URL, { cache: 'no-store' });
    if (md.ok) modelsdev = compactModelsDev(await md.json());
  } catch {
    /* models.dev is optional */
  }
  const snapshot = { updated: new Date().toISOString().slice(0, 10), litellm, modelsdev };
  return invoke<string>('set_pricing', { snapshot: JSON.stringify(snapshot) });
}
