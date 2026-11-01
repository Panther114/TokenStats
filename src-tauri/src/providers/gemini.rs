//! Gemini CLI adapter: `~/.gemini/tmp/<project>/chats/*.json|jsonl` session logs.
//!
//! Message records of type `gemini` carry `tokens: {input, output, cached, thoughts, tool, total}`.
//! `GEMINI_DATA_DIR` (comma-separated) overrides the default location.
use crate::model::{home_dir, parse_ts, Entry, HarnessData, Prompt, Usage};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn default_roots() -> Vec<PathBuf> {
    if let Ok(v) = std::env::var("GEMINI_DATA_DIR") {
        return v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from).collect();
    }
    vec![home_dir().join(".gemini").join("tmp")]
}

#[derive(Clone, Copy, Default)]
struct Tokens {
    input: u64,
    output: u64,
    cached: u64,
    thoughts: u64,
    tool: u64,
    total: Option<u64>,
}

fn num(v: Option<&Value>) -> Option<u64> {
    let f = v?.as_f64()?;
    if f.is_finite() { Some(f.max(0.0).trunc() as u64) } else { None }
}

fn pick(m: &Map<String, Value>, keys: &[&str]) -> u64 {
    keys.iter().find_map(|k| num(m.get(*k))).unwrap_or(0)
}

fn parse_tokens(v: Option<&Value>) -> Option<Tokens> {
    let m = v?.as_object()?;
    Some(Tokens {
        input: pick(m, &["input", "prompt", "input_tokens", "prompt_tokens"]),
        output: pick(m, &["output", "candidates", "output_tokens", "candidates_tokens"]),
        cached: pick(m, &["cached", "cached_tokens"]),
        thoughts: pick(m, &["thoughts", "reasoning", "thoughts_tokens", "reasoning_tokens"]),
        tool: pick(m, &["tool", "tool_tokens"]),
        total: num(m.get("total").or_else(|| m.get("total_tokens"))),
    })
}

/// Splits input into (fresh input, cache read) assuming `input` includes `cached`.
fn subtract_cached(t: &Tokens) -> (u64, u64) {
    (t.input.saturating_sub(t.input.min(t.cached)), t.cached)
}

/// Session messages report `input` either inclusive or exclusive of `cached`; the reported
/// total disambiguates (as in ccusage).
fn normalize_session(t: &Tokens) -> (u64, u64) {
    let inclusive = t.input + t.output + t.thoughts + t.tool;
    if t.cached > 0 && t.total == Some(inclusive) && t.total != Some(inclusive + t.cached) {
        return subtract_cached(t);
    }
    (t.input, t.cached)
}

fn usage_from(t: &Tokens, split: (u64, u64)) -> Usage {
    let (fresh, cache_read) = split;
    let input = fresh + t.tool;
    let mut output = t.output;
    let mut reasoning = t.thoughts;
    if let Some(total) = t.total {
        let missing = total.saturating_sub(input + t.output + cache_read + t.thoughts);
        if missing > 0 {
            output += missing;
            if t.output > 0 {
                reasoning += missing;
            }
        }
    }
    Usage { input, output: output + t.thoughts, cache_read, cache_write: 0, cache_write_1h: 0, reasoning }
}

struct Ctx<'a> {
    session: String,
    project: String,
    fallback_ts: i64,
    out: &'a mut Vec<Entry>,
    prompts: &'a mut Vec<Prompt>,
}

impl Ctx<'_> {
    fn entry(&self, model: Option<&str>, ts: i64, t: &Tokens, split: (u64, u64)) -> Option<Entry> {
        let model = model.map(str::trim).filter(|m| !m.is_empty())?;
        let usage = usage_from(t, split);
        if usage.is_zero() {
            return None;
        }
        Some(Entry {
            ts,
            model: model.to_string(),
            provider: Some("google".to_string()),
            session: self.session.clone(),
            project: self.project.clone(),
            usage,
            stored_cost: None,
        })
    }
}

fn ts_of(m: &Map<String, Value>, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|k| m.get(*k).and_then(Value::as_str).and_then(parse_ts))
}

fn str_of<'a>(m: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    m.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// Entries from a `stats` object (`models.<name>.tokens` or flat tokens); input includes cached.
fn stats_entries(ctx: &mut Ctx, stats: Option<&Value>, hint: Option<&str>, ts: i64) {
    let Some(stats) = stats.and_then(Value::as_object) else { return };
    if let Some(models) = stats.get("models").and_then(Value::as_object) {
        let mut any = false;
        for (name, data) in models {
            let Some(t) = data.get("tokens").and_then(|t| parse_tokens(Some(t))) else { continue };
            if let Some(e) = ctx.entry(Some(name), ts, &t, subtract_cached(&t)) {
                ctx.out.push(e);
                any = true;
            }
        }
        if any {
            return;
        }
    }
    if let Some(t) = parse_tokens(Some(&Value::Object(stats.clone()))) {
        if let Some(e) = ctx.entry(hint.or(Some("unknown")), ts, &t, subtract_cached(&t)) {
            ctx.out.push(e);
        }
    }
}

fn direct_entry(ctx: &Ctx, m: &Map<String, Value>, hint: Option<&str>) -> Option<Entry> {
    let t = parse_tokens(m.get("tokens"))?;
    let ts = ts_of(m, &["timestamp", "created_at"]).unwrap_or(ctx.fallback_ts);
    ctx.entry(str_of(m, "model").or(hint), ts, &t, normalize_session(&t))
}

fn parse_file(path: &Path, jsonl: bool, data: &mut HarnessData) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let fallback_ts = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown").to_string();
    // <tmp>/<project>/chats/<file>
    let project = path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let mut entries = Vec::new();
    let mut prompts = Vec::new();
    let mut ctx = Ctx { session: stem, project, fallback_ts, out: &mut entries, prompts: &mut prompts };
    if !jsonl {
        let Ok(Value::Object(rec)) = serde_json::from_str::<Value>(&text) else { return Ok(()) };
        if let Some(s) = str_of(&rec, "sessionId").or_else(|| str_of(&rec, "session_id")) {
            ctx.session = s.to_string();
        }
        let session_ts = ts_of(&rec, &["startTime", "lastUpdated"]).unwrap_or(fallback_ts);
        if let Some(msgs) = rec.get("messages").and_then(Value::as_array) {
            ctx.fallback_ts = session_ts;
            for m in msgs.iter().filter_map(Value::as_object) {
                match str_of(m, "type") {
                    Some("gemini") => {
                        if let Some(e) = direct_entry(&ctx, m, None) {
                            ctx.out.push(e);
                        }
                    }
                    Some("user") => {
                        let ts = ts_of(m, &["timestamp"]).unwrap_or(session_ts);
                        ctx.prompts.push(Prompt { ts, session: ctx.session.clone() });
                    }
                    _ => {}
                }
            }
        } else if str_of(&rec, "type") == Some("gemini") {
            if let Some(e) = direct_entry(&ctx, &rec, None) {
                ctx.out.push(e);
            }
        } else {
            let ts = ts_of(&rec, &["timestamp"]).unwrap_or(fallback_ts);
            let stats = rec.get("stats").or_else(|| rec.get("result").and_then(|r| r.get("stats")));
            stats_entries(&mut ctx, stats, str_of(&rec, "model"), ts);
        }
    } else {
        let mut model: Option<String> = None;
        let mut by_id: HashMap<String, usize> = HashMap::new();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let Ok(Value::Object(rec)) = serde_json::from_str::<Value>(line) else { continue };
            if let Some(s) = str_of(&rec, "sessionId").or_else(|| str_of(&rec, "session_id")) {
                ctx.session = s.to_string();
            }
            if let Some(m) = str_of(&rec, "model") {
                model = Some(m.to_string());
            }
            match str_of(&rec, "type") {
                Some("gemini") => {
                    let Some(e) = direct_entry(&ctx, &rec, model.as_deref()) else { continue };
                    match str_of(&rec, "id").map(str::to_string) {
                        Some(id) => match by_id.get(&id) {
                            Some(&i) => ctx.out[i] = e,
                            None => {
                                by_id.insert(id, ctx.out.len());
                                ctx.out.push(e);
                            }
                        },
                        None => ctx.out.push(e),
                    }
                }
                Some("user") => {
                    let ts = ts_of(&rec, &["timestamp"]).unwrap_or(fallback_ts);
                    ctx.prompts.push(Prompt { ts, session: ctx.session.clone() });
                }
                _ => {
                    let stats = rec.get("stats").or_else(|| rec.get("result").and_then(|r| r.get("stats")));
                    if stats.is_some() {
                        let ts = ts_of(&rec, &["timestamp"]).unwrap_or(fallback_ts);
                        stats_entries(&mut ctx, stats, model.as_deref(), ts);
                    }
                }
            }
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
            if matches!(p.extension().and_then(|e| e.to_str()), Some("json") | Some("jsonl")) {
                files.push(p.to_path_buf());
            }
        });
    }
    files.sort();
    files.dedup();
    for f in files {
        out.files += 1;
        let jsonl = f.extension().and_then(|e| e.to_str()) == Some("jsonl");
        if let Err(e) = parse_file(&f, jsonl, out) {
            out.errors.push(format!("gemini: {}: {e}", f.display()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ts-gemini-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("proj").join("chats")).unwrap();
        d
    }

    #[test]
    fn jsonl_separates_cached_and_dedups_by_id() {
        let d = fixture("jsonl");
        let lines = [
            r#"{"sessionId":"s1","startTime":"2026-05-17T11:07:00.000Z"}"#,
            r#"{"id":"u1","timestamp":"2026-05-17T11:07:01.000Z","type":"user","content":"hi"}"#,
            r#"{"id":"m1","timestamp":"2026-05-17T11:07:32.000Z","type":"gemini","model":"gemini-3-flash-preview","tokens":{"input":15327,"output":23,"cached":11526,"thoughts":919,"tool":7,"total":16276}}"#,
            r#"{"id":"m1","timestamp":"2026-05-17T11:07:33.000Z","type":"gemini","model":"gemini-3-flash-preview","tokens":{"input":15327,"output":23,"cached":11526,"thoughts":919,"tool":7,"total":16276}}"#,
            "not json",
        ];
        std::fs::write(d.join("proj/chats/a.jsonl"), lines.join("\n")).unwrap();
        let mut out = HarnessData::default();
        scan(&[d.clone()], &mut out);
        assert_eq!(out.entries.len(), 1);
        let e = &out.entries[0];
        assert_eq!(e.session, "s1");
        assert_eq!(e.project, "proj");
        assert_eq!(e.usage.input, 15_327 - 11_526 + 7);
        assert_eq!(e.usage.cache_read, 11_526);
        assert_eq!(e.usage.output, 23 + 919);
        assert_eq!(e.usage.reasoning, 919);
        assert_eq!(out.prompts.len(), 1);
        assert_eq!(out.files, 1);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn json_session_and_stats() {
        let d = fixture("json");
        let sess = r#"{"sessionId":"s2","startTime":"2026-05-17T11:00:00.000Z","messages":[
            {"type":"user","timestamp":"2026-05-17T11:00:01.000Z"},
            {"type":"gemini","model":"gemini-2.5-pro","timestamp":"2026-05-17T11:00:02.000Z","tokens":{"input":100,"output":10,"cached":40,"thoughts":5,"tool":0,"total":155}}]}"#;
        std::fs::write(d.join("proj/chats/s.json"), sess).unwrap();
        let stats = r#"{"timestamp":"2026-05-18T00:00:00.000Z","stats":{"models":{"gemini-2.5-flash":{"tokens":{"prompt":50,"candidates":5,"cached":20}}}}}"#;
        std::fs::write(d.join("proj/chats/t.json"), stats).unwrap();
        std::fs::write(d.join("proj/chats/bad.json"), "[1,2]").unwrap();
        let mut out = HarnessData::default();
        scan(&[d.clone()], &mut out);
        assert_eq!(out.entries.len(), 2);
        let pro = out.entries.iter().find(|e| e.model == "gemini-2.5-pro").unwrap();
        // total 155 == 100+10+5+0+40 (exclusive): input stays 100
        assert_eq!((pro.usage.input, pro.usage.cache_read), (100, 40));
        let flash = out.entries.iter().find(|e| e.model == "gemini-2.5-flash").unwrap();
        assert_eq!((flash.usage.input, flash.usage.cache_read, flash.usage.output), (30, 20, 5));
        assert_eq!(out.prompts.len(), 1);
        let _ = std::fs::remove_dir_all(d);
    }
}
