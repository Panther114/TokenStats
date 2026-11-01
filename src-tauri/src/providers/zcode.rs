//! Z.ai Z Code adapter: `model_usage` rows (joined to `session`) in `<home>/cli/db/db.sqlite`.
//!
//! `ZCODE_HOME` (comma-separated) overrides `~/.zcode`. Roots are the Z Code home directories.
//! `input_tokens` includes both cache buckets, so they are subtracted to give fresh input.
use crate::model::{home_dir, project_name, Entry, HarnessData, Usage};
use rusqlite::{types::ValueRef, Connection, OpenFlags};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const DB_REL: &str = "cli/db/db.sqlite";

pub fn default_roots() -> Vec<PathBuf> {
    if let Ok(v) = std::env::var("ZCODE_HOME") {
        let roots: Vec<PathBuf> = v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from).collect();
        if !roots.is_empty() {
            return roots;
        }
    }
    vec![home_dir().join(".zcode")]
}

fn columns(conn: &Connection, table: &str) -> Option<HashSet<String>> {
    let stmt = conn.prepare(&format!("SELECT * FROM \"{table}\" LIMIT 0")).ok()?;
    Some(stmt.column_names().iter().map(|s| s.to_string()).collect())
}

fn tok(v: ValueRef) -> u64 {
    match v {
        ValueRef::Integer(i) => i.max(0) as u64,
        ValueRef::Real(f) if f.is_finite() && f > 0.0 => f.round() as u64,
        _ => 0,
    }
}

fn opt_text(v: ValueRef) -> Option<String> {
    match v {
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).trim().to_string()).filter(|s| !s.is_empty()),
        _ => None,
    }
}

fn provider_hint(raw: Option<&str>, model: &str) -> Option<String> {
    let is_zai = |p: &str| {
        matches!(p, "zai" | "z.ai" | "zai-coding-plan" | "builtin:zai-coding-plan" | "builtin:bigmodel-coding-plan")
            || p.starts_with("account:zai-")
            || p.starts_with("builtin:zai-")
    };
    match raw.map(str::to_ascii_lowercase) {
        Some(p) if is_zai(&p) => Some("zai".into()),
        Some(p) => Some(p),
        None if model.to_ascii_lowercase().starts_with("glm") => Some("zai".into()),
        None => None,
    }
}

fn read_db(path: &Path, seen: &mut HashSet<String>, out: &mut HarnessData) -> Result<(), String> {
    let uri = format!("file:{}?mode=ro", path.to_string_lossy().replace('\\', "/"));
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(uri, flags).map_err(|e| e.to_string())?;
    let _ = conn.busy_timeout(std::time::Duration::from_millis(5000));
    let mc = columns(&conn, "model_usage").ok_or("missing table model_usage")?;
    let sc = columns(&conn, "session").ok_or("missing table session")?;
    let need = ["id", "session_id", "started_at", "model_id", "status", "input_tokens", "output_tokens"];
    if need.iter().any(|c| !mc.contains(*c)) || !sc.contains("id") || !sc.contains("directory") {
        return Err("unsupported schema".into());
    }
    let opt = |c: &str, default: &'static str| if mc.contains(c) { format!("m.{c}") } else { default.to_string() };
    let query = format!(
        "SELECT m.id, m.session_id, m.started_at, m.model_id, m.input_tokens, m.output_tokens, {}, {}, {}, {}, s.directory \
         FROM model_usage m LEFT JOIN session s ON s.id = m.session_id WHERE m.status = 'completed'",
        opt("cache_creation_input_tokens", "0"),
        opt("cache_read_input_tokens", "0"),
        opt("computed_total_tokens", "m.input_tokens + m.output_tokens"),
        opt("provider_id", "NULL"),
    );
    let mut stmt = conn.prepare(&query).map_err(|e| e.to_string())?;
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    while let Ok(Some(r)) = rows.next() {
        let col = |i: usize| r.get_ref(i).unwrap_or(ValueRef::Null);
        let (Some(id), Some(session), Some(model)) =
            (opt_text(col(0)), opt_text(col(1)), opt_text(col(3)))
        else {
            continue;
        };
        let ts = match col(2) {
            ValueRef::Integer(i) => i,
            ValueRef::Real(f) if f.is_finite() => f.round() as i64,
            _ => continue,
        };
        if ts <= 0 {
            continue;
        }
        let (input_all, output) = (tok(col(4)), tok(col(5)));
        let cache_read = tok(col(7)).min(input_all);
        let cache_write = tok(col(6)).min(input_all - cache_read);
        let input = input_all - cache_read - cache_write;
        let total = tok(col(8));
        // Tokens in the reported total not covered by the parts count as extra output.
        let output = output + total.saturating_sub(input + output + cache_read + cache_write);
        if input + output + cache_read + cache_write == 0 || !seen.insert(id) {
            continue;
        }
        let project = opt_text(col(10)).map(|d| project_name(&d)).unwrap_or_else(|| "unknown".into());
        out.entries.push(Entry {
            ts,
            provider: provider_hint(opt_text(col(9)).as_deref(), &model),
            model,
            session,
            project,
            usage: Usage { input, output, cache_read, cache_write, cache_write_1h: 0, reasoning: 0 },
            stored_cost: None,
        });
    }
    Ok(())
}

pub fn scan(roots: &[PathBuf], out: &mut HarnessData) {
    let mut seen = HashSet::new();
    let mut dbs: Vec<PathBuf> = roots.iter().map(|r| r.join(DB_REL)).filter(|p| p.is_file()).collect();
    dbs.sort();
    dbs.dedup();
    for db in dbs {
        out.files += 1;
        if let Err(e) = read_db(&db, &mut seen, out) {
            out.errors.push(format!("zcode: {}: {e}", db.display()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str, legacy: bool) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ts-zcode-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("cli/db")).unwrap();
        let c = Connection::open(d.join(DB_REL)).unwrap();
        if legacy {
            c.execute_batch(
                "CREATE TABLE model_usage (id TEXT PRIMARY KEY, session_id TEXT, started_at INTEGER, model_id TEXT, status TEXT, \
                 input_tokens INTEGER, output_tokens INTEGER, cache_read_input_tokens INTEGER, computed_total_tokens INTEGER);
                 CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT);
                 INSERT INTO session VALUES ('s1', 'C:/work/proj');
                 INSERT INTO model_usage VALUES ('u1','s1',1786909042666,'glm-4.6','completed',100,10,30,110);",
            )
            .unwrap();
        } else {
            c.execute_batch(
                "CREATE TABLE model_usage (id TEXT PRIMARY KEY, session_id TEXT, started_at INTEGER, model_id TEXT, provider_id TEXT, \
                 status TEXT, input_tokens INTEGER, output_tokens INTEGER, cache_creation_input_tokens INTEGER, \
                 cache_read_input_tokens INTEGER, computed_total_tokens INTEGER);
                 CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, version TEXT);
                 INSERT INTO session VALUES ('s1', '/workspace/project', '0.16.3');
                 INSERT INTO model_usage VALUES ('u1','s1',1786909042666,'GLM-5.3','builtin:zai-coding-plan','completed',100,10,15,25,110);
                 INSERT INTO model_usage VALUES ('u2','s1',1786909042667,'GLM-5.3','builtin:zai-coding-plan','failed',100,10,0,0,110);
                 INSERT INTO model_usage VALUES ('u3','s9',1786909042668,'GLM-5.3',NULL,'completed',0,0,0,0,0);
                 INSERT INTO model_usage VALUES ('u4','s9',1786909042669,'other','OpenAI','completed',10,2,0,0,20);",
            )
            .unwrap();
        }
        d
    }

    #[test]
    fn current_and_legacy_schemas() {
        let (a, b) = (root("cur", false), root("old", true));
        let mut out = HarnessData::default();
        scan(&[a.clone(), b.clone(), a.clone()], &mut out);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.files, 2);
        // u1 appears in both databases; the first one wins. u2 (failed) and u3 (empty) are skipped.
        assert_eq!(out.entries.len(), 2);
        let u1 = out.entries.iter().find(|e| e.model == "GLM-5.3").unwrap();
        assert_eq!((u1.usage.input, u1.usage.output, u1.usage.cache_read, u1.usage.cache_write), (60, 10, 25, 15));
        assert_eq!((u1.project.as_str(), u1.provider.as_deref()), ("project", Some("zai")));
        let u4 = out.entries.iter().find(|e| e.model == "other").unwrap();
        // total 20 > 10 + 2: the gap of 8 is extra output.
        assert_eq!((u4.usage.input, u4.usage.output, u4.project.as_str()), (10, 10, "unknown"));
        assert_eq!(u4.provider.as_deref(), Some("openai"));
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }

    #[test]
    fn legacy_schema_alone() {
        let b = root("old2", true);
        let mut out = HarnessData::default();
        scan(&[b.clone()], &mut out);
        assert_eq!(out.entries.len(), 1);
        let e = &out.entries[0];
        assert_eq!((e.usage.input, e.usage.cache_read, e.usage.cache_write, e.usage.output), (70, 30, 0, 10));
        assert_eq!((e.project.as_str(), e.provider.as_deref()), ("proj", Some("zai")));
        let _ = std::fs::remove_dir_all(b);
    }
}
