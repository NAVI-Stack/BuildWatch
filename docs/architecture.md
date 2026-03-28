# BuildWatch Architecture

## Architecture Specification v0.1

Date: 2026-03-22  
Status: Pre-alpha scaffold

## 1. Purpose & Positioning

BuildWatch is a standalone file watcher and build daemon that keeps artifacts fresh for humans and agents.

### 1.1 Scope

- Per-project daemon lifecycle (`watch`, `stop`, `status`)
- Watchman-backed file change subscriptions
- Targeted build orchestration with queue coalescing
- Freshness-gate runtime (`wr`)
- JSON state files for inter-process coordination

### 1.2 Non-goals (v0.1)

- Embedded library integration with NAVI runtime
- Remote/distributed build orchestration
- Rich TUI dashboard

### 1.3 Naming

`buildwatch` is explicit about behavior: watch files and keep build outputs current.  
`wr` remains the short freshness-gate runner for fast repeated invocation loops.

## 2. Architecture Overview

### 2.1 Component Map

- `main.rs`: CLI entrypoint and command routing
- `daemon.rs`: foreground/background lifecycle and orchestration loop
- `watcher.rs`: Watchman connection and subscriptions
- `queue.rs`: coalescing + priority build queue
- `builder.rs`: process execution, logging, timeouts, result recording
- `state.rs`: atomic JSON state, lock file, global registry
- `notifier.rs`: desktop notifications
- `detector.rs`: project type detection and initial config synthesis
- `config.rs`: config load/write/override behavior
- `bin/wr.rs`: freshness gate and target execution

### 2.2 State Layout

```text
/tmp/buildwatch/ (or %TEMP%/buildwatch on Windows)
├── <project-hash>/
│   ├── daemon.json
│   ├── state.json
│   ├── build.log
│   └── lock
└── global.json
```

## 3. Core Subsystems

### 3.1 Watcher

- Connects with `watchman_client`
- Resolves project root
- Subscribes to file changes with suffix + directory excludes
- Uses `buildwatch.build` state assertions for deferred events during builds
- Reconnects with exponential backoff when subscription channel closes

### 3.2 Queue

- One pending entry per target
- Coalesces repeated changes into a single pending build
- Uses target priority + file affinity scoring
- Guarantees single active build per daemon

### 3.3 Builder

- Spawns target command in configured working directory and env
- Captures both stdout and stderr into `build.log`
- Rotates logs weekly (`build.log.<YYYY-Www>`)
- Enforces timeout and records structured results to state

### 3.4 State

- Atomic writes (`tempfile` + rename)
- Liveness from heartbeat timestamps
- Advisory lock file prevents duplicate daemons per project
- Global registry tracks active project daemons

### 3.5 Notifications

- Uses `notify-rust` for cross-platform desktop notifications
- Success/failure dispatch is best-effort and non-fatal

## 4. CLI Interface

### 4.1 Core Commands

| Command | Purpose |
|---|---|
| `buildwatch init` | Detect project, generate `buildwatch.config.json` |
| `buildwatch watch` | Start watcher/build daemon |
| `buildwatch start` | Alias for `watch` |
| `buildwatch stop` | Stop daemon for project |
| `buildwatch status` | Show active daemon and target status |
| `buildwatch build [target]` | Trigger manual build |
| `buildwatch log [target]` | Tail build log (optionally target-filtered) |
| `buildwatch clean` | Remove project state |

### 4.2 `watch` Runtime Overrides

- `--target <name>` filters active targets at runtime
- `--settling <ms>` overrides debounce delay at runtime

### 4.3 `wr` Freshness Gate

`wr <target> [-- <args>...]`

- Validates daemon liveness
- Resolves target with fuzzy matching
- Blocks on `building` / `pending` unless `--no-wait`
- Executes output binary only after successful fresh build

## 5. Lifecycle Flows

### 5.1 Init

`init -> detect -> generate config -> write config`

### 5.2 Daemon Loop

`watch -> lock -> connect -> subscribe -> queue events -> settle -> build -> heartbeat`

Additional runtime controls:
- Ctrl+C graceful shutdown
- config hot-reload polling
- watcher reconnect backoff

### 5.3 Stop / Cleanup

- Send termination signal
- release lock
- unregister project from global registry
- remove project state directory

## 6. Error Handling & Recovery

- Build failures mark target `failed` with concise error summary
- Watchman disconnect triggers reconnect attempts with capped backoff
- Stale heartbeat marks daemon dead for `wr` and `status`
- Stale lock PID is reclaimable on next daemon start

## 7. Configuration Hot-Reload

The daemon reloads `buildwatch.config.json` when modification time changes:

1. load/parse new config
2. apply if valid, keep old config on parse failure
3. update settling delay and target behavior without daemon restart

## 8. Future: NAVI Skill Adapter (Out of Scope)

- Translate high-level NAVI run/build intents to BuildWatch targets
- Integrate status surfacing and execution orchestration

## 9. Build & Distribution

### 9.1 Key Dependencies

| Crate | Purpose |
|---|---|
| `watchman_client` | Watchman IPC |
| `tokio` | Async runtime |
| `clap` | CLI parsing |
| `serde` / `serde_json` | Config/state serialization |
| `tracing` | Structured logging |
| `notify-rust` | Cross-platform notifications |
| `ctrlc` | Signal-triggered shutdown |
| `chrono` | Time and heartbeat |
| `sha2` | Project hashing |
| `tempfile` | Atomic file writes |
| `anyhow` / `thiserror` | Error handling |

## 10. Locked Decisions

| # | Decision |
|---|---|
| 1 | Config format: JSON (`buildwatch.config.json`) |
| 2 | Settling delay default: 200ms |
| 3 | Log rotation: weekly |
| 4 | Monorepo model: per-project daemon/config |
| 5 | Fuzzy target matching in `wr`: yes |
| 6 | TUI dashboard: deferred |

## 11. Revision History

| Version | Date | Description |
|---|---|---|
| 0.1 | 2026-03-22 | Consolidated architecture baseline |
