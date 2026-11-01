//! Pi coding agent adapter: `~/.pi/agent/sessions/<project>/<ts>_<uuid>.jsonl` session logs.
//!
//! Each line is a tree node `{type, id, parentId, timestamp, message?}`. A `session` header
//! carries `cwd`; `message` nodes with `message.role == "assistant"` carry `message.usage`
//! (`input` is already fresh, i.e. excludes `cacheRead`/`cacheWrite`) and `message.provider`/
//! `message.model`. `message.role == "user"` nodes are user prompts.
//! `PI_AGENT_DIR` (comma-separated) overrides the default sessions location.
use crate::model::{home_dir, parse_ts, project_name, Entry, HarnessData, Prompt, Usage};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

pub fn default_roots() -> Vec<PathBuf> {
    if let Ok(v) = std::env::var("PI_AGENT_DIR") {
        return v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from).collect();
    }
    vec![home_dir().join(".pi").join("agent").join("sessions")]
}

fn num(v: Option<&Value>) -> u64 {
    v.and_then(Value::as_f64).filter(|f| f.is_finite()).map(|f| f.max(0.0) as u64).unwrap_or(0)
}

fn str_of<'a>(m: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    m.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// Known pi provider ids that map directly onto pricing provider hints; local/unknown
/// providers (e.g. "local-lfm3") are left as `None` so pricing falls back to model-name lookup.
fn provider_hint(p: &str) -> Option<String> {
    match p {
        "anthropic" | "openai" | "google" | "deepseek" | "moonshot" | "openrouter" | "xai" => {
            Some(p.to_string())
        }
        "grok" => Some("xai".to_string()),
        _ => None,
    }
}

fn usage_from(m: &Map<String, Value>) -> Option<(Usage, Option<f64>)> {
    let u = m.get("usage")?.as_object()?;
    let reasoning = num(u.get("reasoning"));
    let usage = Usage {
        input: num(u.get("input")),
        output: num(u.get("output")) + reasoning,
        cache_read: num(u.get("cacheRead")),
        cache_write: num(u.get("cacheWrite")),
        cache_write_1h: 0,
        reasoning,
    };
    if usage.is_zero() {
        return None;
    }
    let cost = u
        .get("cost")
        .and_then(Value::as_object)
        .and_then(|c| c.get("total"))
        .and_then(Value::as_f64)
        .filter(|c| *c > 0.0);
    Some((usage, cost))
}

fn parse_file(path: &Path, data: &mut HarnessData) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown");
    let mut session = stem.split_once('_').map_or(stem, |(_, id)| id).to_string();
    // Default project from the <sessions>/<project>/<file> directory layout; overridden by the
    // session header's `cwd` when present.
    let mut project = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .map(project_name)
        .unwrap_or_else(|| "unknown".to_string());
    let mut entries = Vec::new();
    let mut prompts = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Ok(Value::Object(rec)) = serde_json::from_str::<Value>(line) else { continue };
        let ty = str_of(&rec, "type");
        if ty == Some("session") {
            if let Some(id) = str_of(&rec, "id") {
                session = id.to_string();
            }
            if let Some(cwd) = str_of(&rec, "cwd") {
                project = project_name(cwd);
            }
            continue;
        }
        if ty != Some("message") {
            continue;
        }
        let Some(msg) = rec.get("message").and_then(Value::as_object) else { continue };
        let Some(ts) = str_of(&rec, "timestamp").and_then(parse_ts) else { continue };
        match str_of(msg, "role") {
            Some("assistant") => {
                let Some(model) = str_of(msg, "model") else { continue };
                let Some((usage, stored_cost)) = usage_from(msg) else { continue };
                let provider = str_of(msg, "provider").and_then(provider_hint);
                entries.push(Entry {
                    ts,
                    model: model.to_string(),
                    provider,
                    session: session.clone(),
                    project: project.clone(),
                    usage,
                    stored_cost,
                });
            }
            Some("user") => prompts.push(Prompt { ts, session: session.clone() }),
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
            if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                files.push(p.to_path_buf());
            }
        });
    }
    files.sort();
    files.dedup();
    for f in files {
        out.files += 1;
        if let Err(e) = parse_file(&f, out) {
            out.errors.push(format!("pi: {}: {e}", f.display()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ts-pi-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("--proj--")).unwrap();
        d
    }

    #[test]
    fn parses_session_assistant_and_user_messages() {
        let d = fixture("basic");
        let lines = [
            r#"{"type":"session","version":3,"id":"s1","timestamp":"2026-10-28T10:30:21.047Z","cwd":"D:\\devs\\myproj"}"#,
            r#"{"type":"model_change","id":"a","parentId":null,"timestamp":"2026-10-28T10:30:21.133Z","provider":"deepseek","modelId":"deepseek-v4-pro"}"#,
            r#"{"type":"message","id":"b","parentId":"a","timestamp":"2026-10-28T10:30:42.000Z","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}"#,
            r#"{"type":"message","id":"c","parentId":"b","timestamp":"2026-10-28T10:30:46.000Z","message":{"role":"assistant","provider":"deepseek","model":"deepseek-v4-pro","usage":{"input":1231,"output":44,"cacheRead":693,"cacheWrite":0,"reasoning":10,"totalTokens":1978,"cost":{"total":0.002}}}}"#,
            "not json",
        ];
        std::fs::write(d.join("--proj--/2026-10-28T10-30-21-047Z_01a1820f.jsonl"), lines.join("\n")).unwrap();
        let mut out = HarnessData::default();
        scan(&[d.clone()], &mut out);
        assert_eq!(out.entries.len(), 1);
        let e = &out.entries[0];
        assert_eq!(e.session, "s1");
        assert_eq!(e.project, "myproj");
        assert_eq!(e.model, "deepseek-v4-pro");
        assert_eq!(e.provider, Some("deepseek".to_string()));
        assert_eq!(e.usage.input, 1231);
        assert_eq!(e.usage.cache_read, 693);
        assert_eq!(e.usage.output, 44 + 10);
        assert_eq!(e.usage.reasoning, 10);
        assert_eq!(e.stored_cost, Some(0.002));
        assert_eq!(out.prompts.len(), 1);
        assert_eq!(out.files, 1);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn skips_zero_usage_and_unknown_provider_hint() {
        let d = fixture("zero");
        let lines = [
            r#"{"type":"message","id":"c","parentId":null,"timestamp":"2026-10-28T10:30:46.000Z","message":{"role":"assistant","provider":"local-lfm3","model":"lfm3","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"reasoning":0,"totalTokens":0}}}"#,
            r#"{"type":"message","id":"d","parentId":"c","timestamp":"2026-10-28T10:30:47.000Z","message":{"role":"assistant","provider":"local-lfm3","model":"lfm3","usage":{"input":5,"output":1,"cacheRead":0,"cacheWrite":0,"reasoning":0,"totalTokens":6}}}"#,
        ];
        std::fs::write(d.join("--proj--/f_s2.jsonl"), lines.join("\n")).unwrap();
        let mut out = HarnessData::default();
        scan(&[d.clone()], &mut out);
        assert_eq!(out.entries.len(), 1);
        assert_eq!(out.entries[0].provider, None);
        assert_eq!(out.entries[0].session, "s2");
        // No session header in this fixture, so the fallback project is the raw
        // sessions-directory name (pi mangles cwd into it); the header's `cwd` is
        // the normal source of a clean project name (see the other test).
        assert_eq!(out.entries[0].project, "--proj--");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    #[ignore]
    fn real_data_sanity() {
        let roots = default_roots();
        let mut out = HarnessData::default();
        scan(&roots, &mut out);
        println!(
            "pi: roots={:?} files={} entries={} prompts={} errors={:?}",
            roots, out.files, out.entries.len(), out.prompts.len(), out.errors
        );
    }
}
