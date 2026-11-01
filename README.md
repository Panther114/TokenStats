# TokenStats

TokenStats is a Windows desktop dashboard (Tauri 2) that merges local token usage from coding-agent harnesses into one view: Claude Code, Codex, OpenCode, DeepSeek Harness, Gemini CLI, Qwen Code, GitHub Copilot CLI and others. It reads each harness's own transcript files read-only and makes no network requests other than an optional daily price-table refresh.

## Dashboard Contents

- Overview for the selected range (7 days, 30 days, 90 days, 12 months, all time): total tokens with input, output and cache split; API-equivalent cost; sessions; messages (prompts and responses); active days; current and longest streak; peak hour; favorite model.
- One contribution-style heatmap per provider, switchable between tokens, cost and messages, with per-provider statistics: sessions, messages, average cache hit rate, active days, peak hour and top model.
- Cumulative API-equivalent spend per provider, daily tokens by provider, and activity by hour of day.
- Per-model table with token categories, list prices per million tokens and cost.
- Settings for providers, theme (system, light, dark), heatmap colors, week start (Monday by default), price refresh and full rescan.

The average cache hit rate is the share of prompt tokens served from cache over the selected range: cache read / (fresh input + cache read + cache write).

## Supported Providers

| Provider | Default location | Override |
|---|---|---|
| Claude Code | `~/.claude/projects` | `CLAUDE_CONFIG_DIR` |
| Codex | `~/.codex/sessions`, `~/.codex/archived_sessions` | `CODEX_HOME` |
| OpenCode (desktop and CLI) | `~/.local/share/opencode/opencode.db` | `OPENCODE_DATA_DIR`, `XDG_DATA_HOME` |
| DeepSeek Harness (app and web profiles) | `~/.dsh/sessions` | `DSH_HOME` |
| Gemini CLI | `~/.gemini/tmp` | see adapter |
| Qwen Code | `~/.qwen/projects` | see adapter |
| GitHub Copilot CLI | `~/.copilot/session-state` | `COPILOT_HOME` |
| Pi | `~/.pi/agent/sessions` | see adapter |
| Amp | `~/.local/share/amp/threads` | see adapter |
| Kilo CLI | `~/.local/share/kilo/kilo.db` | see adapter |
| Goose | `~/.local/share/goose/sessions/sessions.db` | see adapter |
| Factory Droid | `~/.factory/sessions` | see adapter |
| Kimi CLI | `~/.kimi` | see adapter |
| Codebuff | `~/.config/manicode/projects` | see adapter |
| Hermes Agent | `~/.hermes/state.db` | `HERMES_HOME` |
| OpenClaw | `~/.openclaw/agents` | see adapter |
| Grok CLI | `~/.grok/sessions` | see adapter |
| Z Code | `~/.zcode/cli/db/db.sqlite` | see adapter |

Adapters live in `src-tauri/src/providers/`; each documents its exact discovery rules and record format. Parsing and deduplication follow [ccusage](https://github.com/ccusage/ccusage). Per-day totals for Claude Code, Codex and OpenCode were verified against ccusage output.

## Configuring Providers

On first start, TokenStats enables Claude Code, Codex, OpenCode and DeepSeek Harness, plus every other supported harness whose data directory exists. In **Settings → Providers** a provider can be renamed, recolored, disabled, removed, or pointed at custom data paths (separate several paths with `;`). **Add** creates a new provider of any supported kind, for example a second Claude Code instance reading another profile directory. **Reset** restores the detected defaults. The configuration is stored in `%LOCALAPPDATA%\app.tokenstats.desktop\providers.json`.

## Pricing

Costs are computed from public list prices: a LiteLLM snapshot embedded at build time (`src-tauri/resources/pricing.json`), with long-context tiers from models.dev. The app refreshes the table at most once a day. Cache reads and writes use their own rates, one-hour cache writes are billed at twice the input rate, and free promotional variants (`-free`) are priced as their paid counterparts. Local models and models without a published price are reported without cost.

## Icons

Provider logos come from [lobe-icons](https://github.com/lobehub/lobe-icons) (MIT). Factory Droid and Codebuff use their GitHub organization avatars. Product names and logos are trademarks of their respective owners.

## Building

```bash
pnpm install
pnpm pricing          # optional: rebuild the embedded price snapshot
pnpm tauri build      # produces the NSIS installer under src-tauri/target/release/bundle/nsis
```

`cargo run --bin tokenstats-cli` prints per-provider totals from the command line; `--json` prints the full report and `--all` scans every supported kind.
