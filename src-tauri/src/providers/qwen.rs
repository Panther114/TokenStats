//! Qwen Code adapter: `~/.qwen/projects/<project>/chats/*.jsonl`.
//!
//! Assistant records carry Gemini-style `usageMetadata` (`promptTokenCount` includes the cached
//! part). `QWEN_DATA_DIR` (comma-separated) overrides the default `~/.qwen` root.
use crate::model::{home_dir, parse_ts, Entry, HarnessData, Prompt, Usage};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub fn default_roots() -> Vec<PathBuf> {
    if let Ok(v) = std::env::var("QWEN_DATA_DIR") {
        return v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from).collect();
    }
    vec![home_dir().join(".qwen")]
}

fn num(v: &Value, key: &str) -> u64 {
    match v.get(key).and_then(Value::as_f64) {
        Some(f) if f.is_finite() => f.max(0.0).trunc() as u64,
        _ => 0,
    }
}

/// `<root>/projects/<project>/chats/<file>.jsonl` -> project directory name.
fn project_of(path: &Path) -> Option<String> {
    let chats = path.parent()?;
    if chats.file_name()?.to_str()? != "chats" {
        return None;
    }
    let project = chats.parent()?;
    if project.parent()?.file_name()?.to_str()? != "projects" {
        return None;
    }
    Some(project.file_name()?.to_string_lossy().into_owned())
}

fn parse_file(path: &Path, seen: &mut HashSet<String>, out: &mut HarnessData) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let fallback_ts = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let project = project_of(path).unwrap_or_else(|| "unknown".to_string());
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown");
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Ok(rec) = serde_json::from_str::<Value>(line) else { continue };
        let ts = rec.get("timestamp").and_then(Value::as_str).and_then(parse_ts);
        let session = rec
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{project}-{stem}"));
        match rec.get("type").and_then(Value::as_str) {
            Some("user") => {
                if let Some(ts) = ts {
                    out.prompts.push(Prompt { ts, session });
                }
            }
            Some("assistant") => {
                let Some(u) = rec.get("usageMetadata").filter(|u| u.is_object()) else { continue };
                let prompt = num(u, "promptTokenCount");
                let cached = num(u, "cachedContentTokenCount").min(prompt);
                let candidates = num(u, "candidatesTokenCount");
                let thoughts = num(u, "thoughtsTokenCount");
                let total = num(u, "totalTokenCount");
                // Tokens present in the reported total but in no other field count as output.
                let missing = total.saturating_sub(prompt + candidates + thoughts);
                let usage = Usage {
                    input: prompt - cached,
                    output: candidates + thoughts + missing,
                    cache_read: cached,
                    cache_write: 0,
                    cache_write_1h: 0,
                    reasoning: thoughts,
                };
                if usage.is_zero() {
                    continue;
                }
                let ts = ts.unwrap_or(fallback_ts);
                let model = rec
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                    .unwrap_or("unknown")
                    .to_string();
                let key = format!(
                    "{session}|{ts}|{model}|{}|{}|{}|{missing}",
                    usage.input, usage.output, usage.cache_read
                );
                if !seen.insert(key) {
                    continue;
                }
                out.entries.push(Entry {
                    ts,
                    model,
                    provider: None,
                    session,
                    project: project.clone(),
                    usage,
                    stored_cost: None,
                });
            }
            _ => {}
        }
    }
    Ok(())
}

pub fn scan(roots: &[PathBuf], out: &mut HarnessData) {
    let mut files = Vec::new();
    for root in roots.iter().filter(|r| r.exists()) {
        let projects = root.join("projects");
        crate::scan::walk(&projects, &mut |p: &Path| {
            if p.extension().and_then(|e| e.to_str()) == Some("jsonl") && project_of(p).is_some() {
                files.push(p.to_path_buf());
            }
        });
    }
    files.sort();
    files.dedup();
    let mut seen = HashSet::new();
    for f in files {
        out.files += 1;
        if let Err(e) = parse_file(&f, &mut seen, out) {
            out.errors.push(format!("qwen: {}: {e}", f.display()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usage_metadata_and_dedups() {
        let d = std::env::temp_dir().join(format!("ts-qwen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let chats = d.join("projects").join("proj-a").join("chats");
        std::fs::create_dir_all(&chats).unwrap();
        let line = r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-05-01T00:00:02.000Z","model":"qwen3-coder-plus","usageMetadata":{"promptTokenCount":1000,"candidatesTokenCount":50,"thoughtsTokenCount":10,"cachedContentTokenCount":400,"totalTokenCount":1060}}"#;
        let lines = [
            r#"{"type":"user","sessionId":"s1","timestamp":"2026-05-01T00:00:01.000Z"}"#,
            line,
            line,
            "garbage",
        ];
        std::fs::write(chats.join("c.jsonl"), lines.join("\n")).unwrap();
        // A file outside `chats/` must be ignored.
        std::fs::write(d.join("projects").join("proj-a").join("x.jsonl"), line).unwrap();
        let mut out = HarnessData::default();
        scan(&[d.clone()], &mut out);
        assert_eq!(out.files, 1);
        assert_eq!(out.entries.len(), 1);
        let e = &out.entries[0];
        assert_eq!((e.usage.input, e.usage.cache_read), (600, 400));
        assert_eq!((e.usage.output, e.usage.reasoning), (60, 10));
        assert_eq!((e.project.as_str(), e.session.as_str()), ("proj-a", "s1"));
        assert_eq!(out.prompts.len(), 1);
        let _ = std::fs::remove_dir_all(d);
    }
}
