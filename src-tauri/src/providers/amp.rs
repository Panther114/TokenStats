//! Sourcegraph Amp adapter: `~/.local/share/amp/threads/*.json` thread files.
//!
//! Usage comes from `usageLedger.events[]` (cache tokens joined from the assistant message
//! referenced by `toMessageId`), or from `messages[].usage` when no ledger exists.
//! `AMP_DATA_DIR` (comma-separated) overrides the default location.
use crate::model::{home_dir, parse_ts, Entry, HarnessData, Usage};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn default_roots() -> Vec<PathBuf> {
    if let Ok(v) = std::env::var("AMP_DATA_DIR") {
        return v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from).collect();
    }
    vec![home_dir().join(".local").join("share").join("amp")]
}

fn num(v: Option<&Value>) -> u64 {
    match v.and_then(Value::as_f64) {
        Some(f) if f.is_finite() => f.max(0.0).trunc() as u64,
        _ => 0,
    }
}

fn text<'a>(v: Option<&'a Value>) -> Option<&'a str> {
    v.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// Builds the usage; tokens not covered by the parts but present in `total` count as output.
fn usage_of(input: u64, output: u64, cache_write: u64, cache_read: u64, total: u64) -> Usage {
    let missing = total.saturating_sub(input + output + cache_write + cache_read);
    Usage { input, output: output + missing, cache_read, cache_write, cache_write_1h: 0, reasoning: 0 }
}

fn parse_thread(path: &Path) -> Result<Vec<Entry>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let Ok(thread) = serde_json::from_slice::<Value>(&bytes) else { return Ok(Vec::new()) };
    let Some(id) = text(thread.get("id")) else { return Ok(Vec::new()) };
    let messages: &[Value] = thread.get("messages").and_then(Value::as_array).map_or(&[], |v| v.as_slice());
    let assistant = |m: &&Value| text(m.get("role")) == Some("assistant");
    let mk = |ts: i64, model: &str, usage: Usage| Entry {
        ts,
        model: model.to_string(),
        provider: None,
        session: id.to_string(),
        project: "amp".to_string(),
        usage,
        stored_cost: None,
    };
    let mut out = Vec::new();
    let events = thread.get("usageLedger").and_then(|l| l.get("events")).and_then(Value::as_array);
    if let Some(events) = events {
        let mut cache: HashMap<i64, (u64, u64)> = HashMap::new();
        for m in messages.iter().filter(assistant) {
            if let Some(mid) = m.get("messageId").and_then(Value::as_i64) {
                let u = m.get("usage");
                let g = |k: &str| num(u.and_then(|u| u.get(k)));
                cache.insert(mid, (g("cacheCreationInputTokens"), g("cacheReadInputTokens")));
            }
        }
        for ev in events {
            let (Some(ts), Some(model), Some(t)) =
                (text(ev.get("timestamp")).and_then(parse_ts), text(ev.get("model")), ev.get("tokens"))
            else {
                continue;
            };
            let (cw, cr) = ev
                .get("toMessageId")
                .and_then(Value::as_i64)
                .and_then(|i| cache.get(&i).copied())
                .unwrap_or_default();
            let u = usage_of(num(t.get("input")), num(t.get("output")), cw, cr, num(t.get("total")));
            if !u.is_zero() {
                out.push(mk(ts, model, u));
            }
        }
        return Ok(out);
    }
    for m in messages.iter().filter(assistant) {
        let Some(u) = m.get("usage") else { continue };
        let (Some(ts), Some(model)) = (
            text(u.get("timestamp")).or_else(|| text(m.get("timestamp"))).and_then(parse_ts),
            text(u.get("model")).or_else(|| text(m.get("model"))),
        ) else {
            continue;
        };
        let g = |k: &str| num(u.get(k));
        let usage = usage_of(
            g("inputTokens"),
            g("outputTokens"),
            g("cacheCreationInputTokens"),
            g("cacheReadInputTokens"),
            g("totalTokens"),
        );
        if !usage.is_zero() {
            out.push(mk(ts, model, usage));
        }
    }
    Ok(out)
}

pub fn scan(roots: &[PathBuf], out: &mut HarnessData) {
    let mut files = Vec::new();
    for root in roots.iter().filter(|r| r.exists()) {
        crate::scan::walk(&root.join("threads"), &mut |p: &Path| {
            if p.extension().and_then(|e| e.to_str()) == Some("json") {
                files.push(p.to_path_buf());
            }
        });
    }
    files.sort();
    files.dedup();
    for f in files {
        out.files += 1;
        match parse_thread(&f) {
            Ok(e) => out.entries.extend(e),
            Err(e) => out.errors.push(format!("amp: {}: {e}", f.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ts-amp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("threads")).unwrap();
        d
    }

    #[test]
    fn ledger_joins_cache_and_falls_back_to_total() {
        let d = fixture("ledger");
        let t = r#"{"id":"T-1","messages":[{"role":"user"},{"role":"assistant","messageId":3,"usage":{"cacheCreationInputTokens":7,"cacheReadInputTokens":90}}],
            "usageLedger":{"events":[
              {"id":"e1","timestamp":"2026-01-02T00:00:00.000Z","model":"claude-sonnet-4","toMessageId":3,"tokens":{"input":10,"output":20}},
              {"id":"e2","timestamp":"2026-01-02T00:00:01.000Z","model":"gpt-5","tokens":{"total":345}},
              {"id":"e3","timestamp":"bad","model":"gpt-5","tokens":{"total":5}}]}}"#;
        std::fs::write(d.join("threads/a.json"), t).unwrap();
        std::fs::write(d.join("threads/bad.json"), "{oops").unwrap();
        let mut out = HarnessData::default();
        scan(&[d.clone()], &mut out);
        assert_eq!(out.entries.len(), 2);
        let a = &out.entries[0];
        assert_eq!((a.usage.input, a.usage.output, a.usage.cache_write, a.usage.cache_read), (10, 20, 7, 90));
        assert_eq!(a.session, "T-1");
        assert_eq!(out.entries[1].usage.output, 345);
        assert_eq!(out.files, 2);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn message_usage_without_ledger() {
        let d = fixture("msgs");
        let t = r#"{"id":"T-2","messages":[{"role":"assistant","usage":{"model":"claude-haiku-4-5","inputTokens":10,"outputTokens":178,"cacheCreationInputTokens":986,"cacheReadInputTokens":11372,"timestamp":"2026-01-19T11:42:10.652Z"}},
            {"role":"assistant","usage":{"inputTokens":1}}]}"#;
        std::fs::write(d.join("threads/b.json"), t).unwrap();
        let mut out = HarnessData::default();
        scan(&[d.clone()], &mut out);
        assert_eq!(out.entries.len(), 1);
        assert_eq!(out.entries[0].usage.cache_read, 11372);
        assert_eq!(out.entries[0].usage.output, 178);
        let _ = std::fs::remove_dir_all(d);
    }
}
