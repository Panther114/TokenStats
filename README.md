# TokenStats

TokenStats is a Windows desktop dashboard (Tauri 2) that merges local token usage from four coding-agent harnesses into one view: Claude Code, Codex, OpenCode (desktop app and CLI) and DeepSeek Harness. It reads the harnesses' own transcript files read-only and makes no network requests other than an optional daily price-table refresh.

## Dashboard Contents

- Overview for the selected range (7 days, 30 days, 90 days, 12 months, all time): total tokens with input, output and cache split; API-equivalent cost; sessions; messages (prompts and responses); active days; current and longest streak; peak hour; favorite model.
- One contribution-style heatmap per harness, switchable between tokens, cost and messages, with per-harness statistics.
- Cumulative API-equivalent spend per harness, daily tokens by harness, and activity by hour of day.
- Per-model table with token categories, list prices per million tokens and cost.
- Settings for theme (system, light, dark), heatmap and harness colors, week start, price refresh and full rescan.

## Data Sources

| Harness | Location (overrides) | Format |
|---|---|---|
| Claude Code | `~/.claude/projects/**/*.jsonl` (`CLAUDE_CONFIG_DIR`) | JSONL; deduplicated by message id and request id |
| Codex | `~/.codex/sessions`, `~/.codex/archived_sessions` (`CODEX_HOME`) | JSONL or zstd JSONL; cumulative `token_count` events converted to per-response deltas; forked rollouts de-replayed |
| OpenCode | `~/.local/share/opencode/opencode.db` (`OPENCODE_DATA_DIR`, `XDG_DATA_HOME`) | SQLite (opened read-only), v1 and v2 schemas, legacy JSON storage |
| DeepSeek Harness | `~/.dsh/sessions/<project>/<session>/session.v4.jsonl.zstd` (`DSH_HOME`) | multi-frame zstd JSONL shared by the app and web profiles |

The parsing and deduplication rules follow ccusage; per-day totals for Claude Code, Codex and OpenCode were verified against ccusage output on the development machine.

## Pricing

Costs are computed from public list prices: a LiteLLM snapshot embedded at build time (`src-tauri/resources/pricing.json`), with long-context tiers from models.dev. The app refreshes the table at most once a day. Cache reads and writes use their own rates, one-hour cache writes are billed at twice the input rate, and free promotional variants (`-free`) are priced as their paid counterparts. Local models and models without a published price are reported without cost.

## Building

```bash
pnpm install
pnpm pricing          # optional: rebuild the embedded price snapshot
pnpm tauri build      # produces the NSIS installer under src-tauri/target/release/bundle/nsis
```

`cargo run --bin tokenstats-cli` prints per-harness totals from the command line; `--json` prints the full report.
