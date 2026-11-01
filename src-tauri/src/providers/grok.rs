//! Grok CLI adapter: `~/.grok/sessions/<project>/<session-id>/updates.jsonl` (+ optional
//! sibling `summary.json`).
//!
//! Each line is `{timestamp, params: {sessionId, update: {sessionUpdate, usage}, _meta}}`.
//! Only `sessionUpdate == "turn_completed"` lines with a `usage` block are billable; `usage`
//! carries session totals and, usually, a `modelUsage` map of per-model breakdowns (used when
//! present). `inputTokens` includes `cachedReadTokens` (and, per ccusage, `cacheCreationTokens`
//! as a sibling subset of it, never observed non-zero). `GROK_HOME` overrides `~/.grok`.
use crate::model::{home_dir, project_name, Entry, HarnessData, Usage};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub fn default_roots() -> Vec<PathBuf> {
    let root = std::env::var("GROK_HOME")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".grok"));
    vec![root.join("sessions")]
}

/// 1 cost tick is 1e-10 USD (fixed-point `costUsdTicks`).
const TICKS_PER_USD: f64 = 1e10;

fn num(v: Option<&Value>) -> u64 {
    v.and_then(Value::as_f64).filter(|f| f.is_finite()).map(|f| f.max(0.0) as u64).unwrap_or(0)
}

fn str_of<'a>(m: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    m.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// Minimal percent-decoder for session directory names (`%3A` -> `:`, `%5C` -> `\`, ...).
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Splits `inputTokens` into (fresh, cache_read, cache_write); both cache parts are subsets
/// of `inputTokens`, not additional totals (see module docs).
fn split_input(input: u64, cache_read: u64, cache_write: u64) -> (u64, u64, u64) {
    let cache_read = cache_read.min(input);
    let fresh = input - cache_read;
    let cache_write = cache_write.min(fresh);
    (fresh - cache_write, cache_read, cache_write)
}

struct Row {
    model: String,
    usage: Usage,
    cost: Option<f64>,
}

fn rows_from_usage(usage: &Map<String, Value>, default_model: Option<&str>) -> Vec<Row> {
    let row = |m: &Map<String, Value>, model: String| -> Option<Row> {
        let reasoning = num(m.get("reasoningTokens"));
        let (fresh, cache_read, cache_write) = split_input(
            num(m.get("inputTokens")),
            num(m.get("cachedReadTokens")),
            num(m.get("cacheCreationTokens")),
        );
        let output = num(m.get("outputTokens")) + reasoning;
        let u = Usage {
            input: fresh,
            output,
            cache_read,
            cache_write,
            cache_write_1h: 0,
            reasoning,
        };
        if u.is_zero() {
            return None;
        }
        let ticks = num(m.get("costUsdTicks"));
        let cost = (ticks > 0).then(|| ticks as f64 / TICKS_PER_USD);
        Some(Row { model, usage: u, cost })
    };

    if let Some(models) = usage.get("modelUsage").and_then(Value::as_object) {
        if !models.is_empty() {
            return models
                .iter()
                .filter_map(|(name, v)| v.as_object().and_then(|m| row(m, name.clone())))
                .collect();
        }
    }
    let model = default_model.unwrap_or("unknown").to_string();
    row(usage, model).into_iter().collect()
}

fn session_meta(session_dir: &Path, summary: &Path) -> (String, String, Option<String>) {
    let mut session_id = session_dir.file_name().and_then(|s| s.to_str()).unwrap_or("unknown").to_string();
    let mut project = session_dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .map(|s| project_name(&url_decode(s)))
        .unwrap_or_else(|| "unknown".to_string());
    let mut default_model = None;
    if let Ok(text) = std::fs::read_to_string(summary) {
        if let Ok(Value::Object(rec)) = serde_json::from_str::<Value>(&text) {
            if let Some(info) = rec.get("info").and_then(Value::as_object) {
                if let Some(id) = str_of(info, "id") {
                    session_id = id.to_string();
                }
                let cwd = str_of(info, "cwd").or_else(|| str_of(&rec, "git_root_dir"));
                if let Some(cwd) = cwd {
                    project = project_name(cwd);
                }
            }
            default_model = str_of(&rec, "current_model_id").map(str::to_string);
        }
    }
    (session_id, project, default_model)
}

fn parse_file(path: &Path, data: &mut HarnessData) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let session_dir = path.parent().unwrap_or(path);
    let (mut session, project, default_model) = session_meta(session_dir, &session_dir.join("summary.json"));
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Ok(Value::Object(rec)) = serde_json::from_str::<Value>(line) else { continue };
        let Some(params) = rec.get("params").and_then(Value::as_object) else { continue };
        let Some(update) = params.get("update").and_then(Value::as_object) else { continue };
        if str_of(update, "sessionUpdate") != Some("turn_completed") {
            continue;
        }
        let Some(usage) = update.get("usage").and_then(Value::as_object) else { continue };
        let ts = rec.get("timestamp").and_then(Value::as_i64).unwrap_or(0);
        if ts <= 0 {
            continue;
        }
        if let Some(sid) = str_of(params, "sessionId") {
            session = sid.to_string();
        }
        let event_id = params.get("_meta").and_then(Value::as_object).and_then(|m| str_of(m, "eventId"));
        for row in rows_from_usage(usage, default_model.as_deref()) {
            let key = event_id
                .map(str::to_string)
                .unwrap_or_else(|| format!("{session}|{ts}|{}", row.model));
            if !seen.insert(key) {
                continue;
            }
            entries.push(Entry {
                ts,
                model: row.model,
                provider: Some("xai".to_string()),
                session: session.clone(),
                project: project.clone(),
                usage: row.usage,
                stored_cost: row.cost,
            });
        }
    }
    data.entries.extend(entries);
    Ok(())
}

pub fn scan(roots: &[PathBuf], out: &mut HarnessData) {
    let mut files = Vec::new();
    for root in roots.iter().filter(|r| r.exists()) {
        crate::scan::walk(root, &mut |p: &Path| {
            if p.file_name().and_then(|n| n.to_str()) == Some("updates.jsonl") {
                files.push(p.to_path_buf());
            }
        });
    }
    files.sort();
    files.dedup();
    for f in files {
        out.files += 1;
        if let Err(e) = parse_file(&f, out) {
            out.errors.push(format!("grok: {}: {e}", f.display()));
        }
    }
    // No Prompt is a user-typed message and we have no verified handle in this format, so
    // prompts are intentionally left empty; see model.rs normalization rules.
    let _ = &out.prompts;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ts-grok-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sessions").join("C%3A%5Cproj").join("sess-1")).unwrap();
        d
    }

    #[test]
    fn parses_turn_completed_with_model_usage_and_dedups_by_event_id() {
        let d = fixture("basic");
        let line = r#"{"timestamp":1750000000000,"params":{"sessionId":"sess-1","update":{"sessionUpdate":"turn_completed","usage":{"modelUsage":{"grok-4.5-build":{"inputTokens":18444,"outputTokens":130,"cachedReadTokens":11264,"reasoningTokens":73,"costUsdTicks":185192000}}}},"_meta":{"eventId":"evt-cost"}}}"#;
        std::fs::write(
            d.join("sessions/C%3A%5Cproj/sess-1/updates.jsonl"),
            [line, line, "not json", r#"{"timestamp":1,"params":{"update":{"sessionUpdate":"tool_call"}}}"#].join("\n"),
        )
        .unwrap();
        let mut out = HarnessData::default();
        scan(&[d.join("sessions")], &mut out);
        assert_eq!(out.entries.len(), 1);
        let e = &out.entries[0];
        assert_eq!(e.session, "sess-1");
        assert_eq!(e.project, "proj");
        assert_eq!(e.model, "grok-4.5-build");
        assert_eq!(e.provider, Some("xai".to_string()));
        assert_eq!(e.usage.input, 18444 - 11264);
        assert_eq!(e.usage.cache_read, 11264);
        assert_eq!(e.usage.output, 130 + 73);
        assert_eq!(e.usage.reasoning, 73);
        assert_eq!(e.stored_cost, Some(0.0185192));
        assert_eq!(out.files, 1);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn summary_json_overrides_session_id_and_project() {
        let d = fixture("summary");
        let line = r#"{"timestamp":1750000000000,"params":{"update":{"sessionUpdate":"turn_completed","usage":{"inputTokens":10,"outputTokens":2,"cachedReadTokens":0,"reasoningTokens":0}}},"_meta":{"eventId":"evt-u"}}"#;
        std::fs::write(d.join("sessions/C%3A%5Cproj/sess-1/updates.jsonl"), line).unwrap();
        std::fs::write(
            d.join("sessions/C%3A%5Cproj/sess-1/summary.json"),
            r#"{"info":{"id":"real-session","cwd":"D:\\devs\\myrepo"},"current_model_id":"grok-4.5"}"#,
        )
        .unwrap();
        let mut out = HarnessData::default();
        scan(&[d.join("sessions")], &mut out);
        assert_eq!(out.entries.len(), 1);
        assert_eq!(out.entries[0].session, "real-session");
        assert_eq!(out.entries[0].project, "myrepo");
        assert_eq!(out.entries[0].model, "grok-4.5");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn skips_non_turn_completed_and_zero_usage() {
        let d = fixture("zero");
        let lines = [
            r#"{"timestamp":1,"params":{"update":{"sessionUpdate":"tool_call","usage":{"inputTokens":5,"outputTokens":5}}}}"#,
            r#"{"timestamp":2,"params":{"update":{"sessionUpdate":"turn_completed","usage":{"inputTokens":0,"outputTokens":0,"cachedReadTokens":0,"reasoningTokens":0}}}}"#,
        ];
        std::fs::write(d.join("sessions/C%3A%5Cproj/sess-1/updates.jsonl"), lines.join("\n")).unwrap();
        let mut out = HarnessData::default();
        scan(&[d.join("sessions")], &mut out);
        assert_eq!(out.entries.len(), 0);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    #[ignore]
    fn real_data_sanity() {
        let roots = default_roots();
        let mut out = HarnessData::default();
        scan(&roots, &mut out);
        println!(
            "grok: roots={:?} files={} entries={} errors={:?}",
            roots, out.files, out.entries.len(), out.errors
        );
    }
}
