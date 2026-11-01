//! OpenCode: `$OPENCODE_DATA_DIR` or `$XDG_DATA_HOME/opencode` or `~/.local/share/opencode`.
//!
//! The desktop app runs the bundled `opencode-cli` as a sidecar, so desktop and CLI share
//! one SQLite database (`opencode.db`, or `opencode-<channel>.db`). Schema v2 stores
//! messages in `session_message` (JSON `data`, `type` = assistant/user/...), v1 in `message`;
//! legacy installs keep `storage/message/<session>/<id>.json`. The database is opened
//! read-only. Forked sessions copy the parent's rows up to `fork_boundary`; those are skipped.
use crate::model::{project_name, Entry, HarnessData, Prompt, Usage};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub fn data_dirs() -> Vec<PathBuf> {
    if let Ok(v) = std::env::var("OPENCODE_DATA_DIR") {
        return v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from).filter(|p| p.is_dir()).collect();
    }
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| crate::model::home_dir().join(".local").join("share"));
    let d = base.join("opencode");
    if d.is_dir() { vec![d] } else { vec![] }
}

pub fn db_path(dir: &Path) -> Option<PathBuf> {
    let main = dir.join("opencode.db");
    if main.is_file() {
        return Some(main);
    }
    let mut chans: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            n.starts_with("opencode-") && n.ends_with(".db")
        })
        .collect();
    chans.sort();
    chans.into_iter().next()
}

/// Signature used to detect database changes (main file plus WAL).
pub fn db_signature(db: &Path) -> (u64, i64) {
    let mut size = 0u64;
    let mut mtime = 0i64;
    for p in [db.to_path_buf(), PathBuf::from(format!("{}-wal", db.display()))] {
        if let Ok(m) = std::fs::metadata(&p) {
            size = size.wrapping_add(m.len());
            mtime = mtime.max(crate::scan::mtime_ns(&m));
        }
    }
    (size, mtime)
}

struct Msg {
    id: String,
    session: String,
    ts: i64,
    model: String,
    provider: String,
    usage: Usage,
    cost: f64,
}

fn parse_message(id: &str, session: &str, row_ts: i64, d: &Value) -> Option<Msg> {
    let t = d.get("tokens")?;
    let n = |v: Option<&Value>| v.and_then(|x| x.as_u64().or_else(|| x.as_f64().map(|f| f.max(0.0) as u64))).unwrap_or(0);
    let input = n(t.get("input"));
    let mut output = n(t.get("output"));
    let reasoning = n(t.get("reasoning"));
    let cache_read = n(t.pointer("/cache/read"));
    let cache_write = n(t.pointer("/cache/write"));
    let mut extra = reasoning;
    let total = n(t.get("total"));
    let known = input + output + cache_read + cache_write + extra;
    if total > known {
        if output == 0 { output = total - known } else { extra += total - known }
    }
    if input + output + cache_read + cache_write + extra == 0 {
        return None;
    }
    let m = d.get("model");
    let s = |v: Option<&Value>| v.and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from);
    let model = s(m.and_then(|m| m.get("id")))
        .or_else(|| s(m.and_then(|m| m.get("modelID"))))
        .or_else(|| s(d.get("modelID")))
        .or_else(|| s(m))?;
    let provider = s(m.and_then(|m| m.get("providerID"))).or_else(|| s(d.get("providerID"))).unwrap_or_default();
    let ts = d.pointer("/time/created").and_then(Value::as_i64).filter(|&x| x > 0).unwrap_or(row_ts);
    Some(Msg {
        id: id.to_string(),
        session: s(d.get("sessionID")).unwrap_or_else(|| session.to_string()),
        ts,
        model,
        provider,
        usage: Usage { input, output: output + extra, cache_read, cache_write, cache_write_1h: 0, reasoning },
        cost: d.get("cost").and_then(Value::as_f64).unwrap_or(0.0),
    })
}

fn table_exists(c: &Connection, name: &str) -> bool {
    c.query_row("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1", [name], |_| Ok(())).is_ok()
}

fn columns(c: &Connection, table: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    if let Ok(mut st) = c.prepare(&format!("PRAGMA table_info({table})")) {
        if let Ok(rows) = st.query_map([], |r| r.get::<_, String>(1)) {
            out.extend(rows.flatten());
        }
    }
    out
}

pub fn scan_dir(dir: &Path, out: &mut HarnessData, seen: &mut HashSet<String>) -> Result<(), String> {
    let mut msgs: Vec<Msg> = Vec::new();
    let mut prompts: Vec<(String, i64, String)> = Vec::new();
    let mut session_dir: HashMap<String, String> = HashMap::new();

    if let Some(db) = db_path(dir) {
        let uri = format!("file:{}?mode=ro", db.to_string_lossy().replace('\\', "/"));
        let c = Connection::open_with_flags(&uri, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .map_err(|e| format!("{}: {e}", db.display()))?;
        let _ = c.busy_timeout(std::time::Duration::from_millis(1500));

        for t in ["session_v2", "session"] {
            if table_exists(&c, t) && columns(&c, t).contains("directory") {
                if let Ok(mut st) = c.prepare(&format!("SELECT id, directory FROM {t}")) {
                    let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)));
                    if let Ok(rows) = rows {
                        for (id, d) in rows.flatten() {
                            if let Some(d) = d {
                                session_dir.entry(id).or_insert_with(|| project_name(&d));
                            }
                        }
                    }
                }
            }
        }

        if table_exists(&c, "message") {
            let mut st = c.prepare("SELECT id, session_id, time_created, data FROM message").map_err(|e| e.to_string())?;
            let rows = st
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?.unwrap_or(0), r.get::<_, String>(3)?)))
                .map_err(|e| e.to_string())?;
            for (id, sid, ts, data) in rows.flatten() {
                let Ok(d) = serde_json::from_str::<Value>(&data) else { continue };
                if d.get("role").and_then(Value::as_str) == Some("user") {
                    let t = d.pointer("/time/created").and_then(Value::as_i64).unwrap_or(ts);
                    prompts.push((id, t, sid));
                } else if let Some(m) = parse_message(&id, &sid, ts, &d) {
                    msgs.push(m);
                }
            }
        }

        if table_exists(&c, "session_message") {
            // Fork cutoffs: rows of a fork with seq <= cutoff are copied parent history.
            let mut cutoff: HashMap<String, i64> = HashMap::new();
            let cols = columns(&c, "session_v2");
            if cols.contains("fork_session_id") && cols.contains("fork_boundary") {
                if let Ok(mut st) = c.prepare("SELECT id, fork_session_id, fork_boundary FROM session_v2 WHERE fork_session_id IS NOT NULL AND fork_boundary IS NOT NULL") {
                    let rows: Vec<(String, String, String)> = st
                        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                        .map(|r| r.flatten().collect())
                        .unwrap_or_default();
                    for (id, parent, boundary) in rows {
                        let Ok(b) = serde_json::from_str::<Value>(&boundary) else { continue };
                        let Some(mid) = b.get("messageID").and_then(Value::as_str) else { continue };
                        let seq: Option<i64> = c
                            .query_row("SELECT seq FROM session_message WHERE session_id=?1 AND id=?2", [&parent, mid], |r| r.get(0))
                            .ok();
                        let Some(seq) = seq else { continue };
                        let cut = if b.get("type").and_then(Value::as_str) == Some("before") {
                            c.query_row("SELECT MAX(seq) FROM session_message WHERE session_id=?1 AND seq < ?2", rusqlite::params![parent, seq], |r| r.get::<_, Option<i64>>(0))
                                .ok()
                                .flatten()
                                .unwrap_or(-1)
                        } else {
                            seq
                        };
                        cutoff.insert(id, cut);
                    }
                }
            }
            let has_seq = columns(&c, "session_message").contains("seq");
            let sql = if has_seq {
                "SELECT id, session_id, type, data, time_created, seq FROM session_message WHERE type IN ('assistant','user')"
            } else {
                "SELECT id, session_id, type, data, time_created, 0 FROM session_message WHERE type IN ('assistant','user')"
            };
            let mut st = c.prepare(sql).map_err(|e| e.to_string())?;
            let rows = st
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                        r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    ))
                })
                .map_err(|e| e.to_string())?;
            for (id, sid, ty, data, ts, seq) in rows.flatten() {
                if cutoff.get(&sid).is_some_and(|&cut| seq <= cut) {
                    continue;
                }
                if ty == "user" {
                    prompts.push((id, ts, sid));
                    continue;
                }
                let Ok(d) = serde_json::from_str::<Value>(&data) else { continue };
                if let Some(mut m) = parse_message(&id, &sid, ts, &d) {
                    m.session = sid;
                    msgs.push(m);
                }
            }
        }
    }

    // Legacy JSON storage.
    let legacy = dir.join("storage").join("message");
    if legacy.is_dir() {
        let mut files = Vec::new();
        crate::scan::walk(&legacy, &mut |p| {
            if p.extension().is_some_and(|e| e == "json") {
                files.push(p.to_path_buf());
            }
        });
        let known: HashSet<String> = msgs.iter().map(|m| m.id.clone()).collect();
        for f in files {
            let id = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            if known.contains(&id) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&f) else { continue };
            let Ok(d) = serde_json::from_str::<Value>(&text) else { continue };
            let sid = d.get("sessionID").and_then(Value::as_str).unwrap_or("unknown").to_string();
            if d.get("role").and_then(Value::as_str) == Some("user") {
                let t = d.pointer("/time/created").and_then(Value::as_i64).unwrap_or(0);
                prompts.push((id, t, sid));
            } else if let Some(m) = parse_message(&id, &sid, 0, &d) {
                msgs.push(m);
            }
        }
    }

    for m in msgs {
        if !seen.insert(m.id.clone()) {
            continue;
        }
        let project = session_dir.get(&m.session).cloned().unwrap_or_else(|| "OpenCode".into());
        out.entries.push(Entry {
            ts: m.ts,
            model: m.model,
            provider: (!m.provider.is_empty()).then_some(m.provider),
            session: m.session,
            project,
            usage: m.usage,
            stored_cost: (m.cost > 0.0).then_some(m.cost),
        });
    }
    for (id, ts, sid) in prompts {
        if seen.insert(format!("prompt:{id}")) {
            out.prompts.push(Prompt { ts, session: sid });
        }
    }
    Ok(())
}
