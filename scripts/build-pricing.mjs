// Builds the embedded pricing snapshot (src-tauri/resources/pricing.json).
//
// Sources: LiteLLM model_prices_and_context_window.json (primary) and models.dev
// (fallback, also supplies long-context tier thresholds). Output rates are USD per
// million tokens: [input, output, cacheWrite, cacheRead, inputAbove, outputAbove,
// cacheWriteAbove, cacheReadAbove, threshold]. Missing values are null.
//
// Usage: node scripts/build-pricing.mjs [litellm.json] [modelsdev.json]
import fs from 'node:fs';
import path from 'node:path';
import { compactLiteLLM, compactModelsDev } from '../src/pricing-compact.js';

const LITELLM_URL = 'https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json';
const MODELSDEV_URL = 'https://models.dev/api.json';

async function load(fileArg, url) {
  if (fileArg) return JSON.parse(fs.readFileSync(fileArg, 'utf8'));
  const res = await fetch(url);
  if (!res.ok) throw new Error(`${url}: HTTP ${res.status}`);
  return res.json();
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === path.resolve(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1'));
if (isMain) {
  const [litellmArg, modelsDevArg] = process.argv.slice(2);
  const litellm = compactLiteLLM(await load(litellmArg, LITELLM_URL));
  const modelsdev = compactModelsDev(await load(modelsDevArg, MODELSDEV_URL));
  const snapshot = { updated: new Date().toISOString().slice(0, 10), litellm, modelsdev };
  const dest = path.join(path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, '$1')), '..', 'src-tauri', 'resources', 'pricing.json');
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  fs.writeFileSync(dest, JSON.stringify(snapshot));
  console.log(`pricing snapshot: ${Object.keys(litellm).length} LiteLLM + ${Object.keys(modelsdev).length} models.dev entries -> ${dest} (${fs.statSync(dest).size} bytes)`);
}
