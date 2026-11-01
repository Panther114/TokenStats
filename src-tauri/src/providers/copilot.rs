//! GitHub Copilot CLI adapter: `~/.copilot/session-state/<session-id>/events.jsonl`.
//!
//! Each line is `{type, id, timestamp, parentId, data}`. `session.start` carries
//! `data.context.cwd`; `session.usage_record` carries one billable call's
//! `data.usage` (`inputTokens`/`outputTokens`/`cacheReadTokens`/`cacheWriteTokens`/
//! `reasoningTokens`/`model`), deduplicated by event `id`; `user.message` is a user prompt.
//! `COPILOT_HOME` overrides the default `~/.copilot` root.
use crate::model::{home_dir, parse_ts, project_name, Entry, HarnessData, Prompt, Usage};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub fn default_roots() -> Vec<PathBuf> {
    let root = std::env::var("COPILOT_HOME")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".copilot"));
    vec![root.join("session-state")]
}

fn num(v: Option<&Value>) -> u64 {
    v.and_then(Value::as_f64).filter(|f| f.is_finite()).map(|f| f.max(0.0) as u64).unwrap_or(0)
}

fn str_of<'a>(m: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    m.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn usage_from(u: &Map<String, Value>) -> Option<Usage> {
    let reasoning = num(u.get("reasoningTokens"));
    let usage = Usage {
        input: num(u.get("inputTokens")),
        output: num(u.get("outputTokens")) + reasoning,
        cache_read: num(u.get("cacheReadTokens")),
        cache_write: num(u.get("cacheWriteTokens")),
        cache_write_1h: 0,
        reasoning,
    };
    (!usage.is_zero()).then_some(usage)
}

fn parse_file(path: &Path, data: &mut HarnessData) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let session = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let mut project = "unknown".to_string();
    let mut entries = Vec::new();
    let mut prompts = Vec::new();
    let mut seen_ids = HashSet::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Ok(Value::Object(rec)) = serde_json::from_str::<Value>(line) else { continue };
        let Some(ty) = str_of(&rec, "type") else { continue };
        let Some(obj) = rec.get("data").and_then(Value::as_object) else { continue };
        match ty {
            "session.start" => {
                if let Some(cwd) = obj.get("context").and_then(Value::as_object).and_then(|c| str_of(c, "cwd")) {
                    project = project_name(cwd);
                }
            }
            "session.usage_record" => {
                if let Some(id) = str_of(&rec, "id") {
                    if !seen_ids.insert(id.to_string()) {
                        continue;
                    }
                }
                let Some(ts) = str_of(&rec, "timestamp").and_then(parse_ts) else { continue };
                let Some(usage_obj) = obj.get("usage").and_then(Value::as_object) else { continue };
                let Some(model) = str_of(usage_obj, "model") else { continue };
                let Some(usage) = usage_from(usage_obj) else { continue };
                entries.push(Entry {
                    ts,
                    model: model.to_string(),
                    provider: None,
                    session: session.clone(),
                    project: project.clone(),
                    usage,
                    stored_cost: None,
                });
            }
            "user.message" => {
                if let Some(ts) = str_of(&rec, "timestamp").and_then(parse_ts) {
                    prompts.push(Prompt { ts, session: session.clone() });
                }
            }
            _ => {}
        }
    }
    data.entries.extend(entries);
    data.prompts.extend(prompts);
    Ok(())
}

pub fn scan(roots: &[PathBuf], out: &mut HarnessData) {
    let mut files = Vec::new();
    for root in roots.iter().filter(|r| r.exists()) {
        crate::scan::walk(root, &mut |p: &Path| {
            if p.file_name().and_then(|n| n.to_str()) == Some("events.jsonl") {
                files.push(p.to_path_buf());
            }
        });
    }
    files.sort();
    files.dedup();
    for f in files {
        out.files += 1;
        if let Err(e) = parse_file(&f, out) {
            out.errors.push(format!("copilot: {}: {e}", f.display()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ts-copilot-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("session-state").join("sess-1")).unwrap();
        d
    }

    #[test]
    fn parses_usage_records_and_dedups_by_id() {
        let d = fixture("basic");
        let lines = [
            r#"{"type":"session.start","id":"a","timestamp":"2026-10-25T07:38:20.095Z","data":{"context":{"cwd":"d:\\proj\\myrepo"}}}"#,
            r#"{"type":"user.message","id":"b","timestamp":"2026-10-25T07:38:24.000Z","data":{"content":"hi"}}"#,
            r#"{"type":"session.usage_record","id":"u1","timestamp":"2026-10-25T07:38:28.437Z","data":{"usage":{"model":"gpt-5.6-luna","inputTokens":15979,"outputTokens":50,"cacheReadTokens":0,"cacheWriteTokens":15976,"reasoningTokens":25,"cost":1.0}}}"#,
            r#"{"type":"session.usage_record","id":"u1","timestamp":"2026-10-25T07:38:28.437Z","data":{"usage":{"model":"gpt-5.6-luna","inputTokens":15979,"outputTokens":50,"cacheReadTokens":0,"cacheWriteTokens":15976,"reasoningTokens":25,"cost":1.0}}}"#,
            "not json",
        ];
        std::fs::write(d.join("session-state/sess-1/events.jsonl"), lines.join("\n")).unwrap();
        let mut out = HarnessData::default();
        scan(&[d.join("session-state")], &mut out);
        assert_eq!(out.entries.len(), 1);
        let e = &out.entries[0];
        assert_eq!(e.session, "sess-1");
        assert_eq!(e.project, "myrepo");
        assert_eq!(e.model, "gpt-5.6-luna");
        assert_eq!(e.usage.input, 15979);
        assert_eq!(e.usage.cache_write, 15976);
        assert_eq!(e.usage.output, 50 + 25);
        assert_eq!(e.usage.reasoning, 25);
        assert_eq!(out.prompts.len(), 1);
        assert_eq!(out.files, 1);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn skips_zero_usage_records() {
        let d = fixture("zero");
        let lines = [
            r#"{"type":"session.usage_record","id":"u2","timestamp":"2026-10-25T07:38:28.437Z","data":{"usage":{"model":"gpt-5.6-luna","inputTokens":0,"outputTokens":0,"cacheReadTokens":0,"cacheWriteTokens":0,"reasoningTokens":0}}}"#,
        ];
        std::fs::write(d.join("session-state/sess-1/events.jsonl"), lines.join("\n")).unwrap();
        let mut out = HarnessData::default();
        scan(&[d.join("session-state")], &mut out);
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
            "copilot: roots={:?} files={} entries={} prompts={} errors={:?}",
            roots, out.files, out.entries.len(), out.prompts.len(), out.errors
        );
    }
}
