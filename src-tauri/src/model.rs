//! Normalized data model shared by all harness parsers.
use serde::{Deserialize, Serialize};

/// Harness identifiers. The numeric order is the display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Harness {
    Claude = 0,
    Codex = 1,
    OpenCode = 2,
    Dsh = 3,
}

impl Harness {
    pub const ALL: [Harness; 4] = [Harness::Claude, Harness::Codex, Harness::OpenCode, Harness::Dsh];

    pub fn key(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::OpenCode => "opencode",
            Harness::Dsh => "dsh",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Harness::Claude => "Claude Code",
            Harness::Codex => "Codex",
            Harness::OpenCode => "OpenCode",
            Harness::Dsh => "DeepSeek Harness",
        }
    }
}

/// Token counters in a normalized form:
/// - `input` excludes cached and cache-write tokens (fresh input only);
/// - `output` includes reasoning tokens; `reasoning` is informational;
/// - `cache_write` is 5-minute (or unspecified) cache creation, `cache_write_1h` the 1-hour tier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cache_write_1h: u64,
    pub reasoning: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write + self.cache_write_1h
    }

    pub fn is_zero(&self) -> bool {
        self.total() == 0 && self.reasoning == 0
    }

    pub fn add(&mut self, o: &Usage) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write += o.cache_write;
        self.cache_write_1h += o.cache_write_1h;
        self.reasoning += o.reasoning;
    }
}

/// One billable model response after harness-specific deduplication.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub harness: Harness,
    pub ts: i64,
    pub model: String,
    pub provider: Option<String>,
    pub session: String,
    pub project: String,
    pub usage: Usage,
    /// Cost recorded by the harness itself (used when positive).
    pub stored_cost: Option<f64>,
}

/// One user prompt (a message typed by the user, not a tool result).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Prompt {
    pub harness: Harness,
    pub ts: i64,
    pub session: String,
}

/// Merged output of one harness scan.
#[derive(Default)]
pub struct HarnessData {
    pub entries: Vec<Entry>,
    pub prompts: Vec<Prompt>,
    pub roots: Vec<String>,
    pub files: usize,
    pub errors: Vec<String>,
}

/// Parses an RFC 3339 timestamp into epoch milliseconds.
pub fn parse_ts(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s.trim()).ok().map(|d| d.timestamp_millis())
}

/// Last path component of a working directory, used as the project label.
pub fn project_name(cwd: &str) -> String {
    let trimmed = cwd.trim_end_matches(['\\', '/']);
    let name = trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed);
    if name.is_empty() { "unknown".to_string() } else { name.to_string() }
}

pub fn home_dir() -> std::path::PathBuf {
    for var in ["USERPROFILE", "HOME"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                return v.into();
            }
        }
    }
    ".".into()
}
