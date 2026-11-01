//! Claude Code: `~/.claude/projects/**/*.jsonl` transcripts.
//!
//! Each assistant response is written as one line per content block and is copied again
//! into resumed-session files, so lines are deduplicated by (`message.id`, `requestId`).
//! Without a `requestId` the key adds the session, and the timestamp unless the id is a
//! real `msg_`/`resp_` response id. Sidechain replays of a parent message collapse into
//! the parent. The surviving copy is the non-sidechain one, then the highest token total.
use crate::model::{parse_ts, project_name, Entry, HarnessData, Prompt, Usage};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RawUsage {
    pub ts: i64,
    pub msg_id: Option<String>,
    pub req_id: Option<String>,
    pub session: Option<String>,
    pub model: String,
    pub usage: Usage,
    pub sidechain: bool,
    pub cost: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FileData {
    pub path_session: String,
    pub project: String,
    pub is_subagent: bool,
    pub usages: Vec<RawUsage>,
    /// (uuid, timestamp, sessionId) of user prompts.
    pub prompts: Vec<(String, i64, String)>,
}

pub fn roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(v) = std::env::var("CLAUDE_CONFIG_DIR") {
        for part in v.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let p = PathBuf::from(part);
            let p = if p.file_name().is_some_and(|n| n == "projects") { p.parent().map(Path::to_path_buf).unwrap_or(p) } else { p };
            if p.join("projects").is_dir() && !out.contains(&p) {
                out.push(p);
            }
        }
        return out;
    }
    let home = crate::model::home_dir();
    let xdg = std::env::var("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|_| home.join(".config"));
    for p in [xdg.join("claude"), home.join(".claude")] {
        if p.join("projects").is_dir() && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

pub fn discover_projects(projects: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    crate::scan::walk(projects, &mut |p| {
        if p.extension().is_some_and(|e| e == "jsonl") {
            files.push(p.to_path_buf());
        }
    });
    files.sort();
    files
}

/// (session id, is_subagent) derived from the path below `projects/`.
fn path_session(path: &Path) -> (String, bool) {
    let comps: Vec<String> = path.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    let Some(pi) = comps.iter().position(|c| c == "projects") else {
        return (stem(path), false);
    };
    let rel = &comps[pi + 1..];
    let n = rel.len();
    if n >= 4 && rel[n - 2] == "subagents" {
        return (rel[n - 3].clone(), true);
    }
    if n == 2 {
        return (stem(path), false);
    }
    if n >= 3 {
        return (rel[n - 2].clone(), false);
    }
    (stem(path), false)
}

fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

#[derive(Deserialize)]
struct Line {
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    #[serde(rename = "isSidechain")]
    is_sidechain: Option<bool>,
    #[serde(rename = "costUSD")]
    cost_usd: Option<f64>,
    cwd: Option<String>,
    message: Option<Msg>,
}

#[derive(Deserialize)]
struct Msg {
    id: Option<String>,
    model: Option<String>,
    usage: Option<RawTokens>,
}

#[derive(Deserialize)]
struct RawTokens {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_creation: Option<CacheCreation>,
}

#[derive(Deserialize)]
struct CacheCreation {
    ephemeral_5m_input_tokens: Option<u64>,
    ephemeral_1h_input_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct PromptLine {
    uuid: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
}

pub fn parse_file(path: &Path) -> std::io::Result<FileData> {
    let bytes = std::fs::read(path)?;
    let (path_session, is_subagent) = path_session(path);
    let mut fd = FileData { path_session, is_subagent, ..Default::default() };
    let usage_pat = memchr::memmem::Finder::new(b"\"usage\":{");
    let user_pat = memchr::memmem::Finder::new(b"\"type\":\"user\"");
    let tool_pat = memchr::memmem::Finder::new(b"\"tool_result\"");
    let meta_pat = memchr::memmem::Finder::new(b"\"isMeta\":true");
    let compact_pat = memchr::memmem::Finder::new(b"\"isCompactSummary\":true");
    let side_pat = memchr::memmem::Finder::new(b"\"isSidechain\":true");
    for line in bytes.split(|&b| b == b'\n') {
        if line.len() < 20 {
            continue;
        }
        if usage_pat.find(line).is_some() {
            let Ok(l) = serde_json::from_slice::<Line>(line) else { continue };
            if fd.project.is_empty() {
                if let Some(c) = &l.cwd {
                    fd.project = project_name(c);
                }
            }
            let Some(msg) = l.message else { continue };
            let Some(u) = msg.usage else { continue };
            let (Some(input), Some(output)) = (u.input_tokens, u.output_tokens) else { continue };
            let Some(ts) = l.timestamp.as_deref().and_then(parse_ts) else { continue };
            let model = msg.model.unwrap_or_default();
            if model == "<synthetic>" {
                continue;
            }
            let (c5, c1h) = match &u.cache_creation {
                Some(cc) => (cc.ephemeral_5m_input_tokens.unwrap_or(0), cc.ephemeral_1h_input_tokens.unwrap_or(0)),
                None => (u.cache_creation_input_tokens.unwrap_or(0), 0),
            };
            let nonempty = |s: Option<String>| s.filter(|v| !v.is_empty());
            fd.usages.push(RawUsage {
                ts,
                msg_id: nonempty(msg.id),
                req_id: nonempty(l.request_id),
                session: nonempty(l.session_id),
                model,
                usage: Usage {
                    input,
                    output,
                    cache_read: u.cache_read_input_tokens.unwrap_or(0),
                    cache_write: c5,
                    cache_write_1h: c1h,
                    reasoning: 0,
                },
                sidechain: l.is_sidechain.unwrap_or(false),
                cost: l.cost_usd,
            });
        } else if !fd.is_subagent
            && user_pat.find(line).is_some()
            && tool_pat.find(line).is_none()
            && meta_pat.find(line).is_none()
            && compact_pat.find(line).is_none()
            && side_pat.find(line).is_none()
        {
            let Ok(p) = serde_json::from_slice::<PromptLine>(line) else { continue };
            if fd.project.is_empty() {
                if let Some(c) = &p.cwd {
                    fd.project = project_name(c);
                }
            }
            let (Some(uuid), Some(ts)) = (p.uuid, p.timestamp.as_deref().and_then(parse_ts)) else { continue };
            fd.prompts.push((uuid, ts, p.session_id.unwrap_or_default()));
        }
    }
    if fd.project.is_empty() {
        fd.project = "unknown".into();
    }
    Ok(fd)
}

/// Merges parsed files (in sorted path order) into deduplicated entries.
pub fn merge(files: &[(PathBuf, FileData)], out: &mut HarnessData) {
    struct Kept {
        raw: RawUsage,
        session: String,
        project: String,
    }
    let mut kept: Vec<Kept> = Vec::new();
    let mut exact: HashMap<(String, String, String, i64), usize> = HashMap::new();
    let mut side: HashMap<(String, String), usize> = HashMap::new();

    for (_, fd) in files {
        for r in &fd.usages {
            let sess = r.session.clone().unwrap_or_else(|| fd.path_session.clone());
            let Some(id) = r.msg_id.clone() else {
                kept.push(Kept { raw: r.clone(), session: fd.path_session.clone(), project: fd.project.clone() });
                continue;
            };
            let key = match &r.req_id {
                Some(req) => (id.clone(), req.clone(), String::new(), 0),
                None => {
                    let real = id.starts_with("msg_") || id.starts_with("resp_");
                    (id.clone(), String::new(), sess.clone(), if real { 0 } else { r.ts })
                }
            };
            let mut hit = exact.get(&key).copied();
            if hit.is_none() {
                if let Some(&j) = side.get(&(id.clone(), sess.clone())) {
                    if kept[j].raw.sidechain || r.sidechain {
                        hit = Some(j);
                    }
                }
            }
            match hit {
                Some(j) => {
                    let ex = &kept[j].raw;
                    let replace = if ex.sidechain != r.sidechain {
                        ex.sidechain
                    } else {
                        r.usage.total() > ex.usage.total()
                    };
                    if replace {
                        kept[j].raw.usage = r.usage;
                        kept[j].raw.sidechain = r.sidechain;
                        kept[j].raw.model = r.model.clone();
                        kept[j].raw.cost = r.cost;
                    }
                    exact.entry(key).or_insert(j);
                }
                None => {
                    let j = kept.len();
                    kept.push(Kept { raw: r.clone(), session: fd.path_session.clone(), project: fd.project.clone() });
                    exact.insert(key, j);
                    side.entry((id, sess)).or_insert(j);
                }
            }
        }
    }

    out.entries.extend(kept.into_iter().map(|k| Entry {
        ts: k.raw.ts,
        model: k.raw.model,
        provider: None,
        session: k.session,
        project: k.project,
        usage: k.raw.usage,
        stored_cost: k.raw.cost,
    }));

    let mut seen = std::collections::HashSet::new();
    for (_, fd) in files {
        for (uuid, ts, _) in &fd.prompts {
            if seen.insert(uuid.clone()) {
                out.prompts.push(Prompt { ts: *ts, session: fd.path_session.clone() });
            }
        }
    }
}
