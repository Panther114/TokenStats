<p align="center">
  <img src="src-tauri/icons/icon.png" width="140" alt="TokenStats icon">
</p>

<h1 align="center">TokenStats</h1>

<p align="center">
  Token usage and API-equivalent cost across every AI coding agent, in one local dashboard.
</p>

<p align="center">
  <a href="https://github.com/Panther114/TokenStats/releases/latest"><img src="https://img.shields.io/github/v/release/Panther114/TokenStats?label=release&color=e07a52" alt="Latest release"></a>
  <a href="https://github.com/Panther114/TokenStats/releases"><img src="https://img.shields.io/github/downloads/Panther114/TokenStats/total?color=e07a52" alt="Downloads"></a>
  <img src="https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-555" alt="Platform">
  <img src="https://img.shields.io/badge/built%20with-Tauri%202-24c8db" alt="Tauri 2">
</p>

<p align="center">
  <a href="https://github.com/Panther114/TokenStats/releases/latest"><b>Download for Windows</b></a>
</p>

<p align="center">
  <img src="docs/screenshot.png" alt="TokenStats dashboard" width="100%">
</p>

## Features

- **18 harnesses:** Claude Code (including subagents), Codex, OpenCode, Gemini CLI, Qwen Code, GitHub Copilot CLI, DeepSeek Harness, Amp, Goose, Kimi CLI, Grok CLI and more, detected automatically.
- **Cost:** API-equivalent spend from public list prices, including cache rates and long-context tiers.
- **Insight:** contribution heatmaps, daily tokens or cost per harness, cache hit rates, streaks, peak hours and a sortable per-model table.
- **Ranges:** 7 days, 30 days, 90 days, 1 year, all time, or a custom date range.
- **Private:** reads local log files read-only; the only network request is an optional daily price refresh.

## Installation

Download `TokenStats_<version>_x64-setup.exe` from [Releases](https://github.com/Panther114/TokenStats/releases/latest) and run it. No administrator rights are required.

## Building from Source

```bash
pnpm install
pnpm tauri build
```

Provider adapters, data locations and pricing rules are documented in [`src-tauri/src/providers/`](src-tauri/src/providers/). Parsing and deduplication follow [ccusage](https://github.com/ccusage/ccusage); provider logos come from [lobe-icons](https://github.com/lobehub/lobe-icons) (MIT).
