# BuildWatch 👀 — Universal File Watcher & Build Daemon

**The eyes that keep your builds fresh.**

BuildWatch is a standalone, general-purpose file watcher and build daemon that automatically rebuilds projects when source files change. Built in Rust with Facebook's Watchman for efficient file watching.

Designed for both human and AI-agent development workflows — eliminates stale-binary bugs and removes manual rebuild steps from the code/test loop.

## Status

**Pre-alpha** — Architecture spec complete, implementation scaffolded.

See [`docs/architecture.md`](docs/architecture.md) for the full specification.

## Quick Start

```bash
# Build from source
cargo build

# Auto-detect project and generate config
buildwatch init

# Start watching (daemon mode)
buildwatch haunt

# Run a fresh build artifact (blocks until build is ready)
wr my-target -- --some-flag
```

## Architecture

BuildWatch is a standalone tool in the NAVI Ecosystem. It knows nothing about NAVI — integration happens through a Skill adapter layer.

```
buildwatch init    → Auto-detect project, generate buildwatch.config.json
buildwatch haunt   → Start daemon, subscribe to Watchman, auto-rebuild on changes
wr <target>    → Freshness gate: block until build is ready, then exec
```

Core subsystems:
- **Watcher** — Watchman subscription management (async, via `watchman_client` crate)
- **Queue** — Priority-scored build queue with coalescing
- **Builder** — Child process execution with timeout, output capture, recovery
- **Detector** — Project type auto-detection from filesystem markers
- **State** — JSON files on disk for IPC (no sockets, no databases)

## Binary Names

BuildWatch ships two binaries:
- `buildwatch` — daemon and management CLI
- `wr` — freshness-gate wrapper

`wr` is intentionally short for fast agent/human typing in repeated run loops. If command collisions are observed in target environments, a compatibility alias (for example `bw` or `bwr`) can be added without removing `wr`.

## Prerequisites

- [Rust toolchain](https://rustup.rs/) (1.75+)
- [Watchman](https://facebook.github.io/watchman/docs/install) (file watching service)

## License

MIT
