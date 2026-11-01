//! Codex: `$CODEX_HOME/{sessions,archived_sessions}/**/rollout-*.jsonl[.zst]`.
//!
//! `token_count` events carry cumulative `total_token_usage` and per-response
//! `last_token_usage`; `last` is trusted only when the cumulative total advanced, otherwise
//! the delta is `total - previous` (zero for repeated events, which are skipped). The model
//! comes from the most recent `turn_context`. Forked and subagent rollouts replay their
//! parent's earlier events, which are skipped by delta matching against the parent.
use crate::model::{parse_ts, project_name, Entry, HarnessData, Prompt, Usage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Raw counters: input (incl. cached), cached, cache write, output (incl. reasoning), reasoning, total.
pub type Raw = [u64; 6];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ev {
    pub ts: i64,
    pub model: String,
    pub raw: Raw,
    pub response_id: Option<String>,
    pub compaction: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FileData {
    pub session: String,
    pub project: String,
    pub meta_id: Option<String>,
    pub parent_id: Option<String>,
    pub subagent: bool,
    pub meta_ts: Option<i64>,
    pub events: Vec<Ev>,
    pub prompts: Vec<i64>,
}

pub fn homes() -> Vec<PathBuf> {
    if let Ok(v) = std::env::var("CODEX_HOME") {
        let list: Vec<PathBuf> = v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(PathBuf::from).collect();
        if !list.is_empty() {
            return list;
        }
    }
    let h = crate::model::home_dir().join(".codex");
    if h.is_dir() { vec![h] } else { vec![] }
}

/// Returns (file, source dir) pairs; the active `sessions/` copy wins over `archived_sessions/`,
/// and a plain `.jsonl` wins over its `.jsonl.zst` twin.
pub fn discover(home: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for src in [home.join("sessions"), home.join("archived_sessions")] {
        if !src.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        crate::scan::walk(&src, &mut |p| {
            let n = p.to_string_lossy();
            if n.ends_with(".jsonl") || n.ends_with(".jsonl.zst") {
                files.push(p.to_path_buf());
            }
        });
        files.sort();
        for f in files {
            let rel = f.strip_prefix(&src).unwrap_or(&f).to_string_lossy().replace('\\', "/");
            let rel = rel.trim_end_matches(".zst").to_string();
            if seen.insert(rel) {
                out.push((f, src.clone()));
            }
        }
    }
    out
}

fn num(v: Option<&Value>) -> u64 {
    match v {
        Some(Value::Number(n)) => n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64)).unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn first<'a>(o: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| o.get(*k).filter(|v| !v.is_null()))
}

fn usage_of(u: &Value) -> Option<Raw> {
    if !u.is_object() {
        return None;
    }
    let input = num(first(u, &["input_tokens", "prompt_tokens", "input"]));
    let output = num(first(u, &["output_tokens", "completion_tokens", "output"]));
    let reasoning = num(first(u, &["reasoning_output_tokens", "reasoning_tokens"]));
    let cached = num(first(u, &["cached_input_tokens", "cache_read_input_tokens", "cached_tokens"])).min(input);
    let cw = num(first(u, &["cache_write_input_tokens", "cache_creation_input_tokens"])).min(input - cached);
    let total = match num(u.get("total_tokens")) {
        0 => input + output,
        t => t,
    };
    Some([input, cached, cw, output, reasoning, total])
}

fn model_of(o: &Value) -> Option<String> {
    for k in ["model", "model_name"] {
        if let Some(s) = o.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()) {
            return Some(s.to_string());
        }
    }
    o.get("metadata").and_then(|m| m.get("model")).and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from)
}

/// `codex-auto-review` is routed server-side; resolve it by date (newest first).
fn auto_review_model(ts: i64) -> String {
    const TABLE: &[(&str, &str)] = &[
        ("2026-07-30", "gpt-5.6-luna"),
        ("2026-03-05", "gpt-5.4"),
        ("2026-02-05", "gpt-5.3-codex"),
        ("2025-12-11", "gpt-5.2-codex"),
        ("2025-11-13", "gpt-5.1-codex"),
        ("2025-09-15", "gpt-5-codex"),
    ];
    let date = chrono::DateTime::from_timestamp_millis(ts).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
    TABLE.iter().find(|(d, _)| date.as_str() >= *d).map(|(_, m)| m.to_string()).unwrap_or_else(|| "gpt-5".into())
}

fn read_text(path: &Path) -> std::io::Result<Vec<u8>> {
    let bytes = std::fs::read(path)?;
    if path.extension().is_some_and(|e| e == "zst") {
        let mut out = Vec::new();
        let mut dec = zstd::stream::read::Decoder::new(&bytes[..])?;
        // A file being compressed concurrently may be truncated; keep what decodes.
        let _ = std::io::copy(&mut dec, &mut out);
        Ok(out)
    } else {
        Ok(bytes)
    }
}

pub fn parse_file(path: &Path, src: &Path) -> std::io::Result<FileData> {
    let bytes = read_text(path)?;
    let rel = path.strip_prefix(src).unwrap_or(path).to_string_lossy().replace('\\', "/");
    let session = rel.trim_end_matches(".zst").trim_end_matches(".jsonl").to_string();
    let mut fd = FileData { session, project: "unknown".into(), ..Default::default() };

    let mut prev_total: Option<Raw> = None;
    let mut current_model: Option<String> = None;
    let mut last_delta: Option<Raw> = None;
    let mut last_total: Option<Raw> = None;
    // response_id -> (event, thread total) awaiting a matching `compacted` record.
    let mut pending: HashMap<String, (Ev, Option<Raw>)> = HashMap::new();
    let mut compacted: HashSet<String> = HashSet::new();
    let mut emitted: HashSet<String> = HashSet::new();

    let interesting: [&[u8]; 6] = [
        b"\"session_meta\"", b"\"turn_context\"", b"\"token_count\"", b"\"token_usage_record\"",
        b"\"compacted\"", b"\"task_started\"",
    ];
    for (idx, line) in bytes.split(|&b| b == b'\n').enumerate() {
        if line.len() < 10 || !interesting.iter().any(|p| memchr::memmem::find(line, p).is_some()) {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(line) else { continue };
        let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
        let ts_str = v.get("timestamp").and_then(Value::as_str).unwrap_or("");
        let ts = parse_ts(ts_str);
        let payload = v.get("payload").cloned().unwrap_or(Value::Null);
        match ty {
            "session_meta" if idx == 0 => {
                fd.meta_id = payload.get("id").and_then(Value::as_str).map(String::from);
                fd.meta_ts = ts;
                if let Some(cwd) = payload.get("cwd").and_then(Value::as_str) {
                    fd.project = project_name(cwd);
                }
                let sub_parent = payload
                    .pointer("/source/subagent/thread_spawn/parent_thread_id")
                    .and_then(Value::as_str)
                    .map(String::from);
                fd.subagent = sub_parent.is_some() || payload.pointer("/source/subagent").is_some();
                fd.parent_id = payload.get("forked_from_id").and_then(Value::as_str).map(String::from).or(sub_parent);
            }
            "turn_context" => {
                if let Some(m) = model_of(&payload) {
                    current_model = Some(m);
                }
            }
            "event_msg" => {
                let pty = payload.get("type").and_then(Value::as_str).unwrap_or("");
                if pty == "task_started" {
                    if let Some(t) = ts {
                        fd.prompts.push(t);
                    }
                    continue;
                }
                if pty != "token_count" {
                    continue;
                }
                let Some(ts) = ts else { continue };
                let info = payload.get("info").cloned().unwrap_or(Value::Null);
                let total = info.get("total_token_usage").and_then(usage_of);
                let last = info.get("last_token_usage").and_then(usage_of);
                let advanced = total.is_none() || prev_total != total;
                let raw = if advanced { last } else { None }.or_else(|| {
                    total.map(|t| {
                        let p = prev_total.unwrap_or([0; 6]);
                        let mut d = [0; 6];
                        for i in 0..6 {
                            d[i] = t[i].saturating_sub(p[i]);
                        }
                        d
                    })
                });
                if total.is_some() {
                    prev_total = total;
                    last_total = total;
                }
                let Some(mut raw) = raw else { continue };
                raw[1] = raw[1].min(raw[0]);
                raw[2] = raw[2].min(raw[0] - raw[1]);
                if raw[0] == 0 && raw[1] == 0 && raw[2] == 0 && raw[3] == 0 && raw[4] == 0 {
                    continue;
                }
                if let Some(m) = model_of(&payload).or_else(|| model_of(&info)) {
                    current_model = Some(m);
                }
                let mut model = current_model.clone().unwrap_or_else(|| "gpt-5".into());
                if model == "codex-auto-review" {
                    model = auto_review_model(ts);
                }
                last_delta = Some(raw);
                fd.events.push(Ev { ts, model, raw, response_id: None, compaction: false });
            }
            "token_usage_record" => {
                let Some(ts) = ts else { continue };
                let Some(rid) = payload.get("response_id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else { continue };
                let Some(raw) = payload.get("usage").and_then(usage_of) else { continue };
                if raw.iter().all(|&x| x == 0) {
                    continue;
                }
                let thread = payload.get("thread_token_usage").and_then(usage_of);
                // Already counted by the advancing token_count that precedes a local compaction.
                if last_delta == Some(raw) || (thread.is_some() && thread == last_total) {
                    emitted.insert(rid.to_string());
                    continue;
                }
                let mut model = model_of(&payload).or_else(|| current_model.clone()).unwrap_or_else(|| "gpt-5".into());
                if model == "codex-auto-review" {
                    model = auto_review_model(ts);
                }
                let ev = Ev { ts, model, raw, response_id: Some(rid.to_string()), compaction: true };
                if compacted.contains(rid) && emitted.insert(rid.to_string()) {
                    fd.events.push(ev);
                } else {
                    pending.insert(rid.to_string(), (ev, thread));
                }
            }
            "compacted" => {
                let Some(rid) = payload.get("compaction_response_id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else { continue };
                compacted.insert(rid.to_string());
                if let Some((ev, _)) = pending.remove(rid) {
                    if emitted.insert(rid.to_string()) {
                        fd.events.push(ev);
                    }
                }
            }
            _ => {}
        }
    }
    fd.events.sort_by_key(|e| e.ts);
    Ok(fd)
}

/// Merges parsed rollouts: strips replayed parent prefixes from forks, then deduplicates.
pub fn merge(files: &[(PathBuf, FileData)], out: &mut HarnessData) {
    let by_meta: HashMap<&str, usize> = files
        .iter()
        .enumerate()
        .filter_map(|(i, (_, f))| f.meta_id.as_deref().map(|m| (m, i)))
        .collect();

    let mut seen: HashSet<String> = HashSet::new();
    let mut seen_tuple: HashSet<(i64, String, Raw)> = HashSet::new();
    for (i, (_, fd)) in files.iter().enumerate() {
        let mut skip = vec![false; fd.events.len()];
        if let Some(pid) = fd.parent_id.as_deref() {
            let forked_at = fd.meta_ts.unwrap_or(i64::MAX);
            let prefix: Vec<&Raw> = by_meta
                .get(pid)
                .filter(|&&j| j != i)
                .map(|&j| files[j].1.events.iter().filter(|e| !e.compaction && e.ts <= forked_at).map(|e| &e.raw).collect())
                .unwrap_or_default();
            let normal: Vec<usize> = (0..fd.events.len()).filter(|&k| !fd.events[k].compaction).collect();
            let mut k = 0;
            let mut anchored = false;
            for &ix in &normal {
                if k < prefix.len() && fd.events[ix].raw == *prefix[k] {
                    skip[ix] = true;
                    k += 1;
                    anchored = true;
                } else {
                    break;
                }
            }
            if !anchored && normal.len() >= 2 {
                // Rewritten burst: replayed events land within a second of each other.
                let e0 = &fd.events[normal[0]];
                let e1 = &fd.events[normal[1]];
                let near_meta = fd.meta_ts.is_some_and(|m| (e0.ts - m).abs() <= 1000);
                if near_meta && (0..=1000).contains(&(e1.ts - e0.ts)) {
                    skip[normal[0]] = true;
                    let mut prev = e0.ts;
                    for &ix in &normal[1..] {
                        let gap = fd.events[ix].ts - prev;
                        if !(0..=1000).contains(&gap) {
                            break;
                        }
                        skip[ix] = true;
                        prev = fd.events[ix].ts;
                    }
                }
            }
        }
        for (k, e) in fd.events.iter().enumerate() {
            if skip[k] {
                continue;
            }
            let fresh = match &e.response_id {
                Some(r) => seen.insert(r.clone()),
                None => seen_tuple.insert((e.ts, e.model.clone(), e.raw)),
            };
            if !fresh {
                continue;
            }
            let [input, cached, cw, output, reasoning, _] = e.raw;
            out.entries.push(Entry {
                ts: e.ts,
                model: e.model.clone(),
                provider: Some("openai".into()),
                session: fd.session.clone(),
                project: fd.project.clone(),
                usage: Usage {
                    input: input.saturating_sub(cached + cw),
                    output,
                    cache_read: cached,
                    cache_write: cw,
                    cache_write_1h: 0,
                    reasoning,
                },
                stored_cost: None,
            });
        }
        if !fd.subagent {
            let forked_at = if fd.parent_id.is_some() { fd.meta_ts.unwrap_or(i64::MIN) } else { i64::MIN };
            for &t in &fd.prompts {
                if t > forked_at {
                    out.prompts.push(Prompt { ts: t, session: fd.session.clone() });
                }
            }
        }
    }
}
