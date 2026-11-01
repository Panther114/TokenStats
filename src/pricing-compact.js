// Compacts LiteLLM and models.dev price tables into TokenStats' snapshot rows.
// Shared by scripts/build-pricing.mjs (build time) and the dashboard (daily refresh).
// Row layout, USD per million tokens: [input, output, cacheWrite, cacheRead, inputAbove,
// outputAbove, cacheWriteAbove, cacheReadAbove, contextThreshold].

// Provider prefixes worth keeping in addition to un-prefixed canonical keys.
export const KEEP_PREFIXES = [
  'anthropic', 'openai', 'deepseek', 'xiaomi_mimo', 'opencode_go', 'opencode', 'meta',
  'moonshot', 'zai', 'stepfun', 'gemini', 'xai', 'mistral', 'dashscope', 'minimax',
];
// models.dev providers that are first-party model vendors.
const MODELSDEV_VENDORS = new Set([
  'anthropic', 'openai', 'deepseek', 'xiaomi', 'google', 'moonshotai', 'zhipuai', 'stepfun',
  'xai', 'mistral', 'alibaba', 'minimax', 'meta', 'opencode',
]);

const per1m = (v) => (typeof v === 'number' && Number.isFinite(v) ? +(v * 1e6).toPrecision(6) : null);

export function compactLiteLLM(raw) {
  const out = {};
  for (const [key, v] of Object.entries(raw)) {
    if (!v || typeof v !== 'object' || key === 'sample_spec') continue;
    const slash = key.indexOf('/');
    if (slash !== -1 && !KEEP_PREFIXES.includes(key.slice(0, slash))) continue;
    if (key.split('/').length > 2) continue;
    const i = per1m(v.input_cost_per_token);
    const o = per1m(v.output_cost_per_token);
    if (i === null || o === null) continue;
    out[key.toLowerCase()] = [
      i, o,
      per1m(v.cache_creation_input_token_cost),
      per1m(v.cache_read_input_token_cost),
      per1m(v.input_cost_per_token_above_200k_tokens),
      per1m(v.output_cost_per_token_above_200k_tokens),
      per1m(v.cache_creation_input_token_cost_above_200k_tokens),
      per1m(v.cache_read_input_token_cost_above_200k_tokens),
      null,
    ];
  }
  return out;
}

export function compactModelsDev(raw) {
  const out = {};
  for (const [provider, pv] of Object.entries(raw)) {
    if (!MODELSDEV_VENDORS.has(provider)) continue;
    for (const [id, m] of Object.entries(pv.models || {})) {
      const c = m.cost;
      if (!c || typeof c.input !== 'number' || typeof c.output !== 'number') continue;
      if (c.input === 0 && c.output === 0) continue;
      const tier = (c.tiers || [])
        .filter((t) => t.tier && t.tier.type === 'context')
        .sort((a, b) => a.tier.size - b.tier.size)[0];
      const r = (x) => (typeof x === 'number' ? +x.toPrecision(6) : null);
      out[id.toLowerCase()] = [
        r(c.input), r(c.output), r(c.cache_write), r(c.cache_read),
        tier ? r(tier.input) : null, tier ? r(tier.output) : null,
        tier ? r(tier.cache_write) : null, tier ? r(tier.cache_read) : null,
        tier ? tier.tier.size : null,
      ];
    }
  }
  return out;
}

