//! OpenClaw adapter (also legacy Clawdbot/Moltbot dirs): JSONL transcripts under
//! `<root>/agents/*/sessions/*.jsonl*` plus per-agent SQLite stores
//! `<root>/agents/*/agent/openclaw-agent.sqlite` (`transcript_events` table).
//!
//! Assistant `message.usage` carries `{input, output, cacheRead, cacheWrite, totalTokens, cost.total}`;
//! the active model/provider comes from `model_change` / `model-snapshot` records. SQLite rows
//! replace migrated JSONL duplicates of the same event. `OPENCLAW_DIR` (comma-separated) overrides roots.
use crate::model::{home_dir, parse_ts, Entry, HarnessData, Prompt, Usage};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub fn default_roots() -> Vec<PathBuf> {
    if let Ok(v) = std::env::var("OPENCLAW_DIR") {
        if !v.trim().is_empty() {
            return v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from).collect();
        }
    }
    let h = home_dir();
    [".openclaw", ".clawdbot", ".moltbot", ".moldbot"].iter().map(|d| h.join(d)).collect()
}

fn text(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

fn num(v: Option<&Value>) -> u64 {
    match v.and_then(Value::as_f64) {
        Some(f) if f.is_finite() => f.max(0.0).trunc() as u64,
        _ => 0,
    }
}

fn timestamp(v: Option<&Value>) -> Option<i64> {
    let v = v?;
    match v.as_f64() {
        Some(f) => (f >= 0.0).then_some(f.trunc() as i64),
        None => parse_ts(v.as_str()?),
    }
}

/// Tracks the active model/provider while walking one session's records in order.
#[derive(Default)]
struct State {
    model: Option<String>,
    provider: Option<String>,
}

enum Item {
    Entry(Entry, Option<String>),
    Prompt(i64),
}

fn handle(rec: &Value, st: &mut State, session: &str, fallback_ts: i64) -> Option<Item> {
    let ty = text(rec.get("type"));
    let custom = text(rec.get("customType"));
    if ty.as_deref() == Some("model_change") || (ty.as_deref() == Some("custom") && custom.as_deref() == Some("model-snapshot")) {
        let src = rec.get("data").filter(|d| d.is_object()).unwrap_or(rec);
        if let Some(m) = text(src.get("modelId")).or_else(|| text(src.get("model"))) {
            st.model = Some(m);
        }
        if let Some(p) = text(src.get("provider")) {
            st.provider = Some(p);
        }
        return None;
    }
    if ty.as_deref() != Some("message") {
        return None;
    }
    let msg = rec.get("message").filter(|m| m.is_object())?;
    let ts = timestamp(msg.get("timestamp")).or_else(|| timestamp(rec.get("timestamp"))).unwrap_or(fallback_ts);
    match text(msg.get("role")).as_deref() {
        Some("user") => return Some(Item::Prompt(ts)),
        Some("assistant") => {}
        _ => return None,
    }
    let u = msg.get("usage").filter(|u| u.is_object())?;
    let (input, mut output) = (num(u.get("input")), num(u.get("output")));
    let (cr, cw) = (num(u.get("cacheRead")), num(u.get("cacheWrite")));
    output += num(u.get("totalTokens")).saturating_sub(input + output + cr + cw);
    let usage = Usage { input, output, cache_read: cr, cache_write: cw, cache_write_1h: 0, reasoning: 0 };
    if usage.is_zero() {
        return None;
    }
    let model = text(msg.get("modelId"))
        .or_else(|| text(msg.get("model")))
        .or_else(|| st.model.clone())
        .unwrap_or_else(|| "unknown".into());
    let provider = text(msg.get("provider")).or_else(|| st.provider.clone()).map(|p| match p.as_str() {
        "openai-codex" => "openai".to_string(),
        _ => p,
    });
    let stored_cost = u.get("cost").and_then(|c| c.get("total")).and_then(Value::as_f64).filter(|c| *c > 0.0);
    let entry = Entry { ts, model, provider, session: session.to_string(), project: "openclaw".into(), usage, stored_cost };
    Some(Item::Entry(entry, text(rec.get("id"))))
}

/// Identity of an event across a JSONL -> SQLite migration (cost excluded).
fn key(e: &Entry) -> String {
    let u = &e.usage;
    format!("{}:{}:{}:{}:{}:{}:{}", e.session, e.ts, e.model, u.input, u.output, u.cache_write, u.cache_read)
}

fn session_files(root: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    crate::scan::walk(&root.join("agents"), &mut |p: &Path| {
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else { return };
        if let Some(i) = name.find(".jsonl") {
            let suffix = &name[i..];
            if suffix == ".jsonl" || suffix.starts_with(".jsonl.deleted.") || suffix.starts_with(".jsonl.reset.") {
                v.push(p.to_path_buf());
            }
        }
    });
    v
}

fn read_sqlite(db: &Path) -> Result<Vec<(Entry, Option<String>, Vec<i64>)>, String> {
    let uri = format!("file:{}?mode=ro", db.to_string_lossy().replace('\\', "/"));
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(uri, flags).map_err(|e| e.to_string())?;
    let has: bool = conn
        .query_row("SELECT 1 FROM sqlite_master WHERE type='table' AND name='transcript_events'", [], |_| Ok(true))
        .unwrap_or(false);
    if !has {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare("SELECT session_id, event_json, created_at FROM transcript_events ORDER BY session_id ASC, seq ASC")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, rusqlite::types::Value>(2)?)))
        .map_err(|e| e.to_string())?;
    let (mut out, mut cur, mut st) = (Vec::new(), String::new(), State::default());
    for (sid, json, created) in rows.flatten() {
        if sid != cur {
            cur = sid.clone();
            st = State::default();
        }
        let fallback = match created {
            rusqlite::types::Value::Integer(i) => i.max(0),
            rusqlite::types::Value::Real(f) => f.max(0.0) as i64,
            _ => 0,
        };
        let Ok(rec) = serde_json::from_str::<Value>(&json) else { continue };
        match handle(&rec, &mut st, &sid, fallback) {
            Some(Item::Entry(e, id)) => out.push((e, id, Vec::new())),
            Some(Item::Prompt(ts)) => out.push((
                Entry { ts, model: String::new(), provider: None, session: sid, project: String::new(), usage: Usage::default(), stored_cost: None },
                None,
                vec![ts],
            )),
            None => {}
        }
    }
    Ok(out)
}

pub fn scan(roots: &[PathBuf], out: &mut HarnessData) {
    let mut entries: Vec<Entry> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut prompts: HashSet<(String, i64)> = HashSet::new();
    let mut sqlite: Vec<(Entry, Option<String>)> = Vec::new();
    let mut seen_roots = HashSet::new();
    for root in roots.iter().filter(|r| r.is_dir() && seen_roots.insert((*r).clone())) {
        if let Ok(agents) = std::fs::read_dir(root.join("agents")) {
            let mut dbs: Vec<PathBuf> = agents
                .flatten()
                .map(|a| a.path().join("agent").join("openclaw-agent.sqlite"))
                .filter(|p| p.is_file())
                .collect();
            dbs.sort();
            for db in dbs {
                out.files += 1;
                match read_sqlite(&db) {
                    Ok(rows) => {
                        for (e, id, p) in rows {
                            if let Some(ts) = p.first() {
                                prompts.insert((e.session, *ts));
                            } else {
                                sqlite.push((e, id));
                            }
                        }
                    }
                    Err(e) => out.errors.push(format!("openclaw: {}: {e}", db.display())),
                }
            }
        }
        let mut files = session_files(root);
        files.sort();
        for f in files {
            out.files += 1;
            let Ok(content) = std::fs::read_to_string(&f) else {
                out.errors.push(format!("openclaw: {}: unreadable", f.display()));
                continue;
            };
            let fallback = std::fs::metadata(&f)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as i64);
            let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");
            let session = name.find(".jsonl").filter(|i| *i > 0).map_or(name, |i| &name[..i]).to_string();
            let mut st = State::default();
            for line in content.lines().filter(|l| !l.trim().is_empty()) {
                let Ok(rec) = serde_json::from_str::<Value>(line) else { continue };
                match handle(&rec, &mut st, &session, fallback) {
                    Some(Item::Entry(e, _)) => {
                        if seen.insert(format!("{}:{}", key(&e), e.stored_cost.unwrap_or(0.0))) {
                            entries.push(e);
                        }
                    }
                    Some(Item::Prompt(ts)) => {
                        prompts.insert((session.clone(), ts));
                    }
                    None => {}
                }
            }
        }
    }
    // SQLite rows win over migrated JSONL copies (same event key, possibly corrected cost).
    let mut index: HashMap<String, usize> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        index.entry(key(e)).or_insert(i);
    }
    let mut sqlite_ids = HashSet::new();
    for (e, id) in sqlite {
        if let Some(id) = &id {
            if !sqlite_ids.insert(format!("{}:{id}", e.session)) {
                continue;
            }
        }
        match index.get(&key(&e)) {
            Some(&i) => entries[i] = e,
            None => {
                index.insert(key(&e), entries.len());
                entries.push(e);
            }
        }
    }
    entries.sort_by_key(|e| e.ts);
    out.entries.extend(entries);
    let mut ps: Vec<_> = prompts.into_iter().collect();
    ps.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    out.prompts.extend(ps.into_iter().map(|(session, ts)| Prompt { ts, session }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ts-openclaw-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("agents/main/sessions")).unwrap();
        std::fs::create_dir_all(d.join("agents/main/agent")).unwrap();
        d
    }

    #[test]
    fn jsonl_tracks_model_dedups_and_counts_prompts() {
        let d = fixture("jsonl");
        let line = r#"{"type":"message","message":{"role":"assistant","model":"gpt-5.2","usage":{"input":1,"output":1,"totalTokens":9},"timestamp":1769753935279}}"#;
        let lines = [
            r#"{"type":"model_change","modelId":"gpt-5.2","provider":"openai-codex"}"#,
            r#"{"type":"message","message":{"role":"user","timestamp":1769753930000}}"#,
            r#"{"type":"message","message":{"role":"assistant","usage":{"input":1660,"output":55,"cacheRead":108928,"cost":{"total":0.02}},"timestamp":1769753935279}}"#,
            line,
            line,
            "garbage",
        ];
        std::fs::write(d.join("agents/main/sessions/abc.jsonl"), lines.join("\n")).unwrap();
        std::fs::write(d.join("agents/main/sessions/old.jsonl.reset.2026-03-20T06-34-44.520Z"), line).unwrap();
        let mut out = HarnessData::default();
        scan(&[d.clone()], &mut out);
        assert_eq!(out.entries.len(), 3);
        let e = out.entries.iter().find(|e| e.usage.cache_read > 0).unwrap();
        assert_eq!((e.session.as_str(), e.model.as_str(), e.provider.as_deref()), ("abc", "gpt-5.2", Some("openai")));
        assert_eq!((e.usage.input, e.usage.output, e.stored_cost), (1660, 55, Some(0.02)));
        let dup = out.entries.iter().find(|e| e.session == "abc" && e.usage.cache_read == 0).unwrap();
        assert_eq!(dup.usage.output, 1 + 7); // totalTokens gap added to output
        assert!(out.entries.iter().any(|e| e.session == "old"));
        assert_eq!(out.prompts.len(), 1);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn sqlite_replaces_migrated_jsonl_copy() {
        let d = fixture("sqlite");
        let line = r#"{"type":"message","message":{"role":"assistant","model":"gpt-5.2","usage":{"input":10,"output":20,"cost":{"total":0.50}},"timestamp":1769753935279}}"#;
        std::fs::write(d.join("agents/main/sessions/session.jsonl"), line).unwrap();
        let conn = Connection::open(d.join("agents/main/agent/openclaw-agent.sqlite")).unwrap();
        conn.execute("CREATE TABLE transcript_events (session_id TEXT NOT NULL, seq INTEGER NOT NULL, event_json TEXT NOT NULL, created_at INTEGER NOT NULL)", []).unwrap();
        let rows = [
            r#"{"id":"e0","type":"model_change","modelId":"gpt-5.2","provider":"openai"}"#,
            r#"{"id":"e1","type":"message","message":{"role":"assistant","usage":{"input":10,"output":20,"cost":{"total":0.51}},"timestamp":1769753935279}}"#,
            r#"{"id":"e2","type":"message","message":{"role":"assistant","usage":{"input":5,"output":5},"timestamp":1769753936000}}"#,
        ];
        for (i, r) in rows.iter().enumerate() {
            conn.execute("INSERT INTO transcript_events VALUES ('session', ?1, ?2, 1769753935279)", rusqlite::params![i as i64, r]).unwrap();
        }
        drop(conn);
        let mut out = HarnessData::default();
        scan(&[d.clone()], &mut out);
        assert_eq!(out.entries.len(), 2);
        assert_eq!(out.entries[0].stored_cost, Some(0.51));
        assert_eq!(out.entries[0].provider.as_deref(), Some("openai"));
        assert_eq!(out.files, 2);
        let _ = std::fs::remove_dir_all(d);
    }
}
