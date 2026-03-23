# BuildWatch: Universal File Watcher & Build Daemon

## Architecture Specification v0.1

**Status:** Approved — Ready for Implementation  
**Author:** Eric (design direction) / Claude (spec authorship)  
**Date:** 2026-03-22  
**Reference Implementation:** [Poltergeist](https://github.com/steipete/poltergeist) by Peter Steinberger

---

## 1. Purpose & Positioning

BuildWatch is a standalone, general-purpose file watcher and build daemon that automatically rebuilds projects when source files change. It is designed to accelerate both human and AI-agent development workflows by eliminating stale-binary bugs and removing manual rebuild steps from the code/test loop.

### 1.1 Relationship to NAVI Ecosystem

BuildWatch is a **standalone tool** that NAVI and Helm command as a Skill. It is not coupled to the NAVI runtime, NAVI's World Model, or any NAVI-specific protocol. The integration boundary is:

```
┌─────────────────────────────────────────┐
│  NAVI / Helm                            │
│  ┌───────────────┐                      │
│  │ BuildWatch Skill  │ ← Skill adapter      │
│  │ (future)      │   translates BuildWatch  │
│  └───────┬───────┘   state → Proposals, │
│          │            Attributes, etc.   │
│          ▼                               │
│    CLI invocation / state file reads     │
└──────────┬──────────────────────────────┘
           │
           ▼
┌─────────────────────────────────────────┐
│  BuildWatch (standalone binary)             │
│  - Watches files via Watchman           │
│  - Manages build queue                  │
│  - Exposes state via JSON files         │
│  - Provides CLI interface               │
└─────────────────────────────────────────┘
```

BuildWatch must be useful to any developer on any project, independent of NAVI. NAVI-specific behaviors (surfacing build status as Proposals, feeding into the Conscious Process, etc.) live entirely in the Skill adapter layer and are out of scope for this spec.

### 1.2 Design Principles

1. **General purpose first.** Works on any project, any language, any build system.
2. **Zero-config where possible.** `buildwatch init` auto-detects project type and generates configuration.
3. **Agent-native.** Structured output for machine consumption. Human-friendly output for terminal use. Caller detection where feasible.
4. **Invisible until needed.** Runs as a background daemon. The developer (or agent) never thinks about build freshness — it just works.
5. **Simple state, simple IPC.** JSON files on disk. No databases, no custom protocols, no daemon-to-daemon RPC.

### 1.3 Why Rust

| Concern | Rust | Go | TypeScript |
|---|---|---|---|
| Watchman bindings | Official `watchman_client` crate (Facebook-maintained, 2.6M+ downloads, async/tokio) | No maintained bindings | Official `fb-watchman` npm package |
| Daemon suitability | No GC pauses, low memory footprint, predictable latency | GC pauses acceptable but present | Requires Node.js/Bun runtime |
| Binary distribution | Single static binary, no runtime deps | Single static binary | Requires bundler or runtime |
| Cross-platform | Excellent (Windows, macOS, Linux) | Excellent | Excellent via Node/Bun |
| CLI ecosystem | `clap`, `serde`, `tokio`, `tracing` — mature | `cobra`, `viper` — mature | `commander`, `zod` — mature |

The decisive factor is the Watchman client. Facebook maintains the Rust crate as a first-party binding with full async subscription support, state-enter/state-leave primitives, and type-safe query results. The Go ecosystem has no maintained equivalent.

### 1.4 Naming

"BuildWatch" continues the spectral theme established by Poltergeist. CLI commands:

- `buildwatch` — the daemon/manager binary
- `wr` — the freshness-gate execution wrapper (equivalent to Poltergeist's `polter`)

---

## 2. Architecture Overview

### 2.1 Component Map

```
buildwatch/
├── src/
│   ├── main.rs              # CLI entry point (clap)
│   ├── bin/wr.rs            # Freshness gate runner
│   ├── lib.rs               # Shared library root
│   ├── daemon.rs            # Background daemon lifecycle
│   ├── watcher.rs           # Watchman integration layer
│   ├── queue.rs             # Build queue with priority scoring
│   ├── builder.rs           # Build execution engine
│   ├── detector.rs          # Project type auto-detection
│   ├── config.rs            # Configuration parsing (serde/JSON)
│   ├── state.rs             # Shared state management (JSON files)
│   ├── notifier.rs          # System notification dispatch
│   └── output.rs            # Terminal formatting & agent detection
├── tests/
├── docs/
│   └── architecture.md      # This file
├── Cargo.toml
├── AGENTS.md
└── README.md
```

### 2.2 Runtime Architecture

```
                    ┌──────────────────────────────────────┐
                    │           Watchman Server             │
                    │  (external process, manages inotify/  │
                    │   kqueue/ReadDirectoryChangesW)       │
                    └──────────────┬───────────────────────┘
                                   │ subscription events
                                   ▼
┌──────────────────────────────────────────────────────────┐
│                    BuildWatch Daemon                          │
│                                                          │
│  ┌─────────┐    ┌──────────┐    ┌──────────┐            │
│  │ Watcher  │───▶│  Queue   │───▶│ Builder  │            │
│  │ (async)  │    │ (priority│    │ (spawns  │            │
│  │          │    │  sorted) │    │  child   │            │
│  └─────────┘    └──────────┘    │  process) │            │
│                                  └─────┬────┘            │
│                                        │                 │
│  ┌─────────┐    ┌──────────┐           │                 │
│  │Notifier │◀───│  State   │◀──────────┘                 │
│  │(system  │    │ Manager  │   build result              │
│  │ notifs) │    │ (JSON)   │                             │
│  └─────────┘    └──────────┘                             │
│                      ▲                                   │
└──────────────────────┼───────────────────────────────────┘
                       │ reads state files
              ┌────────┴────────┐
              │   wr (runner)   │
              │ Blocks until    │
              │ build is fresh, │
              │ then exec's     │
              │ the target      │
              └─────────────────┘
```

### 2.3 Process Model

Each project root gets its own BuildWatch daemon process. Daemons are independent — one crashing never affects another. The shared state directory (`/tmp/buildwatch/` on Unix, `%TEMP%\buildwatch\` on Windows) allows any process to query the status of all active daemons.

```
/tmp/buildwatch/
├── <project-hash>/
│   ├── daemon.json          # PID, start time, heartbeat
│   ├── state.json           # Target states, build history
│   ├── build.log            # Rolling build output (weekly rotation)
│   └── lock                 # Advisory lock file
├── <another-project-hash>/
│   └── ...
└── global.json              # Registry of all active projects
```

---

## 3. Core Subsystems

### 3.1 Watcher (watcher.rs)

Wraps the `watchman_client` crate. Responsible for:

1. **Connecting to Watchman.** Uses `Connector::new().connect()` to establish an async connection.
2. **Resolving the project root.** Calls `client.resolve_root()` which triggers Watchman's recursive crawl if the path isn't already watched.
3. **Subscribing to changes.** Creates a Watchman subscription with file expression filters based on project type and config.
4. **Debouncing and settling.** Configurable settling delay to batch rapid file changes.
5. **Filtering.** Respects `.gitignore`, custom ignore patterns, and Watchman's built-in VCS-aware filtering.

#### State Assertions for Build Coordination

BuildWatch uses Watchman's state-enter/state-leave mechanism to prevent cascading rebuilds during a build. When a build starts, BuildWatch asserts `buildwatch.build` state. The subscription's `defer` field pauses change delivery until the state is released.

```
File change detected
  → Queue build
  → state-enter "buildwatch.build"
  → Execute build command (may create output files)
  → state-leave "buildwatch.build"
  → Deferred changes delivered (only source changes, not build artifacts)
```

### 3.2 Build Queue (queue.rs)

Manages pending builds with priority scoring. Only one build runs at a time per daemon. The queue holds at most one pending build per target — if a new change arrives while a build is pending, the pending entry is updated (not duplicated).

#### Priority Scoring

```
priority = base_priority       (from config, per-target)
         + recency_boost       (targets built recently get slight deprioritization)
         + file_affinity       (targets whose source patterns match the changed files)
```

#### Build Coalescing

```
t0: file A changes    → enqueue Target X
t1: file B changes    → Target X already pending, update change set
t2: settling expires  → dequeue Target X, execute build with full change set
t3: file C changes    → Target X build running, enqueue new pending
t4: build completes   → check pending queue, rebuild if pending
```

### 3.3 Builder (builder.rs)

Executes build commands as child processes. Responsibilities:

1. **Process spawning.** Runs the target's `build_command` as a child process with configurable working directory and environment variables.
2. **Output capture.** Streams stdout/stderr to the rolling build log and (if terminal attached) to the console.
3. **Timeout enforcement.** Kills builds exceeding `build_timeout_seconds` (default: 300).
4. **Result recording.** Writes build outcome to state.
5. **Failure recovery.** Subsequent file changes after a failure trigger automatic retry. After `max_consecutive_failures` (default: 10), auto-retry stops; manual `buildwatch build <target>` or config change resets.

#### Build Execution Flow

```
fn execute_build(target):
    1. Acquire project lock (advisory, non-blocking)
    2. Assert Watchman state "buildwatch.build"
    3. Spawn child process: target.build_command
    4. Stream output to log file + optional terminal
    5. Wait for exit (with timeout)
    6. Release Watchman state "buildwatch.build"
    7. Update state.json with result
    8. Dispatch notification (success/failure)
    9. Return BuildResult
```

### 3.4 Project Detector (detector.rs)

Auto-detects project type from filesystem markers. Generates `buildwatch.config.json` during `buildwatch init`.

#### Detection Matrix

| Marker File(s) | Project Type | Default Build Command | Watch Extensions |
|---|---|---|---|
| `Cargo.toml` | Rust | `cargo build` | `.rs`, `.toml` |
| `go.mod` | Go | `go build ./...` | `.go`, `.mod`, `.sum` |
| `Package.swift` | Swift | `swift build` | `.swift` |
| `package.json` + `tsconfig.json` | TypeScript | `npm run build` | `.ts`, `.tsx`, `.js`, `.jsx` |
| `CMakeLists.txt` | CMake/C++ | `cmake --build build` | `.c`, `.cpp`, `.h`, `.hpp`, `.cmake` |
| `Makefile` | Make | `make` | `*` |
| `pyproject.toml` / `setup.py` | Python | (none) | `.py` |
| `Dockerfile` | Docker | `docker build .` | `Dockerfile`, `.dockerignore` |
| `.sln` / `.csproj` | .NET | `dotnet build` | `.cs`, `.csproj`, `.sln` |

Per-directory detection: if `buildwatch init` detects nested project markers in a monorepo, it generates per-directory configs rather than one combined config.

For project types with multiple targets (Cargo workspaces, Go modules, CMake), the detector introspects further — parsing `Cargo.toml` for `[[bin]]` sections, running `go list ./...`, or reading `CMakeCache.txt`.

### 3.5 Configuration (config.rs)

Config file: `buildwatch.config.json` at the project root. Hot-reloaded on change (no restart required).

**Decision locked:** JSON format (not TOML) for Poltergeist parity and serde-native simplicity.

```jsonc
{
  "version": 1,
  "settling_delay_ms": 200,
  "build_timeout_seconds": 300,
  "notifications": {
    "enabled": true,
    "on_success": true,
    "on_failure": true,
    "sound": true
  },
  "targets": [
    {
      "name": "navi-server",
      "build_command": "go build -o bin/navi ./cmd/navi",
      "output_path": "bin/navi",
      "working_directory": ".",
      "watch_extensions": [".go", ".mod", ".sum"],
      "watch_paths": ["cmd/", "internal/", "pkg/"],
      "exclude_paths": ["vendor/", "bin/"],
      "environment": { "CGO_ENABLED": "0" },
      "priority": 10,
      "enabled": true,
      "post_build": null,
      "auto_restart": false
    }
  ],
  "global_excludes": [
    ".git/", "node_modules/", "__pycache__/",
    "target/", "bin/", ".next/", "dist/"
  ]
}
```

### 3.6 State Manager (state.rs)

All writes are atomic (write to temp file, then rename). The state file is the IPC mechanism — no sockets, no pipes, no HTTP.

#### daemon.json

```jsonc
{
  "pid": 12345,
  "project_root": "/home/user/projects/navi",
  "project_hash": "a1b2c3d4",
  "started_at": "2026-03-22T10:00:00Z",
  "heartbeat": "2026-03-22T10:05:32Z",
  "version": "0.1.0",
  "watchman_version": "2024.01.01.00"
}
```

#### state.json

```jsonc
{
  "targets": {
    "navi-server": {
      "status": "ready",
      "last_build": {
        "started_at": "2026-03-22T10:04:50Z",
        "finished_at": "2026-03-22T10:05:02Z",
        "duration_ms": 12000,
        "exit_code": 0,
        "output_path": "bin/navi",
        "trigger_files": ["internal/server/handler.go"],
        "error_summary": null
      },
      "build_count": 47,
      "failure_count": 2,
      "consecutive_failures": 0
    }
  },
  "queue": { "pending": [], "current": null },
  "updated_at": "2026-03-22T10:05:02Z"
}
```

#### Heartbeat and Liveness

Daemon writes `heartbeat` every 5 seconds. Liveness check: `if now - heartbeat > 15s → daemon is dead`. The `wr` runner, `buildwatch status`, and future NAVI Skill adapter all use this mechanism.

#### Log Rotation

Build logs rotate weekly. On rotation, the current `build.log` is renamed to `build.log.<YYYY-WW>` and a new empty file is created. On daemon restart, the current log is truncated. Retained logs are not automatically cleaned in v0.1 — manual cleanup or future policy.

### 3.7 Notifier (notifier.rs)

Platform-native notifications:

- **macOS:** `osascript` / `terminal-notifier`
- **Linux:** `notify-send` (libnotify)
- **Windows:** PowerShell toast notifications

Content: `"✓ navi-server built (12.0s)"` or `"✗ navi-server failed (exit 1)"` with first error line.

### 3.8 Output Formatter (output.rs)

#### Agent Detection

```rust
fn is_agent_caller() -> bool {
    env::var("CLAUDE_CODE").is_ok()
        || env::var("CURSOR_SESSION").is_ok()
        || env::var("CODEX_SESSION").is_ok()
        || env::var("NAVI_AGENT").is_ok()
        || env::var("HELM_SESSION").is_ok()
        || !atty::is(atty::Stream::Stdout)
        || env::var("BUILDWATCH_AGENT_MODE").is_ok()
}
```

Agent mode → structured JSON lines. Human mode → colored terminal with spinners.

---

## 4. CLI Interface

### 4.1 `buildwatch` Commands

```
buildwatch init [--auto] [--type TYPE]       Auto-detect project, generate config
buildwatch haunt [--foreground] [--target N] Start daemon (alias: start)
buildwatch stop                              Stop daemon for this project
buildwatch status [--verbose] [--json]       Show all active daemons
buildwatch build [TARGET]                    Trigger manual build
buildwatch clean                             Remove state, stop daemon
buildwatch config                            Show resolved configuration
buildwatch log [TARGET]                      Tail build log
buildwatch version                           Show version
```

### 4.2 `wr` Runner (Freshness Gate)

```
wr <TARGET> [-- <ARGS>...]

Behavior:
  status == "ready"    → exec output_path with ARGS immediately
  status == "building" → poll state.json every 200ms, then exec
  status == "failed"   → print error summary, exit with build's exit code
  no daemon running    → print hint, exit 1

Options:
  --timeout <SECS>     Max wait time (default: 600)
  --no-wait            Fail immediately if not ready
  --json               Structured output
```

**Fuzzy target matching:** `wr` supports substring and fuzzy matching on target names. `wr navi` matches `navi-server` if it's the only target containing "navi". Ambiguous matches print all candidates and exit.

Agent-specific: when no daemon is running and agent is detected, outputs structured JSON hint:

```json
{"error": "no_daemon", "hint": "Run 'buildwatch haunt'", "project_root": "/path"}
```

---

## 5. Lifecycle Flows

### 5.1 First-Time Setup

```
buildwatch init → scan markers → introspect targets → write buildwatch.config.json
buildwatch haunt → connect Watchman → subscribe → write daemon.json + state.json → background
```

### 5.2 Steady-State Build Loop

```
Editor saves file → Watchman detects → Watcher receives (filtered, debounced)
  → Queue evaluates targets (priority, coalescing)
  → Builder spawns command (state-enter, capture output, wait)
  → Build completes (state-leave, update state.json, notify)
  → Deferred changes released → loop if pending
```

### 5.3 Agent Workflow

```
Agent edits file → BuildWatch rebuilds in background (invisible)
  → Agent runs: wr navi-server -- --test-mode
  → wr reads state.json, blocks if building, execs when ready
  → If failed: agent reads error, fixes code, cycle repeats
```

---

## 6. Error Handling & Recovery

### 6.1 Build Failures

- `state.json` updated with `status: "failed"`, `exit_code`, `error_summary` (first 5 stderr lines).
- Next file change triggers automatic retry.
- After 10 consecutive failures: auto-retry stops. Manual `buildwatch build` resets.

### 6.2 Watchman Disconnection

- Exponential backoff reconnection (1s, 2s, 4s, ..., max 60s).
- During disconnection: targets marked `status: "stale"`.
- On reconnect: `since` query from last clock value catches missed changes.

### 6.3 Daemon Crash Recovery

- On startup, BuildWatch checks for stale `daemon.json` (heartbeat > 15s old).
- Cleans up stale state, starts fresh.
- `wr` checks heartbeat before reading state — stale → "daemon not running".

### 6.4 Concurrent Build Protection

- One build at a time per daemon.
- Advisory lock file prevents duplicate daemons per project.
- Changes during build are coalesced into pending queue.

---

## 7. Configuration Hot-Reload

BuildWatch watches `buildwatch.config.json` via secondary Watchman subscription. On change:

1. Parse and validate new config (reject invalid, log errors).
2. Diff targets: added, removed, modified.
3. Removed → cancel subscriptions, remove from queue.
4. Added → create new subscriptions.
5. Modified → update subscription expressions, rebuild if `build_command` changed.

No daemon restart required.

---

## 8. Future: NAVI Skill Adapter (Out of Scope)

Informational only — the Skill adapter bridges BuildWatch into NAVI:

- **Daemon lifecycle:** Start/stop BuildWatch for projects NAVI works on.
- **State translation:** Read `state.json` → NAVI Attributes on project entity.
- **Proposal generation:** Build failures → Proposals in NAVI's queue.
- **Conscious Process:** Build events feed Perceive → Interpret → Contextualize.

Integration surface:

```
NAVI World Model
  └── Project Entity
      ├── build_status: "ready" | "building" | "failed"
      ├── last_build_duration_ms: 12000
      ├── last_build_error: null | "error text"
      └── buildwatch_daemon_pid: 12345
```

---

## 9. Build & Distribution

```bash
cargo build                    # Debug
cargo build --release          # Release (stripped, LTO)
cargo test                     # All tests
cargo clippy                   # Lint
```

Cross-compilation targets: `x86_64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`.

Distribution: GitHub Releases (pre-built binaries), Homebrew tap, `cargo install buildwatch-build`.

### 9.1 Key Dependencies

| Crate | Purpose |
|---|---|
| `watchman_client` 0.9 | Watchman IPC (Facebook-maintained) |
| `tokio` | Async runtime |
| `clap` 4 | CLI argument parsing |
| `serde` / `serde_json` | Config and state serialization |
| `tracing` | Structured logging |
| `notify-rust` | Cross-platform system notifications |
| `chrono` | Timestamps |
| `sha2` | Project path hashing |
| `tempfile` | Atomic file writes |

### 2.3 Process Model

Each project root gets its own BuildWatch daemon process. Daemons are independent — one crashing never affects another. The shared state directory (`/tmp/buildwatch/` on Unix, `%TEMP%\buildwatch\` on Windows) allows any process to query the status of all active daemons.

```
/tmp/buildwatch/
├── <project-hash>/
│   ├── daemon.json          # PID, start time, heartbeat
│   ├── state.json           # Target states, build history
│   ├── build.log            # Rolling build output (weekly rotation)
│   └── lock                 # Advisory lock file
├── <another-project-hash>/
│   └── ...
└── global.json              # Registry of all active projects
```

---

## 3. Core Subsystems

### 3.1 Watcher (watcher.rs)

Wraps the `watchman_client` crate. Responsible for:

1. **Connecting to Watchman.** Uses `Connector::new().connect()` to establish an async connection.
2. **Resolving the project root.** Calls `client.resolve_root()` which triggers Watchman's recursive crawl if needed.
3. **Subscribing to changes.** Creates a subscription with file expression filters based on project type and config.
4. **Debouncing and settling.** Configurable settling delay to batch rapid file changes.
5. **Filtering.** Respects `.gitignore`, custom ignore patterns, and Watchman's built-in VCS-aware filtering.

---

## 10. Resolved Decisions

These were open questions, now locked:

| # | Question | Decision |
|---|---|---|
| 1 | Config format | `buildwatch.config.json` (JSON, not TOML) |
| 2 | Settling delay default | 200ms universal default, adjustable per-target |
| 3 | Log rotation | Weekly rotation; truncation on daemon restart for v0.1 |
| 4 | Monorepo handling | Per-project-directory configs (not combined) |
| 5 | `wr` fuzzy matching | Yes, included in v0.1 |
| 6 | Status panel (TUI) | Deferred — JSON state files + `buildwatch status` sufficient |

---

## 11. Revision History

| Version | Date | Description |
|---|---|---|
| 0.1 | 2026-03-22 | Initial draft with all decisions locked |

#### State Assertions for Build Coordination

BuildWatch uses Watchman's state-enter/state-leave mechanism to prevent cascading rebuilds during a build. When a build starts, BuildWatch asserts `buildwatch.build` state. The subscription's `defer` field pauses change delivery until the state is released.

```
File change detected
  → Queue build
  → state-enter "buildwatch.build"
  → Execute build command (may create output files)
  → state-leave "buildwatch.build"
  → Deferred changes delivered (only source changes, not build artifacts)
```

### 3.2 Build Queue (queue.rs)

Manages pending builds with priority scoring. Only one build runs at a time per daemon. The queue holds at most one pending build per target — new changes coalesce into the pending entry.

#### Priority Scoring

```
priority = base_priority          (from config, per-target)
         + recency_boost          (recently-built targets slightly deprioritized)
         + file_affinity          (targets whose source patterns match changed files)
```

#### Build Coalescing

```
t0: file A changes    → enqueue Target X
t1: file B changes    → Target X already pending, update change set
t2: settling expires  → dequeue Target X, execute build
t3: file C changes    → Target X building, enqueue new pending
t4: build completes   → check pending queue, rebuild if pending
```

### 3.3 Builder (builder.rs)

Executes build commands as child processes:

1. **Process spawning.** Runs the target's `build_command` with configurable working directory and environment.
2. **Output capture.** Streams stdout/stderr to rolling build log and optional terminal.
3. **Timeout enforcement.** Kills builds exceeding `build_timeout_seconds` (default: 300).
4. **Result recording.** Writes outcome to state (success/failure, duration, exit code, error summary).
5. **Failure recovery.** Subsequent file changes after failure trigger automatic retry. After `max_consecutive_failures` (default: 10), auto-retry stops. Manual `buildwatch build <target>` or config change resets.

#### Build Execution Flow

```
fn execute_build(target):
    1. Acquire project lock (advisory, non-blocking)
    2. Assert Watchman state "buildwatch.build"
    3. Spawn child process: target.build_command
    4. Stream output to log + optional terminal
    5. Wait for exit (with timeout)
    6. Release Watchman state "buildwatch.build"
    7. Update state.json with result
    8. Dispatch notification
    9. Return BuildResult
```

### 3.4 Project Detector (detector.rs)

Auto-detects project type from filesystem markers. Generates `buildwatch.config.json` during `buildwatch init`. Per-project-directory scope — each directory gets its own config.

#### Detection Matrix

| Marker File(s) | Project Type | Default Build Command | Default Watch Extensions |
|---|---|---|---|
| `Cargo.toml` | Rust | `cargo build` | `.rs`, `.toml` |
| `go.mod` | Go | `go build ./...` | `.go`, `.mod`, `.sum` |
| `Package.swift` | Swift | `swift build` | `.swift` |
| `package.json` + `tsconfig.json` | TypeScript/Node | `npm run build` | `.ts`, `.tsx`, `.js`, `.jsx` |
| `CMakeLists.txt` | CMake/C/C++ | `cmake --build build` | `.c`, `.cpp`, `.h`, `.hpp`, `.cmake` |
| `Makefile` / `makefile` | Make | `make` | `*` (watch all) |
| `pyproject.toml` / `setup.py` | Python | (none — interpreted) | `.py` |
| `Dockerfile` | Docker | `docker build .` | `Dockerfile`, `.dockerignore` |
| `.sln` / `.csproj` | .NET/C# | `dotnet build` | `.cs`, `.csproj`, `.sln` |

Multiple markers can coexist (e.g., a monorepo). The detector creates a target for each detected project type.

#### Target Discovery

For project types with multiple build targets (Cargo workspaces, CMake, Go modules), the detector introspects further:

- **Cargo:** Parse `Cargo.toml` for `[[bin]]` sections and workspace members.
- **Go:** Run `go list ./...` to enumerate packages.
- **CMake:** Parse `CMakeLists.txt` for `add_executable`/`add_library`, or read `CMakeCache.txt`.

### 3.5 Configuration (config.rs)

Configuration lives in `buildwatch.config.json` at the project root. Generated by `buildwatch init`, editable by hand. BuildWatch watches this file and hot-reloads on change (no restart required).

```jsonc
{
  "version": 1,
  "settling_delay_ms": 200,
  "build_timeout_seconds": 300,
  "notifications": {
    "enabled": true,
    "on_success": true,
    "on_failure": true,
    "sound": true
  },
  "targets": [
    {
      "name": "navi-server",
      "build_command": "go build -o bin/navi ./cmd/navi",
      "output_path": "bin/navi",
      "working_directory": ".",
      "watch_extensions": [".go", ".mod", ".sum"],
      "watch_paths": ["cmd/", "internal/", "pkg/"],
      "exclude_paths": ["vendor/", "bin/", "testdata/"],
      "environment": { "CGO_ENABLED": "0" },
      "priority": 10,
      "enabled": true,
      "post_build": null,
      "auto_restart": false
    }
  ],
  "global_excludes": [
    ".git/", "node_modules/", "__pycache__/",
    "target/", "bin/", ".next/", "dist/"
  ]
}
```

### 3.6 State Manager (state.rs)

All writes are atomic (write to temp file, then rename). State files are the IPC mechanism — no sockets, no pipes, no HTTP servers.

#### daemon.json

```jsonc
{
  "pid": 12345,
  "project_root": "/home/user/projects/navi",
  "project_hash": "a1b2c3d4",
  "started_at": "2026-03-22T10:00:00Z",
  "heartbeat": "2026-03-22T10:05:32Z",   // Updated every 5 seconds
  "version": "0.1.0",
  "watchman_version": "2024.01.01.00"
}
```

#### state.json

```jsonc
{
  "targets": {
    "navi-server": {
      "status": "ready",    // "ready" | "building" | "failed" | "pending" | "stale"
      "last_build": {
        "started_at": "2026-03-22T10:04:50Z",
        "finished_at": "2026-03-22T10:05:02Z",
        "duration_ms": 12000,
        "exit_code": 0,
        "output_path": "bin/navi",
        "trigger_files": ["internal/server/handler.go"],
        "error_summary": null
      },
      "build_count": 47,
      "failure_count": 2,
      "consecutive_failures": 0
    }
  },
  "queue": {
    "pending": [],
    "current": null
  },
  "updated_at": "2026-03-22T10:05:02Z"
}
```

#### Heartbeat and Liveness

The daemon writes `heartbeat` to `daemon.json` every 5 seconds. Any process determines liveness by:

```
if now - daemon.heartbeat > 15 seconds → daemon is dead (stale state)
```

The `wr` runner, `buildwatch status`, and NAVI's Skill adapter all use this mechanism.

#### Log Rotation

`build.log` rotates weekly. On rotation, the current log is renamed to `build.log.<YYYY-WNN>` and a fresh log is created. On daemon restart, the current log is truncated regardless of rotation schedule.

### 3.7 Notifier (notifier.rs)

Platform-native system notifications:

- **macOS:** `osascript` / `terminal-notifier`
- **Linux:** `notify-send` (libnotify)
- **Windows:** PowerShell toast notifications

Notification content:
- Success: `"✓ navi-server built (12.0s)"`
- Failure: `"✗ navi-server build failed (exit 1)"` + first error line

### 3.8 Output Formatter (output.rs)

#### Agent Detection

```rust
fn is_agent_caller() -> bool {
    env::var("CLAUDE_CODE").is_ok()
        || env::var("CURSOR_SESSION").is_ok()
        || env::var("CODEX_SESSION").is_ok()
        || env::var("NAVI_AGENT").is_ok()
        || env::var("HELM_SESSION").is_ok()
        || !atty::is(atty::Stream::Stdout)
        || env::var("BUILDWATCH_AGENT_MODE").is_ok()
}
```

Agent mode → structured JSON lines. Human mode → colored terminal with spinners.

---

## 4. CLI Interface

### 4.1 Commands

| Command | Description |
|---|---|
| `buildwatch init [--auto] [--type TYPE]` | Auto-detect project, generate `buildwatch.config.json` |
| `buildwatch haunt [--foreground] [--target NAME]` | Start daemon (alias: `start`) |
| `buildwatch stop` | Stop daemon for this project |
| `buildwatch status [--verbose] [--json]` | Show status of all active daemons |
| `buildwatch build [TARGET]` | Trigger manual build |
| `buildwatch clean` | Remove state files, stop daemon |
| `buildwatch config` | Show resolved configuration |
| `buildwatch log [TARGET]` | Tail build log |

### 4.2 The `wr` Runner (Freshness Gate)

```
wr <TARGET> [-- <ARGS>...]

OPTIONS:
    --timeout <SECS>    Max wait for build (default: 600)
    --no-wait           Fail if not ready (don't block)
    --json              Output status as JSON
```

**Behavior:**

1. Read `state.json` for the target
2. `status == "ready"` + `exit_code == 0` → exec the binary with ARGS
3. `status == "building"` → poll every 200ms until done, then exec or report failure
4. `status == "failed"` → print error summary, exit with build's exit code
5. No daemon → structured hint: `"Run 'buildwatch haunt' to start"`

Supports fuzzy target name matching (e.g., `wr navi` matches `navi-server`).

Agent-specific: when agent detected and no daemon running, outputs JSON:
```json
{"error": "no_daemon", "hint": "Run 'buildwatch haunt'", "project_root": "/path"}
```

---

## 5. Lifecycle Flows

### 5.1 First-Time Setup

```
buildwatch init  →  scan markers  →  introspect targets  →  write buildwatch.config.json
buildwatch haunt →  connect Watchman  →  subscribe  →  write daemon.json + state.json  →  background
```

### 5.2 Steady-State Build Loop

```
Editor saves file → Watchman detects → Watcher receives (filtered, debounced)
  → Queue evaluates targets (priority, coalescing)
  → Builder: state-enter → spawn → capture output → wait → state-leave
  → State update → Notification → Check queue for pending → loop
```

### 5.3 Agent Workflow

```
Agent edits source → BuildWatch rebuilds in background (invisible)
  → Agent runs: wr navi-server -- --test-mode
  → building? poll+wait. ready? exec. failed? print error, exit non-zero.
```

---

## 6. Configuration Hot-Reload

BuildWatch watches its own `buildwatch.config.json` via a secondary Watchman subscription. On change:

1. Parse new config, validate against schema
2. Diff targets: identify added, removed, modified
3. Removed → cancel subscriptions, remove from queue
4. Added → create subscriptions
5. Modified → update subscription expressions, rebuild if `build_command` changed
6. Update in-memory config. No daemon restart.

---

## 7. Error Handling & Recovery

### 7.1 Build Failures

- State updated with `status: "failed"`, `exit_code`, `error_summary` (first 5 stderr lines)
- Subsequent file changes trigger automatic retry
- After `max_consecutive_failures` (10), auto-retry stops. Manual `buildwatch build` resets.

### 7.2 Watchman Disconnection

- Reconnect with exponential backoff (1s → 2s → 4s → ... → max 60s)
- During disconnect: targets marked `status: "stale"`
- On reconnect: full `since` query from last known clock to catch up

### 7.3 Daemon Crash Recovery

- On startup, BuildWatch checks for existing `daemon.json` with stale heartbeat
- Stale state cleaned up, fresh start
- `wr` checks heartbeat before reading state — stale = "daemon not running"

### 7.4 Concurrent Build Protection

- One build at a time per daemon
- Advisory lock file prevents duplicate daemons per project
- Changes during active build coalesce into pending queue

---

## 8. Future: NAVI Skill Adapter (Out of Scope)

Informational only — describes how a Skill adapter bridges BuildWatch into NAVI:

- **Daemon lifecycle:** Start/stop BuildWatch for projects NAVI works on
- **State translation:** Read `state.json`, surface as NAVI Attributes on project entity
- **Proposal generation:** Build failures → Proposals in NAVI's Proposal Queue
- **Conscious Process:** Feed build events into Perceive → Interpret → Contextualize
- **Config generation:** `buildwatch init` on new projects, customize via NAVI knowledge

```
NAVI World Model
  └── Project Entity
      ├── Attribute: build_status = "ready" | "building" | "failed"
      ├── Attribute: last_build_duration_ms = 12000
      ├── Attribute: last_build_error = null | "error text"
      └── Attribute: buildwatch_daemon_pid = 12345
```

---

## 9. Build & Distribution

```bash
cargo build                                          # Debug
cargo build --release                                # Release (stripped, LTO)
cargo build --release --target x86_64-unknown-linux-gnu
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-pc-windows-msvc
```

Distribution channels: GitHub Releases (pre-built binaries), Homebrew tap, `cargo install buildwatch-build`.

### Dependencies

| Crate | Purpose |
|---|---|
| `watchman_client` | Watchman IPC (Facebook-maintained) |
| `tokio` | Async runtime |
| `clap` | CLI argument parsing |
| `serde` / `serde_json` | Serialization |
| `tracing` | Structured logging |
| `notify-rust` | System notifications |
| `ctrlc` | Graceful shutdown |
| `chrono` | Timestamps |
| `sha2` | Path hashing |
| `tempfile` | Atomic writes |
| `atty` | Terminal detection |
| `anyhow` / `thiserror` | Error handling |

---

## 10. Locked Decisions

Resolved from Open Questions during spec review:

| # | Question | Decision | Rationale |
|---|---|---|---|
| 1 | Config file format | `buildwatch.config.json` (JSON) | Poltergeist parity, serde-native, universal tooling |
| 2 | Settling delay default | 200ms universal default | Per-target override available in config |
| 3 | Log rotation | Weekly auto-rotation; truncate on daemon restart | `build.log.<YYYY-WNN>` naming. v0.1 truncation on restart is acceptable |
| 4 | Monorepo handling | Per-project-directory configs | Each directory with markers gets its own `buildwatch.config.json` and daemon |
| 5 | Fuzzy target matching in `wr` | Yes, include in v0.1 | Essential for agent ergonomics |
| 6 | Status panel (TUI) | Deferred | JSON state files + `buildwatch status` sufficient for v0.1 |

---

## 11. Revision History

| Version | Date | Description |
|---|---|---|
| 0.1 | 2026-03-22 | Initial draft — all architectural decisions locked |
