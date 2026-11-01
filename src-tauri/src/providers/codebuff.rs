//! Codebuff adapter: `~/.config/{manicode,manicode-dev,manicode-staging}/projects/<project>/chats/<chat>/chat-messages.json`.
//!
//! Each file is a JSON array of messages; assistant messages (`variant`/`role` of ai, agent or
//! assistant) carry usage in `metadata.usage`, `metadata.codebuff.usage` or the run-state
//! message history. `CODEBUFF_DATA_DIR` (comma-separated) overrides the default channel roots.
use crate::model::{home_dir, parse_ts, Entry, HarnessData, Prompt, Usage};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const CHANNELS: [&str; 3] = ["manicode", "manicode-dev", "manicode-staging"];

pub fn default_roots() -> Vec<PathBuf> {
    if let Ok(v) = std::env::var("CODEBUFF_DATA_DIR") {
        return v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from).collect();
    }
    CHANNELS.iter().map(|c| home_dir().join(".config").join(c)).collect()
}

#[derive(Default)]
struct Raw {
    model: Option<String>,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    extra: u64,
    signal: bool,
}

fn s<'a>(m: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    m.get(k).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty())
}

fn n(m: &Map<String, Value>, keys: &[&str]) -> u64 {
    keys.iter()
        .find_map(|k| m.get(*k).and_then(Value::as_f64))
        .filter(|f| f.is_finite())
        .map(|f| f.max(0.0).trunc() as u64)
        .unwrap_or(0)
}

fn nested(m: &Map<String, Value>, obj: &str, key: &str) -> u64 {
    m.get(obj).and_then(Value::as_object).map(|o| n(o, &[key])).unwrap_or(0)
}

/// Usage block parser. OpenAI-style `prompt_tokens` include cached tokens; they are subtracted.
fn parse_usage(v: Option<&Value>) -> Raw {
    let mut r = Raw::default();
    let Some(m) = v.and_then(Value::as_object) else { return r };
    let cache_read = n(m, &["cacheReadInputTokens", "cache_read_input_tokens"])
        .max(nested(m, "promptTokensDetails", "cachedTokens"))
        .max(nested(m, "prompt_tokens_details", "cached_tokens"));
    let openai_input = n(m, &["promptTokens", "prompt_tokens"]);
    let mut input = n(m, &["inputTokens", "input_tokens"]);
    if input == 0 && openai_input > 0 {
        input = openai_input.saturating_sub(cache_read);
    }
    r.input = input;
    r.output = n(m, &["outputTokens", "output_tokens", "completionTokens", "completion_tokens"]);
    r.cache_read = cache_read;
    r.cache_write = n(
        m,
        &[
            "cacheCreationInputTokens",
            "cache_creation_input_tokens",
            "cacheCreationTokens",
            "cache_creation_tokens",
            "cachedTokensCreated",
            "cached_tokens_created",
        ],
    );
    let total = n(m, &["totalTokens", "total_tokens", "total"]);
    let known = r.input + r.output + r.cache_read + r.cache_write;
    let missing = total.saturating_sub(known);
    if missing > 0 {
        r.output += missing; // no breakdown available: attribute the remainder to output
    }
    r.model = s(m, "model").map(str::to_string);
    r.signal = known + missing > 0;
    r
}

fn merge(t: &mut Raw, f: Raw) {
    if t.input == 0 { t.input = f.input; }
    if t.output == 0 { t.output = f.output; }
    if t.cache_read == 0 { t.cache_read = f.cache_read; }
    if t.cache_write == 0 { t.cache_write = f.cache_write; }
    if t.extra == 0 { t.extra = f.extra; }
    if t.model.is_none() { t.model = f.model; }
    t.signal = t.input + t.output + t.cache_read + t.cache_write > 0;
}

fn run_state_usage(meta: &Map<String, Value>) -> Option<Raw> {
    let hist = meta
        .get("runState")?
        .get("sessionState")?
        .get("mainAgentState")?
        .get("messageHistory")?
        .as_array()?;
    let mut total = Raw::default();
    let mut found = false;
    for item in hist.iter().rev().filter_map(Value::as_object) {
        if s(item, "role") != Some("assistant") {
            continue;
        }
        let Some(po) = item.get("providerOptions").and_then(Value::as_object) else { continue };
        let mut e = parse_usage(po.get("usage"));
        if let Some(cb) = po.get("codebuff").and_then(Value::as_object) {
            merge(&mut e, parse_usage(cb.get("usage")));
            e.model = s(cb, "model").map(str::to_string).or(e.model);
        }
        found |= e.signal || e.model.is_some();
        merge(&mut total, e);
    }
    found.then_some(total)
}

fn timestamp(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::String(t) => parse_ts(t),
        Value::Number(x) => {
            let raw = x.as_i64()?;
            let ms = if raw < 10_000_000_000 { raw.checked_mul(1000)? } else { raw };
            (ms > 0).then_some(ms)
        }
        _ => None,
    }
}

/// Chat directory names look like `2026-01-02T03-04-05.000Z`.
fn chat_id_ts(id: &str) -> Option<i64> {
    let (date, time) = id.split_once('T')?;
    parse_ts(&format!("{date}T{}", time.replacen('-', ":", 2)))
}

fn parse_file(path: &Path, out: &mut Vec<(String, Entry)>, prompts: &mut Vec<Prompt>) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let Ok(Value::Array(msgs)) = serde_json::from_str::<Value>(&text) else { return Ok(()) };
    let name = |p: Option<&Path>| p.and_then(Path::file_name).map(|x| x.to_string_lossy().into_owned());
    let chat_dir = path.parent();
    let chat_id = name(chat_dir).unwrap_or_else(|| "unknown".into());
    let project_dir = chat_dir.and_then(Path::parent).and_then(Path::parent);
    let project = name(project_dir).unwrap_or_else(|| "unknown".into());
    let channel = name(project_dir.and_then(Path::parent).and_then(Path::parent)).unwrap_or_else(|| "manicode".into());
    let session = format!("{channel}/{project}/{chat_id}");
    let file_ts = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let base_ts = chat_id_ts(&chat_id).unwrap_or(file_ts);
    for (ordinal, m) in msgs.iter().filter_map(Value::as_object).enumerate() {
        let kind = s(m, "variant").or_else(|| s(m, "role"));
        let meta = m.get("metadata").and_then(Value::as_object);
        let ts = timestamp(m.get("timestamp"))
            .or_else(|| timestamp(m.get("createdAt")))
            .or_else(|| meta.and_then(|x| timestamp(x.get("timestamp"))));
        if kind == Some("user") {
            prompts.push(Prompt { ts: ts.unwrap_or(base_ts), session: session.clone() });
            continue;
        }
        if !matches!(kind, Some("ai" | "agent" | "assistant")) {
            continue;
        }
        let mut u = Raw::default();
        if let Some(meta) = meta {
            u.model = s(meta, "model").map(str::to_string);
            merge(&mut u, parse_usage(meta.get("usage")));
            merge(&mut u, parse_usage(meta.get("codebuff").and_then(|c| c.get("usage"))));
            if let Some(rs) = run_state_usage(meta) {
                merge(&mut u, rs);
            }
        }
        if !u.signal {
            continue;
        }
        let ts = ts.unwrap_or(base_ts);
        let model = u.model.clone().unwrap_or_else(|| "codebuff-unknown".into());
        let key = match s(m, "id") {
            Some(id) => format!("{session}:{id}"),
            None => format!("{session}:{ts}:{model}:{ordinal}:{}:{}:{}:{}", u.input, u.output, u.cache_read, u.cache_write),
        };
        out.push((
            key,
            Entry {
                ts,
                provider: infer_provider(&model),
                model,
                session: session.clone(),
                project: project.clone(),
                usage: Usage {
                    input: u.input,
                    output: u.output,
                    cache_read: u.cache_read,
                    cache_write: u.cache_write,
                    ..Usage::default()
                },
                stored_cost: None, // `credits` are Codebuff credits, not currency
            },
        ));
    }
    Ok(())
}

fn infer_provider(model: &str) -> Option<String> {
    let m = model.to_ascii_lowercase();
    let p = if m.starts_with("claude-") || m.starts_with("anthropic") {
        "anthropic"
    } else if m.starts_with("gpt-") || m.starts_with("openai/") || ["o1", "o3", "o4"].iter().any(|p| m.starts_with(p)) {
        "openai"
    } else if m.starts_with("gemini") || m.starts_with("google/") {
        "google"
    } else if m.starts_with("grok") || m.starts_with("xai/") {
        "xai"
    } else if m.starts_with("openrouter/") {
        "openrouter"
    } else {
        return None;
    };
    Some(p.to_string())
}

pub fn scan(roots: &[PathBuf], out: &mut HarnessData) {
    let mut files = Vec::new();
    for root in roots.iter().filter(|r| r.exists()) {
        let base = if root.file_name().is_some_and(|n| n == "projects") { root.clone() } else { root.join("projects") };
        crate::scan::walk(&base, &mut |p: &Path| {
            if p.file_name().is_some_and(|n| n == "chat-messages.json") {
                files.push(p.to_path_buf());
            }
        });
    }
    files.sort();
    files.dedup();
    // Last occurrence of a dedup key wins (chats are rewritten as they grow).
    let mut by_key: HashMap<String, Entry> = HashMap::new();
    for f in files {
        out.files += 1;
        let mut entries = Vec::new();
        match parse_file(&f, &mut entries, &mut out.prompts) {
            Ok(()) => by_key.extend(entries),
            Err(e) => out.errors.push(format!("codebuff: {}: {e}", f.display())),
        }
    }
    let mut all: Vec<Entry> = by_key.into_values().collect();
    all.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.session.cmp(&b.session)));
    out.entries.extend(all);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_metadata_and_run_state_usage() {
        let d = std::env::temp_dir().join(format!("ts-codebuff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let a = d.join("projects/proj-a/chats/2026-01-02T03-04-05.000Z");
        let b = d.join("projects/proj-a/chats/2026-01-03T03-04-05.000Z");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let msg = r#"{"id":"m1","role":"assistant","timestamp":"2026-01-02T03:04:06.000Z","metadata":{"model":"claude-sonnet-4-20250514","usage":{"inputTokens":100,"outputTokens":50,"cacheCreationInputTokens":20,"cacheReadInputTokens":10}},"credits":1.25}"#;
        std::fs::write(a.join("chat-messages.json"), format!(r#"[{{"role":"user","text":"hi"}},{msg},{msg}]"#)).unwrap();
        let rs = r#"[{"variant":"agent","metadata":{"runState":{"sessionState":{"mainAgentState":{"messageHistory":[{"role":"user"},{"role":"assistant","providerOptions":{"codebuff":{"model":"openai/gpt-5","usage":{"prompt_tokens":100,"completion_tokens":50,"prompt_tokens_details":{"cached_tokens":10}}}}}]}}}}}]"#;
        std::fs::write(b.join("chat-messages.json"), rs).unwrap();
        let mut out = HarnessData::default();
        scan(&[d.clone()], &mut out);
        assert_eq!(out.files, 2);
        assert_eq!(out.entries.len(), 2);
        let c = out.entries.iter().find(|e| e.model.starts_with("claude")).unwrap();
        assert_eq!((c.usage.input, c.usage.output, c.usage.cache_write, c.usage.cache_read), (100, 50, 20, 10));
        assert_eq!(c.project, "proj-a");
        assert_eq!(c.ts, 1_767_323_046_000);
        let o = out.entries.iter().find(|e| e.model == "openai/gpt-5").unwrap();
        assert_eq!((o.usage.input, o.usage.output, o.usage.cache_read), (90, 50, 10));
        assert_eq!(o.provider.as_deref(), Some("openai"));
        assert_eq!(out.prompts.len(), 1);
        let _ = std::fs::remove_dir_all(d);
    }
}
