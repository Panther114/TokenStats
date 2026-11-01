//! Kimi CLI adapter: `~/.kimi/sessions/**/wire.jsonl` (and `~/.kimi-code`).
//!
//! Old format: `StatusUpdate` messages with `token_usage` (timestamp in seconds, model from
//! `config.json`). New Kimi Code format: `usage.record` lines with `usageScope: "turn"`.
//! `KIMI_DATA_DIR` (comma-separated) overrides the default roots.
use crate::model::{home_dir, Entry, HarnessData, Prompt, Usage};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const DEFAULT_MODEL: &str = "kimi-for-coding";

pub fn default_roots() -> Vec<PathBuf> {
    if let Ok(v) = std::env::var("KIMI_DATA_DIR") {
        return v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from).collect();
    }
    let h = home_dir();
    vec![h.join(".kimi"), h.join(".kimi-code")]
}

fn num(v: &Value, keys: &[&str]) -> u64 {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(Value::as_f64))
        .filter(|f| f.is_finite())
        .map(|f| f.max(0.0).trunc() as u64)
        .unwrap_or(0)
}

/// Wire files sit at `sessions/<group>/<session>/wire.jsonl` (3 components) or
/// `sessions/<ws>/<session>/agents/<agent>/wire.jsonl` (5). Returns (session id, is_agent_layout).
fn layout(sessions: &Path, file: &Path) -> Option<(String, bool)> {
    if file.file_name()?.to_str()? != "wire.jsonl" {
        return None;
    }
    let rel: Vec<_> = file.strip_prefix(sessions).ok()?.components().collect();
    let session = |i: usize| rel[i].as_os_str().to_string_lossy().into_owned();
    match rel.len() {
        3 => Some((session(1), false)),
        5 if rel[2].as_os_str() == "agents" => Some((session(1), true)),
        _ => None,
    }
}

fn config_model(root: &Path) -> String {
    std::fs::read_to_string(root.join("config.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("model").and_then(Value::as_str).map(str::trim).map(str::to_string))
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

fn scan_file(
    path: &Path,
    session: &str,
    cfg_model: &str,
    seen: &mut HashSet<String>,
    out: &mut HarnessData,
) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let fallback_ts = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Ok(rec) = serde_json::from_str::<Value>(line) else { continue };
        let secs_ts = rec
            .get("timestamp")
            .and_then(Value::as_f64)
            .filter(|f| f.is_finite())
            .map(|f| (f * 1000.0) as i64);
        let (ts, model, msg_id, u): (i64, String, String, Usage);
        match rec.get("type").and_then(Value::as_str) {
            Some("metadata") => continue,
            Some("usage.record") => {
                if rec.get("usageScope").and_then(Value::as_str) != Some("turn") {
                    continue; // session-scoped records are cumulative totals
                }
                let Some(uv) = rec.get("usage") else { continue };
                ts = rec.get("time").and_then(Value::as_i64).unwrap_or(fallback_ts);
                let m = rec.get("model").and_then(Value::as_str).filter(|m| !m.is_empty());
                let m = m.unwrap_or(DEFAULT_MODEL);
                model = m.strip_prefix("kimi-code/").unwrap_or(m).to_string();
                msg_id = String::new();
                u = Usage {
                    input: num(uv, &["inputOther"]),
                    output: num(uv, &["output"]),
                    cache_read: num(uv, &["inputCacheRead"]),
                    cache_write: num(uv, &["inputCacheCreation"]),
                    ..Usage::default()
                };
            }
            _ => {
                let Some(m) = rec.get("message") else { continue };
                let payload = m.get("payload");
                match m.get("type").and_then(Value::as_str) {
                    Some("TurnBegin") => {
                        out.prompts.push(Prompt { ts: secs_ts.unwrap_or(fallback_ts), session: session.to_string() });
                        continue;
                    }
                    Some("StatusUpdate") => {}
                    _ => continue,
                }
                let Some(tu) = payload.and_then(|p| p.get("token_usage")) else { continue };
                ts = secs_ts.unwrap_or(fallback_ts);
                model = cfg_model.to_string();
                msg_id = payload
                    .and_then(|p| p.get("message_id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let mut usage = Usage {
                    input: num(tu, &["input_other"]),
                    output: num(tu, &["output"]),
                    cache_read: num(tu, &["input_cache_read"]),
                    cache_write: num(tu, &["input_cache_creation"]),
                    ..Usage::default()
                };
                let total = num(tu, &["total"]);
                let known = usage.input + usage.output + usage.cache_read + usage.cache_write;
                usage.output += total.saturating_sub(known);
                u = usage;
            }
        }
        if u.is_zero() {
            continue;
        }
        let key = format!(
            "{session}|{msg_id}|{ts}|{model}|{}|{}|{}|{}",
            u.input, u.output, u.cache_write, u.cache_read
        );
        if !seen.insert(key) {
            continue;
        }
        out.entries.push(Entry {
            ts,
            model,
            provider: Some("moonshot".to_string()),
            session: session.to_string(),
            project: "unknown".to_string(),
            usage: u,
            stored_cost: None,
        });
    }
    Ok(())
}

pub fn scan(roots: &[PathBuf], out: &mut HarnessData) {
    let mut seen = HashSet::new();
    let mut done = HashSet::new();
    for root in roots.iter().filter(|r| r.exists()) {
        let sessions = root.join("sessions");
        let model = config_model(root);
        let mut files = Vec::new();
        crate::scan::walk(&sessions, &mut |p: &Path| {
            if let Some((sid, _)) = layout(&sessions, p) {
                files.push((p.to_path_buf(), sid));
            }
        });
        files.sort();
        for (f, sid) in files {
            if !done.insert(f.clone()) {
                continue;
            }
            out.files += 1;
            if let Err(e) = scan_file(&f, &sid, &model, &mut seen, out) {
                out.errors.push(format!("kimi: {}: {e}", f.display()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_formats() {
        let d = std::env::temp_dir().join(format!("ts-kimi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let old = d.join("sessions/g/sess-a");
        let new = d.join("sessions/ws/sess-b/agents/a1");
        let skip = d.join("sessions/x/y/z/w");
        for p in [&old, &new, &skip] {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(d.join("config.json"), r#"{"model":"kimi-k2"}"#).unwrap();
        let status = r#"{"timestamp":1770983427.123,"message":{"type":"StatusUpdate","payload":{"token_usage":{"input_other":100,"output":50,"input_cache_read":10,"input_cache_creation":20},"message_id":"m1"}}}"#;
        let old_lines = [
            r#"{"type":"metadata","protocol_version":"1.3"}"#,
            r#"{"timestamp":1770983426.4,"message":{"type":"TurnBegin","payload":{}}}"#,
            status,
            status,
            "bad",
        ];
        std::fs::write(old.join("wire.jsonl"), old_lines.join("\n")).unwrap();
        let new_lines = [
            r#"{"type":"usage.record","model":"kimi-code/kimi-for-coding","usage":{"inputOther":3064,"output":76,"inputCacheRead":14848,"inputCacheCreation":0},"usageScope":"turn","time":1782113184943}"#,
            r#"{"type":"usage.record","model":"kimi-code/kimi-for-coding","usage":{"inputOther":5000,"output":200},"usageScope":"session","time":1782113185000}"#,
        ];
        std::fs::write(new.join("wire.jsonl"), new_lines.join("\n")).unwrap();
        std::fs::write(skip.join("wire.jsonl"), status).unwrap();
        let mut out = HarnessData::default();
        scan(&[d.clone()], &mut out);
        assert_eq!(out.files, 2);
        assert_eq!(out.entries.len(), 2);
        let a = out.entries.iter().find(|e| e.session == "sess-a").unwrap();
        assert_eq!((a.model.as_str(), a.usage.input, a.usage.output), ("kimi-k2", 100, 50));
        assert_eq!((a.usage.cache_read, a.usage.cache_write, a.ts), (10, 20, 1_770_983_427_123));
        let b = out.entries.iter().find(|e| e.session == "sess-b").unwrap();
        assert_eq!((b.model.as_str(), b.usage.input, b.usage.cache_read), ("kimi-for-coding", 3064, 14848));
        assert_eq!(out.prompts.len(), 1);
        let _ = std::fs::remove_dir_all(d);
    }
}
