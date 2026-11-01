//! Factory Droid adapter: `~/.factory/sessions/**/<id>.settings.json` session snapshots.
//!
//! Each settings file holds cumulative `tokenUsage` for the session (one entry per session; when a
//! session id appears in several files the newest snapshot wins). `DROID_SESSIONS_DIR`
//! (comma-separated) overrides the default location.
use crate::model::{home_dir, parse_ts, Entry, HarnessData, Usage};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn default_roots() -> Vec<PathBuf> {
    if let Ok(v) = std::env::var("DROID_SESSIONS_DIR") {
        return v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from).collect();
    }
    vec![home_dir().join(".factory").join("sessions")]
}

fn num(m: &Map<String, Value>, k: &str) -> u64 {
    match m.get(k).and_then(Value::as_f64) {
        Some(f) if f.is_finite() => f.max(0.0).trunc() as u64,
        _ => 0,
    }
}

fn string(m: &Map<String, Value>, k: &str) -> Option<String> {
    m.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// `custom:Claude-Opus-4.5-Thinking-[Anthropic]-0` -> `claude-opus-4-5-thinking-0`.
fn normalize_model(model: &str) -> String {
    let raw = model.strip_prefix("custom:").unwrap_or(model);
    let mut s = String::new();
    let mut depth = 0u32;
    for ch in raw.chars() {
        match ch {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => s.push(ch),
            _ => {}
        }
    }
    let mut out = String::new();
    let mut dash = false;
    for ch in s.trim().trim_end_matches('-').to_ascii_lowercase().chars() {
        if ch == '.' || ch == '-' || ch.is_whitespace() {
            if !dash {
                out.push('-');
            }
            dash = true;
        } else {
            out.push(ch);
            dash = false;
        }
    }
    out.trim_matches('-').to_string()
}

fn provider_of(lock: Option<&str>, model: &str) -> Option<String> {
    let p = lock.map(|l| l.trim().to_ascii_lowercase().replace('-', "_")).unwrap_or_default();
    match p.as_str() {
        "claude" | "anthropic" => return Some("anthropic".into()),
        "google" | "google_ai" | "gemini" | "vertex" | "vertex_ai" => return Some("google".into()),
        "xai" | "x_ai" | "grok" => return Some("xai".into()),
        "" | "unknown" => {}
        other => return Some(other.to_string()),
    }
    let o_series = model.starts_with('o') && model.as_bytes().get(1).is_some_and(u8::is_ascii_digit);
    if ["claude", "opus", "sonnet", "haiku"].iter().any(|k| model.contains(k)) {
        Some("anthropic".into())
    } else if model.starts_with("gpt-") || model.contains("-gpt-") || model.contains("chatgpt") || o_series {
        Some("openai".into())
    } else if model.contains("gemini") {
        Some("google".into())
    } else if model.contains("grok") {
        Some("xai".into())
    } else {
        None
    }
}

/// First `Model: <name>` mention in the sidecar `<id>.jsonl` (first 500 lines).
fn sidecar_model(settings: &Path, id: &str) -> Option<String> {
    let text = std::fs::read_to_string(settings.with_file_name(format!("{id}.jsonl"))).ok()?;
    text.lines().take(500).find_map(|line| {
        let tail = line.split_once("Model:")?.1;
        let raw = tail.split(['"', '\\', '[']).next()?.trim();
        let m = normalize_model(raw);
        (!m.is_empty()).then_some(m)
    })
}

fn parse_file(path: &Path, id: &str) -> Result<Option<Entry>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let Ok(Value::Object(s)) = serde_json::from_str::<Value>(&text) else { return Ok(None) };
    let Some(u) = s.get("tokenUsage").and_then(Value::as_object) else { return Ok(None) };
    let (input, mut output) = (num(u, "inputTokens"), num(u, "outputTokens"));
    let (cw, cr, mut think) = (num(u, "cacheCreationTokens"), num(u, "cacheReadTokens"), num(u, "thinkingTokens"));
    let missing = num(u, "totalTokens").saturating_sub(input + output + cw + cr + think);
    if missing > 0 {
        if output == 0 {
            output = missing;
        } else {
            think += missing;
        }
    }
    let usage = Usage { input, output: output + think, cache_read: cr, cache_write: cw, cache_write_1h: 0, reasoning: think };
    if usage.is_zero() {
        return Ok(None);
    }
    let lock = string(&s, "providerLock");
    let model = string(&s, "model")
        .map(|m| normalize_model(&m))
        .filter(|m| !m.is_empty())
        .or_else(|| sidecar_model(path, id))
        .unwrap_or_else(|| match provider_of(lock.as_deref(), "").as_deref() {
            Some("anthropic") => "claude-unknown".into(),
            Some("openai") => "gpt-unknown".into(),
            Some("google") => "gemini-unknown".into(),
            Some("xai") => "grok-unknown".into(),
            _ => "unknown".into(),
        });
    let ts = string(&s, "providerLockTimestamp").and_then(|t| parse_ts(&t)).or_else(|| {
        let d = std::fs::metadata(path).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
        Some(d.as_millis() as i64)
    });
    let Some(ts) = ts else { return Ok(None) };
    Ok(Some(Entry {
        ts,
        provider: provider_of(lock.as_deref(), &model),
        model,
        session: id.to_string(),
        project: "droid".to_string(),
        usage,
        stored_cost: None,
    }))
}

pub fn scan(roots: &[PathBuf], out: &mut HarnessData) {
    let mut files = Vec::new();
    for root in roots.iter().filter(|r| r.exists()) {
        crate::scan::walk(root, &mut |p: &Path| {
            if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(".settings.json")) {
                files.push(p.to_path_buf());
            }
        });
    }
    files.sort();
    files.dedup();
    let mut latest: HashMap<String, Entry> = HashMap::new();
    for f in files {
        out.files += 1;
        let id = f.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".settings.json")).unwrap_or("unknown");
        match parse_file(&f, id) {
            Ok(Some(e)) => {
                if latest.get(id).is_none_or(|old| e.ts >= old.ts) {
                    latest.insert(id.to_string(), e);
                }
            }
            Ok(None) => {}
            Err(e) => out.errors.push(format!("droid: {}: {e}", f.display())),
        }
    }
    let mut entries: Vec<Entry> = latest.into_values().collect();
    entries.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.session.cmp(&b.session)));
    out.entries.extend(entries);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ts-droid-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("archive")).unwrap();
        d
    }

    #[test]
    fn normalizes_names() {
        assert_eq!(normalize_model("custom:Claude-Opus-4.5-Thinking-[Anthropic]-0"), "claude-opus-4-5-thinking-0");
        assert_eq!(normalize_model("Claude-Sonnet-4-[Anthropic]"), "claude-sonnet-4");
        assert_eq!(normalize_model("gemini-2.5-pro"), "gemini-2-5-pro");
    }

    #[test]
    fn parses_usage_latest_snapshot_and_sidecar() {
        let d = fixture("main");
        let w = |p: &str, c: &str| std::fs::write(d.join(p), c).unwrap();
        w("a.settings.json", r#"{"model":"Claude-Sonnet-4-[Anthropic]","providerLock":"anthropic","providerLockTimestamp":"2026-05-01T01:02:03.000Z",
            "tokenUsage":{"inputTokens":100,"outputTokens":50,"cacheCreationTokens":20,"cacheReadTokens":10,"thinkingTokens":5}}"#);
        w("zero.settings.json", r#"{"model":"gpt-5","tokenUsage":{"inputTokens":0}}"#);
        w("archive/c.settings.json", r#"{"model":"gpt-5","providerLockTimestamp":"2026-05-01T00:00:00Z","tokenUsage":{"inputTokens":10,"outputTokens":20}}"#);
        w("c.settings.json", r#"{"model":"gpt-5","providerLockTimestamp":"2026-05-02T00:00:00Z","tokenUsage":{"inputTokens":100,"outputTokens":200}}"#);
        w("b.settings.json", r#"{"providerLockTimestamp":"2026-05-02T01:02:03.000Z","tokenUsage":{"totalTokens":456}}"#);
        w("b.jsonl", r#"{"content":"Model: Claude Opus 4.5 Thinking [Anthropic]"}"#);
        w("bad.settings.json", "{nope");
        let mut out = HarnessData::default();
        scan(&[d.clone()], &mut out);
        assert_eq!(out.entries.len(), 3);
        let a = out.entries.iter().find(|e| e.session == "a").unwrap();
        assert_eq!(a.model, "claude-sonnet-4");
        assert_eq!((a.usage.input, a.usage.output, a.usage.cache_write, a.usage.cache_read, a.usage.reasoning), (100, 55, 20, 10, 5));
        assert_eq!(a.provider.as_deref(), Some("anthropic"));
        let c = out.entries.iter().find(|e| e.session == "c").unwrap();
        assert_eq!((c.usage.input, c.usage.output), (100, 200));
        let b = out.entries.iter().find(|e| e.session == "b").unwrap();
        assert_eq!(b.model, "claude-opus-4-5-thinking");
        assert_eq!(b.usage.output, 456);
        let _ = std::fs::remove_dir_all(d);
    }
}
