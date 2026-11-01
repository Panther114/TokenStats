//! API-equivalent pricing.
//!
//! The table is a compact LiteLLM snapshot (primary) plus first-party models.dev entries
//! (fallback and long-context tiers). Rates are USD per million tokens. Lookup order:
//! provider-prefixed exact key, exact key, family aliases, then a boundary-aware fuzzy match
//! that picks the longest matching key (the ccusage rule: `gpt-5` must not match `gpt-5.4`,
//! but `mimo-v2.5-free` matches `mimo-v2.5`).
use crate::model::Usage;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;

pub const EMBEDDED: &str = include_str!("../resources/pricing.json");

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_write: f64,
    pub cache_read: f64,
    pub input_above: Option<f64>,
    pub output_above: Option<f64>,
    pub cache_write_above: Option<f64>,
    pub cache_read_above: Option<f64>,
    /// Whole-request context threshold (models.dev tiers). `None` means LiteLLM-style
    /// marginal tiering at 200k tokens when `*_above` rates exist.
    pub threshold: Option<u64>,
}

type Row = [Option<f64>; 9];

#[derive(Deserialize)]
struct Snapshot {
    updated: String,
    litellm: HashMap<String, Row>,
    #[serde(default)]
    modelsdev: HashMap<String, Row>,
}

fn row_to_price(r: &Row) -> Option<Price> {
    let input = r[0]?;
    let output = r[1]?;
    Some(Price {
        input,
        output,
        cache_write: r[2].unwrap_or(input * 1.25),
        cache_read: r[3].unwrap_or(input * 0.1),
        input_above: r[4],
        output_above: r[5],
        cache_write_above: r[6],
        cache_read_above: r[7],
        threshold: r[8].map(|t| t as u64),
    })
}

pub struct Pricing {
    pub updated: String,
    primary: HashMap<String, Price>,
    fallback: HashMap<String, Price>,
    memo: Mutex<HashMap<(String, String), Option<(String, Price)>>>,
}

/// Bare family names some harness lines carry instead of a model id.
const FAMILY_ALIASES: &[(&str, &str)] = &[
    ("sonnet", "claude-sonnet-5-5"),
    ("opus", "claude-opus-5-5"),
    ("haiku", "claude-haiku-5-5"),
    ("gpt-5.6", "gpt-5.6-sol"),
    ("gpt-reserve", "gpt-5.6-luna"),
];

/// Providers whose models run locally and have no API price.
const LOCAL_PROVIDERS: &[&str] = &["localllm", "ollama", "lmstudio", "llama.cpp", "local"];

impl Pricing {
    pub fn from_json(text: &str) -> Result<Pricing, String> {
        let snap: Snapshot = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let mut fallback: HashMap<String, Price> = HashMap::new();
        for (k, r) in &snap.modelsdev {
            if let Some(p) = row_to_price(r) {
                fallback.insert(k.clone(), p);
            }
        }
        let mut primary: HashMap<String, Price> = HashMap::new();
        for (k, r) in &snap.litellm {
            if let Some(mut p) = row_to_price(r) {
                // Fill whole-request long-context tiers from models.dev when LiteLLM lacks them.
                if p.input_above.is_none() {
                    let bare = k.rsplit('/').next().unwrap_or(k);
                    if let Some(f) = fallback.get(bare).or_else(|| fallback.get(k.as_str())) {
                        if f.threshold.is_some() && (f.input - p.input).abs() < 1e-9 {
                            p.input_above = f.input_above;
                            p.output_above = f.output_above;
                            p.cache_write_above = f.cache_write_above;
                            p.cache_read_above = f.cache_read_above;
                            p.threshold = f.threshold;
                        }
                    }
                }
                primary.insert(k.clone(), p);
            }
        }
        Ok(Pricing { updated: snap.updated, primary, fallback, memo: Mutex::new(HashMap::new()) })
    }

    pub fn embedded() -> Pricing {
        Pricing::from_json(EMBEDDED).expect("embedded pricing snapshot is valid")
    }

    /// Resolves a model (with an optional harness provider id) to a price entry.
    /// Returns the matched table key alongside the price.
    pub fn find(&self, model: &str, provider: Option<&str>) -> Option<(String, Price)> {
        let memo_key = (model.to_string(), provider.unwrap_or("").to_string());
        if let Some(hit) = self.memo.lock().unwrap().get(&memo_key) {
            return hit.clone();
        }
        let found = self.find_uncached(model, provider);
        self.memo.lock().unwrap().insert(memo_key, found.clone());
        found
    }

    fn find_uncached(&self, model: &str, provider: Option<&str>) -> Option<(String, Price)> {
        let provider = provider.unwrap_or("").to_ascii_lowercase();
        if LOCAL_PROVIDERS.contains(&provider.as_str()) {
            return None;
        }
        let mut m = model.trim().to_ascii_lowercase();
        if m.is_empty() || m == "<synthetic>" || m == "unknown" {
            return None;
        }
        for (alias, target) in FAMILY_ALIASES {
            if m == *alias {
                m = (*target).to_string();
            }
        }
        // Free promotional variants are priced as their paid counterparts.
        if let Some(base) = m.strip_suffix("-free") {
            let prov = (!provider.is_empty()).then_some(provider.as_str());
            if let Some(found) = self.find_uncached(base, prov) {
                if found.1.input > 0.0 || found.1.output > 0.0 {
                    return Some(found);
                }
            }
        }
        let prefixes: Vec<String> = match provider.as_str() {
            "" => vec![],
            p if p.starts_with("deepseek") => vec!["deepseek".into()],
            p if p.starts_with("anthropic") => vec!["anthropic".into()],
            p if p.starts_with("openai") => vec!["openai".into()],
            p => vec![p.replace('-', "_")],
        };
        for pre in &prefixes {
            let key = format!("{pre}/{m}");
            if let Some(p) = self.primary.get(&key) {
                return Some((key, *p));
            }
        }
        if let Some(p) = self.primary.get(&m) {
            return Some((m.clone(), *p));
        }
        if let Some(p) = self.fallback.get(&m) {
            return Some((m.clone(), *p));
        }
        // Model ids that already carry a provider prefix ("anthropic/claude-...").
        if let Some((_, bare)) = m.split_once('/') {
            if let Some(found) = self.find_uncached(bare, None) {
                return Some(found);
            }
        }
        // Fuzzy: the provider-prefixed table entries first, then canonical keys.
        for pre in &prefixes {
            let scoped = self
                .primary
                .iter()
                .filter_map(|(k, p)| k.strip_prefix(pre.as_str()).and_then(|r| r.strip_prefix('/')).map(|r| (k, r, p)));
            if let Some((k, p)) = best_fuzzy(&m, scoped) {
                return Some((k, p));
            }
        }
        let canonical = self.primary.iter().filter(|(k, _)| !k.contains('/')).map(|(k, p)| (k, k.as_str(), p));
        if let Some(found) = best_fuzzy(&m, canonical) {
            return Some(found);
        }
        let fallback = self.fallback.iter().map(|(k, p)| (k, k.rsplit('/').next().unwrap_or(k), p));
        best_fuzzy(&m, fallback)
    }

    /// Cost in USD of one response.
    pub fn cost(price: &Price, u: &Usage) -> f64 {
        let per = 1e-6;
        let rate_1h = price.input * 2.0;
        let rate_1h_above = price.input_above.map(|r| r * 2.0);
        if let Some(th) = price.threshold {
            let ctx = u.input + u.cache_read + u.cache_write + u.cache_write_1h;
            let long = ctx > th;
            let pick = |base: f64, above: Option<f64>| if long { above.unwrap_or(base) } else { base };
            return per
                * (u.input as f64 * pick(price.input, price.input_above)
                    + u.output as f64 * pick(price.output, price.output_above)
                    + u.cache_write as f64 * pick(price.cache_write, price.cache_write_above)
                    + u.cache_write_1h as f64 * pick(rate_1h, rate_1h_above)
                    + u.cache_read as f64 * pick(price.cache_read, price.cache_read_above));
        }
        const TIER: u64 = 200_000;
        let tiered = |tokens: u64, base: f64, above: Option<f64>| match above {
            Some(a) if tokens > TIER => TIER as f64 * base + (tokens - TIER) as f64 * a,
            _ => tokens as f64 * base,
        };
        per * (tiered(u.input, price.input, price.input_above)
            + tiered(u.output, price.output, price.output_above)
            + tiered(u.cache_write, price.cache_write, price.cache_write_above)
            + tiered(u.cache_write_1h, rate_1h, rate_1h_above)
            + tiered(u.cache_read, price.cache_read, price.cache_read_above))
    }
}

fn best_fuzzy<'a>(
    model: &str,
    candidates: impl Iterator<Item = (&'a String, &'a str, &'a Price)>,
) -> Option<(String, Price)> {
    let norm_model = normalize(model);
    let mut best: Option<(&String, usize, &Price)> = None;
    for (key, name, price) in candidates {
        if name.len() < 3 {
            continue;
        }
        let ok = contains_key(model, name)
            || contains_key(name, model)
            || {
                let n = normalize(name);
                contains_key(&norm_model, &n) || contains_key(&n, &norm_model)
            };
        if !ok {
            continue;
        }
        let better = match best {
            None => true,
            Some((bk, blen, _)) => name.len() > blen || (name.len() == blen && key < bk),
        };
        if better {
            best = Some((key, name.len(), price));
        }
    }
    best.map(|(k, _, p)| (k.clone(), *p))
}

fn normalize(s: &str) -> String {
    s.replace(['.', '@'], "-")
}

/// True when `key` occurs in `value` on token boundaries, without swallowing a numeric
/// version continuation (`gpt-5` vs `gpt-5.4`); an 8-digit date suffix is allowed.
fn contains_key(value: &str, key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    let vb = value.as_bytes();
    let mut from = 0;
    while let Some(pos) = value[from..].find(key) {
        let i = from + pos;
        let before_ok = i == 0 || !vb[i - 1].is_ascii_alphanumeric();
        let rest = &value[i + key.len()..];
        let after_ok = suffix_ok(key, rest);
        if before_ok && after_ok {
            return true;
        }
        from = i + 1;
    }
    false
}

fn suffix_ok(key: &str, rest: &str) -> bool {
    let rb = rest.as_bytes();
    if rb.is_empty() {
        return true;
    }
    if rb[0].is_ascii_alphanumeric() {
        return false;
    }
    let key_ends_digit = key.as_bytes().last().is_some_and(|c| c.is_ascii_digit());
    if key_ends_digit && (rb[0] == b'-' || rb[0] == b'.') && rb.len() > 1 && rb[1].is_ascii_digit() {
        let digits = rb[1..].iter().take_while(|c| c.is_ascii_digit()).count();
        let date = digits == 8 && (rb.len() == 9 || !rb[9].is_ascii_alphanumeric());
        return date;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundaries() {
        assert!(contains_key("mimo-v2.5-free", "mimo-v2.5"));
        assert!(!contains_key("gpt-5.4", "gpt-5"));
        assert!(contains_key("claude-sonnet-4-5-20250929", "claude-sonnet-4-5"));
        assert!(!contains_key("claude-sonnet-4-5", "claude-sonnet-4"));
    }

    #[test]
    fn lookups() {
        let p = Pricing::embedded();
        let f = |m: &str, prov: Option<&str>| p.find(m, prov).map(|x| x.0);
        assert_eq!(f("claude-sonnet-5-5", None).as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(f("muse-spark-1.3-contributor", Some("opencode-go")).as_deref(), Some("opencode_go/muse-spark-1.3-contributor"));
        assert_eq!(f("deepseek-flash", Some("deepseek-account")).as_deref(), Some("deepseek/deepseek-flash"));
        assert!(f("minicpm5-2b", Some("localllm")).is_none());
        assert!(f("mimo-v2.5-free", Some("opencode")).is_some());
    }

    #[test]
    fn one_hour_cache_rate() {
        let price = Price { input: 1.0, output: 5.0, cache_write: 1.25, cache_read: 0.1, input_above: None, output_above: None, cache_write_above: None, cache_read_above: None, threshold: None };
        let u = Usage { cache_write: 10, cache_write_1h: 20, cache_read: 30, ..Default::default() };
        assert!((Pricing::cost(&price, &u) * 1e6 - 55.5).abs() < 1e-9);
    }
}
