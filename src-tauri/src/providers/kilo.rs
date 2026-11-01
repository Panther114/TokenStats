//! Kilo CLI adapter: assistant messages in `<data>/kilo.db` (SQLite, table `message`).
//!
//! `KILO_DATA_DIR` (comma-separated directories) overrides `~/.local/share/kilo`.
//! Message rows carry `{role, modelID, providerID, time.created, tokens{input,output,reasoning,total,cache{read,write}}, cost}`.
use crate::model::{home_dir, Entry, HarnessData, Usage};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub fn default_roots() -> Vec<PathBuf> {
    if let Ok(v) = std::env::var("KILO_DATA_DIR") {
        return v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from).collect();
    }
    vec![home_dir().join(".local").join("share").join("kilo")]
}

fn num(v: Option<&Value>) -> u64 {
    match v.and_then(Value::as_f64) {
        Some(f) if f.is_finite() => f.max(0.0).trunc() as u64,
        _ => 0,
    }
}

fn nonempty(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

/// Parses one message JSON blob into an entry (None for non-assistant / empty / undated rows).
fn parse_message(data: &str, row_session: &str, project: &str) -> Option<(Option<String>, Entry)> {
    let v: Value = serde_json::from_str(data).ok()?;
    if v.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let tokens = v.get("tokens")?.as_object()?;
    let cache = tokens.get("cache");
    let (input, output) = (num(tokens.get("input")), num(tokens.get("output")));
    let reasoning = num(tokens.get("reasoning"));
    let cache_read = num(cache.and_then(|c| c.get("read")));
    let cache_write = num(cache.and_then(|c| c.get("write")));
    let known = input + output + cache_read + cache_write + reasoning;
    // Tokens in `total` not covered by the parts are treated as extra output (as ccusage does).
    let missing = num(tokens.get("total")).saturating_sub(known);
    let output = output + reasoning + missing;
    if input + output + cache_read + cache_write == 0 {
        return None;
    }
    let model = nonempty(v.get("modelID"))?.to_string();
    let mut ts = v.get("time")?.get("created")?.as_f64()? as i64;
    if ts <= 0 {
        return None;
    }
    if ts < 1_000_000_000_000 {
        ts *= 1000;
    }
    let session = nonempty(v.get("session_id")).unwrap_or(row_session).to_string();
    let stored_cost = v.get("cost").and_then(Value::as_f64).filter(|c| *c > 0.0 && c.is_finite());
    let id = nonempty(v.get("id")).map(str::to_string);
    let entry = Entry {
        ts,
        model,
        provider: nonempty(v.get("providerID")).map(str::to_string),
        session,
        project: project.to_string(),
        usage: Usage { input, output, cache_read, cache_write, cache_write_1h: 0, reasoning: reasoning + missing },
        stored_cost,
    };
    Some((id, entry))
}

fn read_db(path: &Path, seen: &mut HashSet<String>, out: &mut HarnessData) -> Result<(), String> {
    let uri = format!("file:{}?mode=ro", path.to_string_lossy().replace('\\', "/"));
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(uri, flags).map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare("SELECT id, session_id, data FROM message").map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
        .map_err(|e| e.to_string())?;
    for (row_id, session, data) in rows.flatten() {
        let Some((id, e)) = parse_message(&data, &session, "kilo") else { continue };
        let key = id.unwrap_or_else(|| format!("{}:{row_id}", path.display()));
        if seen.insert(key) {
            out.entries.push(e);
        }
    }
    Ok(())
}

pub fn scan(roots: &[PathBuf], out: &mut HarnessData) {
    let mut seen = HashSet::new();
    for db in roots.iter().map(|r| r.join("kilo.db")).filter(|p| p.is_file()) {
        out.files += 1;
        if let Err(e) = read_db(&db, &mut seen, out) {
            out.errors.push(format!("kilo: {}: {e}", db.display()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db(name: &str, rows: &[(&str, &str, &str)]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ts-kilo-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let c = Connection::open(d.join("kilo.db")).unwrap();
        c.execute("CREATE TABLE message (id TEXT, session_id TEXT, data TEXT)", []).unwrap();
        for (id, s, data) in rows {
            c.execute("INSERT INTO message VALUES (?1, ?2, ?3)", [id, s, data]).unwrap();
        }
        d
    }

    #[test]
    fn parses_dedups_and_normalizes() {
        let a = r#"{"id":"m1","role":"assistant","providerID":"anthropic","modelID":"claude-sonnet-4","time":{"created":1767312000000},"tokens":{"input":100,"output":50,"reasoning":5,"cache":{"read":10,"write":20}},"cost":0.02}"#;
        let dup = a.replace("\"input\":100", "\"input\":999");
        let nots = r#"{"role":"assistant","modelID":"gpt-5","tokens":{"input":1,"output":1}}"#;
        let user = r#"{"role":"user","modelID":"gpt-5","time":{"created":1767312000000},"tokens":{"input":1}}"#;
        let total = r#"{"id":"m2","role":"assistant","modelID":"gpt-5","time":{"created":1767312000},"tokens":{"total":234,"cache":0}}"#;
        let d1 = make_db("a", &[("r1", "s1", a), ("r2", "s1", nots), ("r3", "s1", user), ("r4", "s2", total), ("r5", "s1", "bad")]);
        let d2 = make_db("b", &[("r1", "s1", &dup)]);
        let mut out = HarnessData::default();
        scan(&[d1.clone(), d2.clone()], &mut out);
        assert_eq!(out.files, 2);
        assert_eq!(out.entries.len(), 2);
        let e = &out.entries[0];
        assert_eq!((e.usage.input, e.usage.output, e.usage.cache_read, e.usage.cache_write), (100, 55, 10, 20));
        assert_eq!(e.usage.reasoning, 5);
        assert_eq!(e.stored_cost, Some(0.02));
        assert_eq!(e.provider.as_deref(), Some("anthropic"));
        let t = &out.entries[1];
        assert_eq!((t.usage.output, t.session.as_str(), t.ts), (234, "s2", 1767312000000));
        let _ = std::fs::remove_dir_all(d1);
        let _ = std::fs::remove_dir_all(d2);
    }
}
