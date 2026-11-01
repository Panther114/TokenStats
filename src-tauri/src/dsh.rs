//! DeepSeek Harness: `$DSH_HOME/sessions/<project>/<session>/session.v<N>.jsonl[.zstd]`.
//!
//! The session root is shared by the desktop (app) and web profiles. Logs are append-only
//! zstd streams written as one frame per flush. Each `assistant/message` event carries
//! `data.usage` (`inputTokens` excludes cache reads; `totalTokens` = input + output + cache
//! read) and the routed model in `data.message.source`. Seeded child sessions may copy the
//! parent's history, so responses are deduplicated by message id.
use crate::model::{project_name, Entry, Harness, HarnessData, Prompt, Usage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Resp {
    pub id: String,
    pub ts: i64,
    pub model: String,
    pub provider: String,
    pub usage: Usage,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FileData {
    pub session: String,
    pub project: String,
    pub subagent: bool,
    pub responses: Vec<Resp>,
    pub prompts: Vec<(String, i64)>,
}

pub fn home() -> Option<PathBuf> {
    let h = std::env::var("DSH_HOME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::model::home_dir().join(".dsh"));
    h.join("sessions").is_dir().then_some(h)
}

pub fn discover(home: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    crate::scan::walk(&home.join("sessions"), &mut |p| {
        let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if n.starts_with("session") && (n.ends_with(".jsonl") || n.ends_with(".jsonl.zstd")) && !n.contains(".tmp") {
            files.push(p.to_path_buf());
        }
    });
    files.sort();
    files
}

/// Decodes a multi-frame zstd log. Frame boundaries are located at magic numbers; a chunk
/// that fails to decode is extended to the next candidate (magic bytes can occur inside
/// compressed data). A truncated trailing frame (log being written) is ignored.
fn decode_frames(b: &[u8]) -> Vec<u8> {
    const MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
    let mut idx: Vec<usize> = memchr::memmem::find_iter(b, &MAGIC).collect();
    idx.push(b.len());
    let mut out = Vec::with_capacity(b.len() * 4);
    let (mut start, mut k) = (0usize, 1usize);
    if idx.first() != Some(&0) {
        return out;
    }
    while start < b.len() && k < idx.len() {
        match zstd::stream::decode_all(&b[start..idx[k]]) {
            Ok(d) => {
                out.extend_from_slice(&d);
                start = idx[k];
                k += 1;
            }
            Err(_) => k += 1,
        }
    }
    out
}

pub fn parse_file(path: &Path) -> std::io::Result<FileData> {
    let raw = std::fs::read(path)?;
    let text = if path.extension().is_some_and(|e| e == "zstd") { decode_frames(&raw) } else { raw };
    let mut fd = FileData { project: "unknown".into(), ..Default::default() };
    let pats: [&[u8]; 3] = [b"\"type\":\"session\"", b"\"type\":\"assistant/message\"", b"\"type\":\"user/message\""];
    for line in text.split(|&b| b == b'\n') {
        if line.len() < 10 || !pats.iter().any(|p| memchr::memmem::find(&line[..line.len().min(64)], p).is_some()) {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(line) else { continue };
        match v.get("type").and_then(Value::as_str) {
            Some("session") => {
                fd.session = v.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
                if let Some(cwd) = v.get("cwd").and_then(Value::as_str) {
                    fd.project = project_name(cwd);
                }
                fd.subagent = v.get("origin").and_then(Value::as_str) == Some("subagent")
                    || v.get("delegationDepth").and_then(Value::as_u64).unwrap_or(0) > 0;
            }
            Some("assistant/message") => {
                let Some(data) = v.get("data") else { continue };
                let Some(u) = data.get("usage").filter(|u| u.is_object()) else { continue };
                let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                let usage = Usage {
                    input: n("inputTokens"),
                    output: n("outputTokens"),
                    cache_read: n("cacheReadTokens"),
                    cache_write: n("cacheWriteTokens"),
                    cache_write_1h: 0,
                    reasoning: n("reasoningTokens"),
                };
                if usage.is_zero() {
                    continue;
                }
                let msg = data.get("message").cloned().unwrap_or(Value::Null);
                let src = msg.get("source").cloned().unwrap_or(Value::Null);
                let s = |o: &Value, k: &str| o.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                let ts = v.get("time").and_then(Value::as_i64).unwrap_or(0);
                fd.responses.push(Resp {
                    id: s(&msg, "id"),
                    ts,
                    model: s(&src, "model"),
                    provider: s(&src, "provider"),
                    usage,
                });
            }
            Some("user/message") => {
                let Some(data) = v.get("data") else { continue };
                if data.pointer("/source/kind").and_then(Value::as_str) != Some("user") {
                    continue;
                }
                let id = data.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
                let ts = v.get("time").and_then(Value::as_i64).unwrap_or(0);
                fd.prompts.push((id, ts));
            }
            _ => {}
        }
    }
    if fd.session.is_empty() {
        fd.session = path.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    }
    Ok(fd)
}

pub fn merge(files: &[(PathBuf, FileData)], out: &mut HarnessData) {
    let mut seen = HashSet::new();
    let mut seen_prompts = HashSet::new();
    // Parents before seeded children, so copied history is attributed to the parent.
    let mut order: Vec<&FileData> = files.iter().map(|(_, f)| f).collect();
    order.sort_by_key(|f| f.subagent);
    for fd in order {
        for r in &fd.responses {
            if !r.id.is_empty() && !seen.insert(r.id.clone()) {
                continue;
            }
            out.entries.push(Entry {
                harness: Harness::Dsh,
                ts: r.ts,
                model: if r.model.is_empty() { "unknown".into() } else { r.model.clone() },
                provider: (!r.provider.is_empty()).then(|| r.provider.clone()),
                session: fd.session.clone(),
                project: fd.project.clone(),
                usage: r.usage,
                stored_cost: None,
            });
        }
        if fd.subagent {
            continue;
        }
        for (id, ts) in &fd.prompts {
            if id.is_empty() || seen_prompts.insert(id.clone()) {
                out.prompts.push(Prompt { harness: Harness::Dsh, ts: *ts, session: fd.session.clone() });
            }
        }
    }
}
