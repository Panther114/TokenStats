//! Provider registry: the catalog of supported harness kinds and the user's configured
//! provider instances.
//!
//! A provider instance is one data source of a given kind (for example "Claude Code" reading
//! `~/.claude`, or a second Claude Code instance reading a work profile directory). Instances
//! are stored in `providers.json` in the app data directory; an empty `paths` list means the
//! kind's default locations.
pub mod amp;
pub mod codebuff;
pub mod copilot;
pub mod droid;
pub mod gemini;
pub mod goose;
pub mod grok;
pub mod hermes;
pub mod kilo;
pub mod kimi;
pub mod openclaw;
pub mod pi;
pub mod qwen;
pub mod zcode;

use crate::model::HarnessData;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Serialize)]
pub struct KindInfo {
    pub key: &'static str,
    pub name: &'static str,
    /// Icon id understood by the dashboard.
    pub icon: &'static str,
    /// Default brand-derived accent color.
    pub color: &'static str,
    /// Human-readable default data location.
    pub location: &'static str,
    /// Environment variable that overrides the default location, if any.
    pub env: &'static str,
}

pub const KINDS: &[KindInfo] = &[
    KindInfo { key: "claude", name: "Claude Code", icon: "claudecode", color: "#d97757", location: "~/.claude/projects", env: "CLAUDE_CONFIG_DIR" },
    KindInfo { key: "codex", name: "Codex", icon: "codex", color: "#10a37f", location: "~/.codex/sessions, ~/.codex/archived_sessions", env: "CODEX_HOME" },
    KindInfo { key: "opencode", name: "OpenCode", icon: "opencode", color: "#8b5cf6", location: "~/.local/share/opencode/opencode.db", env: "OPENCODE_DATA_DIR" },
    KindInfo { key: "dsh", name: "DeepSeek Harness", icon: "deepseek", color: "#4d6bfe", location: "~/.dsh/sessions", env: "DSH_HOME" },
    KindInfo { key: "gemini", name: "Gemini CLI", icon: "geminicli", color: "#4285f4", location: "~/.gemini/tmp", env: "GEMINI_DIR" },
    KindInfo { key: "qwen", name: "Qwen Code", icon: "qwen", color: "#615ced", location: "~/.qwen/projects", env: "QWEN_DATA_DIR" },
    KindInfo { key: "copilot", name: "GitHub Copilot CLI", icon: "githubcopilot", color: "#8b949e", location: "~/.copilot/session-state", env: "COPILOT_HOME" },
    KindInfo { key: "pi", name: "Pi", icon: "pi", color: "#a3a3a3", location: "~/.pi/agent/sessions", env: "PI_AGENT_DIR" },
    KindInfo { key: "amp", name: "Amp", icon: "amp", color: "#f34e3f", location: "~/.local/share/amp/threads", env: "AMP_DATA_HOME" },
    KindInfo { key: "kilo", name: "Kilo CLI", icon: "kilocode", color: "#e5c100", location: "~/.local/share/kilo/kilo.db", env: "KILO_DATA_DIR" },
    KindInfo { key: "goose", name: "Goose", icon: "goose", color: "#64748b", location: "~/.local/share/goose/sessions/sessions.db", env: "GOOSE_PATH_ROOT" },
    KindInfo { key: "droid", name: "Factory Droid", icon: "factory", color: "#f97316", location: "~/.factory/sessions", env: "FACTORY_DIR" },
    KindInfo { key: "kimi", name: "Kimi CLI", icon: "kimi", color: "#0ea5e9", location: "~/.kimi", env: "KIMI_SHARE_DIR" },
    KindInfo { key: "codebuff", name: "Codebuff", icon: "codebuff", color: "#22c55e", location: "~/.config/manicode/projects", env: "CODEBUFF_DATA_DIR" },
    KindInfo { key: "hermes", name: "Hermes Agent", icon: "hermesagent", color: "#a855f7", location: "~/.hermes/state.db", env: "HERMES_HOME" },
    KindInfo { key: "openclaw", name: "OpenClaw", icon: "openclaw", color: "#ef4444", location: "~/.openclaw/agents", env: "OPENCLAW_DIR" },
    KindInfo { key: "grok", name: "Grok CLI", icon: "grok", color: "#737373", location: "~/.grok/sessions", env: "GROK_HOME" },
    KindInfo { key: "zcode", name: "Z Code", icon: "zai", color: "#2563eb", location: "~/.zcode/cli/db/db.sqlite", env: "ZCODE_HOME" },
];

pub fn kind_info(key: &str) -> Option<&'static KindInfo> {
    KINDS.iter().find(|k| k.key == key)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    pub kind: String,
    pub name: String,
    /// Custom data locations; empty means the kind's defaults.
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub color: Option<String>,
}

fn yes() -> bool {
    true
}

/// Default locations for a kind (environment overrides are honored by each adapter).
pub fn default_roots(kind: &str) -> Vec<PathBuf> {
    match kind {
        "claude" => crate::claude::roots(),
        "codex" => crate::codex::homes(),
        "opencode" => crate::opencode::data_dirs(),
        "dsh" => crate::dsh::home().into_iter().collect(),
        "pi" => pi::default_roots(),
        "gemini" => gemini::default_roots(),
        "qwen" => qwen::default_roots(),
        "amp" => amp::default_roots(),
        "copilot" => copilot::default_roots(),
        "kilo" => kilo::default_roots(),
        "goose" => goose::default_roots(),
        "droid" => droid::default_roots(),
        "kimi" => kimi::default_roots(),
        "codebuff" => codebuff::default_roots(),
        "hermes" => hermes::default_roots(),
        "openclaw" => openclaw::default_roots(),
        "grok" => grok::default_roots(),
        "zcode" => zcode::default_roots(),
        _ => Vec::new(),
    }
}

/// Roots for a configured instance: its custom paths, or the kind's defaults.
pub fn roots_for(cfg: &ProviderConfig) -> Vec<PathBuf> {
    if cfg.paths.iter().any(|p| !p.trim().is_empty()) {
        cfg.paths.iter().map(|p| p.trim()).filter(|p| !p.is_empty()).map(expand_home).collect()
    } else {
        default_roots(&cfg.kind)
    }
}

fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        crate::model::home_dir().join(rest)
    } else if p == "~" {
        crate::model::home_dir()
    } else {
        PathBuf::from(p)
    }
}

/// Uncached full scan for kinds without an incremental parser.
pub fn scan_simple(kind: &str, roots: &[PathBuf], out: &mut HarnessData) {
    match kind {
        "pi" => pi::scan(roots, out),
        "gemini" => gemini::scan(roots, out),
        "qwen" => qwen::scan(roots, out),
        "amp" => amp::scan(roots, out),
        "copilot" => copilot::scan(roots, out),
        "kilo" => kilo::scan(roots, out),
        "goose" => goose::scan(roots, out),
        "droid" => droid::scan(roots, out),
        "kimi" => kimi::scan(roots, out),
        "codebuff" => codebuff::scan(roots, out),
        "hermes" => hermes::scan(roots, out),
        "openclaw" => openclaw::scan(roots, out),
        "grok" => grok::scan(roots, out),
        "zcode" => zcode::scan(roots, out),
        _ => out.errors.push(format!("unsupported provider kind '{kind}'")),
    }
}

/// True when any default location of the kind exists on this machine.
pub fn detected(kind: &str) -> bool {
    default_roots(kind).iter().any(|p| p.exists())
}

/// First-run configuration: the four original harnesses plus every other detected kind.
pub fn default_config() -> Vec<ProviderConfig> {
    KINDS
        .iter()
        .enumerate()
        .filter(|(i, k)| *i < 4 || detected(k.key))
        .map(|(_, k)| ProviderConfig { id: k.key.to_string(), kind: k.key.to_string(), name: k.name.to_string(), paths: Vec::new(), enabled: true, color: None })
        .collect()
}

#[derive(Serialize)]
pub struct CatalogEntry {
    #[serde(flatten)]
    pub info: KindInfo,
    pub detected: bool,
    pub default_paths: Vec<String>,
}

pub fn catalog() -> Vec<CatalogEntry> {
    KINDS
        .iter()
        .map(|k| {
            let roots = default_roots(k.key);
            CatalogEntry {
                info: *k,
                detected: roots.iter().any(|p| p.exists()),
                default_paths: roots.iter().map(|p| p.display().to_string()).collect(),
            }
        })
        .collect()
}
