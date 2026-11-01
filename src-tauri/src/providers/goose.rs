//! Block Goose adapter: per-session accumulated token counters in `sessions.db` (SQLite).
//!
//! `GOOSE_PATH_ROOT` overrides the default (`<root>/data/sessions`). Defaults:
//! `~/.local/share/goose/sessions`, `~/Library/Application Support/goose/sessions`,
//! `~/.local/share/Block/goose/sessions`, plus `%APPDATA%\Block\goose\data\sessions` on Windows.
//! Roots are directories containing `sessions.db` (a direct path to the db also works).
//! One entry per session (accumulated totals).
use crate::model::{home_dir, parse_ts, Entry, HarnessData, Usage};
use rusqlite::{types::ValueRef, Connection, OpenFlags};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub fn default_roots() -> Vec<PathBuf> {
    if let Ok(root) = std::env::var("GOOSE_PATH_ROOT") {
        let root = root.trim();
        if !root.is_empty() {
            return vec![PathBuf::from(root).join("data").join("sessions")];
        }
    }
    let h = home_dir();
    let mut v = vec![
        h.join(".local/share/goose/sessions"),
        h.join("Library/Application Support/goose/sessions"),
        h.join(".local/share/Block/goose/sessions"),
    ];
    for var in ["APPDATA", "LOCALAPPDATA"] {
        if let Ok(a) = std::env::var(var) {
            v.push(PathBuf::from(a).join("Block").join("goose").join("data").join("sessions"));
        }
    }
    v
}

fn int(v: ValueRef) -> Option<u64> {
    match v {
        ValueRef::Integer(i) if i > 0 => Some(i as u64),
        ValueRef::Real(f) if f.is_finite() && f > 0.0 => Some(f as u64),
        _ => None,
    }
}

fn text(v: ValueRef) -> Option<String> {
    match v {
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).trim().to_string()),
        ValueRef::Integer(i) => Some(i.to_string()),
        _ => None,
    }
    .filter(|s| !s.is_empty())
}

fn parse_created(s: &str) -> Option<i64> {
    if let Ok(n) = s.parse::<i64>() {
        return (n > 0).then(|| if n > 1_000_000_000_000 { n } else { n * 1000 });
    }
    if let Some(t) = parse_ts(s) {
        return Some(t);
    }
    let b = s.as_bytes();
    if s.len() >= 19 && b[4] == b'-' && b[7] == b'-' && (b[10] == b' ' || b[10] == b'T') {
        return parse_ts(&format!("{}T{}Z", &s[..10], &s[11..19]));
    }
    if s.len() == 10 && b[4] == b'-' && b[7] == b'-' {
        return parse_ts(&format!("{s}T00:00:00Z"));
    }
    None
}

fn normalize_provider(p: Option<String>, model: &str) -> Option<String> {
    if let Some(p) = p {
        return Some(p.to_ascii_lowercase());
    }
    let m = model.to_ascii_lowercase();
    if m.starts_with("claude-") {
        Some("anthropic".into())
    } else if m.starts_with("gpt-") || m.starts_with("chatgpt-") {
        Some("openai".into())
    } else if m.starts_with("gemini-") {
        Some("google".into())
    } else {
        None
    }
}

fn read_db(path: &Path, seen: &mut HashSet<String>, out: &mut HarnessData) -> Result<(), String> {
    let uri = format!("file:{}?mode=ro", path.to_string_lossy().replace('\\', "/"));
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(uri, flags).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, model_config_json, provider_name, created_at, total_tokens, input_tokens, output_tokens, \
             accumulated_total_tokens, accumulated_input_tokens, accumulated_output_tokens \
             FROM sessions WHERE model_config_json IS NOT NULL AND TRIM(model_config_json) != ''",
        )
        .map_err(|e| e.to_string())?;
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    while let Ok(Some(r)) = rows.next() {
        let col = |i: usize| r.get_ref(i).ok();
        let Some(id) = col(0).and_then(text) else { continue };
        let model = col(1)
            .and_then(text)
            .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok())
            .and_then(|v| v.get("model_name").and_then(|m| m.as_str()).map(|m| m.trim().to_string()))
            .filter(|m| !m.is_empty());
        let ts = col(3).and_then(text).as_deref().and_then(parse_created);
        let (Some(model), Some(ts)) = (model, ts) else { continue };
        let input = col(8).and_then(int).or_else(|| col(5).and_then(int)).unwrap_or(0);
        let output = col(9).and_then(int).or_else(|| col(6).and_then(int)).unwrap_or(0);
        let total = col(7).and_then(int).or_else(|| col(4).and_then(int)).unwrap_or(input + output);
        if input + output + total == 0 {
            continue;
        }
        // Tokens beyond input + output (reasoning) are folded into output.
        let extra = total.saturating_sub(input + output);
        if !seen.insert(format!("{}:{id}", path.display())) {
            continue;
        }
        out.entries.push(Entry {
            ts,
            provider: normalize_provider(col(2).and_then(text), &model),
            model,
            session: id,
            project: "goose".into(),
            usage: Usage { input, output: output + extra, reasoning: extra, ..Usage::default() },
            stored_cost: None,
        });
    }
    Ok(())
}

pub fn scan(roots: &[PathBuf], out: &mut HarnessData) {
    let mut seen = HashSet::new();
    let mut dbs: Vec<PathBuf> =
        roots.iter().flat_map(|r| [r.join("sessions.db"), r.clone()]).filter(|p| p.is_file()).collect();
    dbs.sort();
    dbs.dedup();
    for db in dbs {
        out.files += 1;
        if let Err(e) = read_db(&db, &mut seen, out) {
            out.errors.push(format!("goose: {}: {e}", db.display()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_accumulated_tokens() {
        let d = std::env::temp_dir().join(format!("ts-goose-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let c = Connection::open(d.join("sessions.db")).unwrap();
        c.execute(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, model_config_json TEXT, provider_name TEXT, created_at TEXT, \
             total_tokens INTEGER, input_tokens INTEGER, output_tokens INTEGER, accumulated_total_tokens INTEGER, \
             accumulated_input_tokens INTEGER, accumulated_output_tokens INTEGER)",
            [],
        )
        .unwrap();
        let ins = [
            r#"INSERT INTO sessions VALUES ('a','{"model_name":"claude-sonnet-4"}','anthropic','2026-05-01 01:02:03',NULL,NULL,NULL,180,100,50)"#,
            r#"INSERT INTO sessions VALUES ('b','{"model_name":"gpt-5"}',NULL,'2026-05-01 01:02:03',30,20,10,0,0,0)"#,
            r#"INSERT INTO sessions VALUES ('c','{}',NULL,'2026-05-01 01:02:03',30,20,10,0,0,0)"#,
            r#"INSERT INTO sessions VALUES ('d','{"model_name":"x"}',NULL,'garbage',30,20,10,0,0,0)"#,
        ];
        for s in ins {
            c.execute(s, []).unwrap();
        }
        drop(c);
        let mut out = HarnessData::default();
        scan(&[d.clone(), d.clone()], &mut out);
        assert_eq!(out.entries.len(), 2);
        let a = out.entries.iter().find(|e| e.session == "a").unwrap();
        assert_eq!((a.usage.input, a.usage.output, a.usage.reasoning), (100, 80, 30));
        assert_eq!(a.provider.as_deref(), Some("anthropic"));
        assert_eq!(a.ts, parse_ts("2026-05-01T01:02:03Z").unwrap());
        let b = out.entries.iter().find(|e| e.session == "b").unwrap();
        assert_eq!((b.usage.input, b.usage.output, b.provider.as_deref()), (20, 10, Some("openai")));
        assert_eq!(out.files, 1);
        let _ = std::fs::remove_dir_all(d);
    }
}
