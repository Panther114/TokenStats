//! Hermes Agent adapter: `~/.hermes/state.db` (SQLite `sessions` table).
//!
//! Each session row carries cumulative token counters, so one Entry is produced per session,
//! stamped with `started_at`. Recorded cost is `actual_cost_usd` else `estimated_cost_usd`.
//! `HERMES_HOME` (comma-separated directories containing `state.db`) overrides the default.
use crate::model::{home_dir, Entry, HarnessData, Usage};
use rusqlite::{types::Value, Connection, OpenFlags};
use std::collections::HashSet;
use std::path::PathBuf;

pub fn default_roots() -> Vec<PathBuf> {
    if let Ok(v) = std::env::var("HERMES_HOME") {
        return v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from).collect();
    }
    vec![home_dir().join(".hermes")]
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Real(f) if f.is_finite() => Some(*f),
        _ => None,
    }
}

fn as_u64(v: &Value) -> u64 {
    as_f64(v).map_or(0, |f| f.max(0.0).trunc() as u64)
}

fn provider_of(raw: Option<&str>, model: &str) -> Option<String> {
    let p = raw.map(|r| r.trim().to_ascii_lowercase().replace('-', "_")).unwrap_or_default();
    match p.as_str() {
        "" => {}
        "claude" | "anthropic" => return Some("anthropic".into()),
        "openai" | "openai_codex" => return Some("openai".into()),
        "google" | "google_ai" | "gemini" | "vertex" | "vertex_ai" => return Some("google".into()),
        other => return Some(other.to_string()),
    }
    let m = model.to_ascii_lowercase();
    if m.contains("claude") {
        Some("anthropic".into())
    } else if m.starts_with("gpt-") || m.contains("chatgpt") {
        Some("openai".into())
    } else if m.contains("gemini") {
        Some("google".into())
    } else if m.contains("grok") {
        Some("xai".into())
    } else {
        None
    }
}

fn read_db(path: &std::path::Path) -> Result<Vec<Entry>, String> {
    let uri = format!("file:{}?mode=ro", path.to_string_lossy().replace('\\', "/"));
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(uri, flags).map_err(|e| e.to_string())?;
    let cols: HashSet<String> = conn
        .prepare("PRAGMA table_info(sessions)")
        .and_then(|mut s| s.query_map([], |r| r.get::<_, String>(1))?.collect())
        .map_err(|e| e.to_string())?;
    if !["id", "model", "started_at"].iter().all(|c| cols.contains(*c)) {
        return Ok(Vec::new());
    }
    let opt = |c: &str| if cols.contains(c) { c.to_string() } else { "NULL".to_string() };
    let sql = format!(
        "SELECT id, model, {}, started_at, {}, {}, {}, {}, {}, {}, {} FROM sessions WHERE model IS NOT NULL AND TRIM(model) != ''",
        opt("billing_provider"),
        opt("input_tokens"),
        opt("output_tokens"),
        opt("cache_read_tokens"),
        opt("cache_write_tokens"),
        opt("reasoning_tokens"),
        opt("estimated_cost_usd"),
        opt("actual_cost_usd"),
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| (0..11).map(|i| r.get::<_, Value>(i)).collect::<Result<Vec<_>, _>>())
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows.flatten() {
        let (Value::Text(id), Value::Text(model)) = (&row[0], &row[1]) else { continue };
        let model = model.trim();
        let Some(started) = as_f64(&row[3]) else { continue };
        // started_at is epoch seconds (or milliseconds when > 1e12)
        let ms = if started > 1e12 { started } else { started * 1000.0 };
        if id.is_empty() || model.is_empty() || ms <= 0.0 {
            continue;
        }
        let provider = match &row[2] {
            Value::Text(t) => Some(t.as_str()),
            _ => None,
        };
        let reasoning = as_u64(&row[8]);
        let cost = as_f64(&row[10]).filter(|c| *c > 0.0).or_else(|| as_f64(&row[9]).filter(|c| *c > 0.0));
        let usage = Usage {
            input: as_u64(&row[4]),
            output: as_u64(&row[5]) + reasoning,
            cache_read: as_u64(&row[6]),
            cache_write: as_u64(&row[7]),
            cache_write_1h: 0,
            reasoning,
        };
        if usage.is_zero() && cost.is_none() {
            continue;
        }
        out.push(Entry {
            ts: ms.trunc() as i64,
            model: model.to_string(),
            provider: provider_of(provider, model),
            session: id.clone(),
            project: "hermes".to_string(),
            usage,
            stored_cost: cost,
        });
    }
    Ok(out)
}

pub fn scan(roots: &[PathBuf], out: &mut HarnessData) {
    let mut seen_dbs = HashSet::new();
    let mut seen_sessions = HashSet::new();
    for db in roots.iter().map(|r| r.join("state.db")).filter(|p| p.is_file() && seen_dbs.insert(p.clone())) {
        out.files += 1;
        match read_db(&db) {
            Ok(entries) => out.entries.extend(entries.into_iter().filter(|e| seen_sessions.insert(e.session.clone()))),
            Err(e) => out.errors.push(format!("hermes: {}: {e}", db.display())),
        }
    }
    out.entries.sort_by_key(|e| e.ts);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ts-hermes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reads_sessions_with_cost_and_reasoning() {
        let d = fixture("main");
        let c = Connection::open(d.join("state.db")).unwrap();
        c.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, source TEXT, model TEXT, billing_provider TEXT, started_at REAL NOT NULL,
               message_count INTEGER, input_tokens INTEGER, output_tokens INTEGER, cache_read_tokens INTEGER, cache_write_tokens INTEGER,
               reasoning_tokens INTEGER, estimated_cost_usd REAL, actual_cost_usd REAL);
             INSERT INTO sessions VALUES ('s1','cli','gpt-5.5','openai-codex',1779148800.5,72,244075,10019,3339776,0,3216,0.3,0.0);
             INSERT INTO sessions VALUES ('s2','cli','claude-sonnet-4',NULL,1779148900,1,10,20,0,5,0,NULL,0.42);
             INSERT INTO sessions VALUES ('s3','cli','  ',NULL,1779148900,1,10,20,0,0,0,NULL,NULL);
             INSERT INTO sessions VALUES ('s4','cli','gpt-5.5',NULL,1779149000,1,0,0,0,0,0,NULL,NULL);",
        )
        .unwrap();
        drop(c);
        let mut out = HarnessData::default();
        scan(&[d.clone(), d.clone()], &mut out);
        assert_eq!(out.entries.len(), 2);
        assert_eq!(out.files, 1);
        let s1 = &out.entries[0];
        assert_eq!((s1.session.as_str(), s1.ts, s1.provider.as_deref()), ("s1", 1_779_148_800_500, Some("openai")));
        assert_eq!((s1.usage.input, s1.usage.output, s1.usage.cache_read, s1.usage.reasoning), (244_075, 13_235, 3_339_776, 3_216));
        assert_eq!(s1.stored_cost, Some(0.3)); // zero actual cost is not a positive recorded cost -> estimated
        let s2 = &out.entries[1];
        assert_eq!((s2.usage.cache_write, s2.stored_cost, s2.provider.as_deref()), (5, Some(0.42), Some("anthropic")));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn missing_table_is_not_an_error() {
        let d = fixture("empty");
        Connection::open(d.join("state.db")).unwrap().execute_batch("CREATE TABLE x (a)").unwrap();
        let mut out = HarnessData::default();
        scan(&[d.clone()], &mut out);
        assert!(out.entries.is_empty() && out.errors.is_empty());
        let _ = std::fs::remove_dir_all(d);
    }
}
