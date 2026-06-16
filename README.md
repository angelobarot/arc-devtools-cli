# Arc DevTools CLI

A Rust CLI that drives [Arc](https://arc.net/) via the Chrome DevTools Protocol, relaunching Arc with remote debugging enabled when needed. It connects to your real Arc profile (your logins, your tabs).

```bash
arc-devtools navigate https://example.com
arc-devtools --target red-snake snapshot
arc-devtools --target red-snake evaluate "document.title"
```

## Why this exists

A fork of [opzero1/chrome-devtools-cli](https://github.com/opzero1/chrome-devtools-cli) (by Aero), specialized for Arc. Arc is Chromium, so the CDP layer is identical — only connecting differs:

- Arc has no in-app remote-debugging toggle and writes no `DevToolsActivePort` file.
- It must be launched with `--remote-debugging-port`, and ignores that flag if already running.

So `arc-devtools` probes the debug port and, if Arc isn't exposing it, quits and relaunches Arc (tabs/spaces restored), then connects via the live WebSocket URL.

## Installation

macOS with [Arc](https://arc.net/) installed.

### 1. Install Rust (provides `cargo`)

If you don't already have it:

```bash
# Official installer (recommended)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# …or via Homebrew
brew install rust
```

After rustup, restart your shell (or `source "$HOME/.cargo/env"`) so `cargo` is on your PATH. Check with `cargo --version`.

### 2. Install arc-devtools

```bash
# One-shot: install the latest + register the opencode skill (recommended)
curl -fsSL https://raw.githubusercontent.com/angelobarot/arc-devtools-cli/main/install.sh | sh

# …or just the binary (latest main)
cargo install --git https://github.com/angelobarot/arc-devtools-cli

# …or pin to a released version
cargo install --git https://github.com/angelobarot/arc-devtools-cli --tag v0.1.0

# …or from a local clone:  ./install.sh --local   (or)   cargo build --release
```

The `install.sh` script also runs step 3 for you (and `--force`-upgrades an existing install).

**No Rust?** Each [release](https://github.com/angelobarot/arc-devtools-cli/releases) ships prebuilt macOS binaries for Apple Silicon (`aarch64-apple-darwin`) and Intel (`x86_64-apple-darwin`). Download the matching `.tar.gz` and its `.sha256`, verify, extract, then move `arc-devtools` onto your PATH:

```bash
# Verify the download (run in the dir containing both files)
shasum -a 256 -c arc-devtools-v0.1.0-aarch64-apple-darwin.tar.gz.sha256

tar -xzf arc-devtools-v0.1.0-aarch64-apple-darwin.tar.gz
mkdir -p /usr/local/bin
mv arc-devtools-v0.1.0-aarch64-apple-darwin/arc-devtools /usr/local/bin/   # or ~/.cargo/bin
```

> macOS may block the unsigned binary on first run ("cannot be opened"). Clear the
> quarantine flag, then run it: `xattr -d com.apple.quarantine /usr/local/bin/arc-devtools`

> **No Rust on the target machine?** The binary is self-contained with no runtime
> dependencies. Build it once, then copy `target/release/arc-devtools` to any dir
> on PATH (e.g. `~/.cargo/bin` or `/usr/local/bin`) on another Mac of the same arch.

### 3. Install the opencode skill

The agent skill ships inside the binary. Install it into opencode and restart:

```bash
arc-devtools install            # → ~/.config/opencode/skills/arc-devtools/SKILL.md
arc-devtools install --dir /path/to/skills   # custom location
```

## How connection works

```
arc-devtools navigate https://example.com
        │
        ├─ Probe http://127.0.0.1:9223/json/version
        │     ├─ up   → use its webSocketDebuggerUrl
        │     └─ down → quit Arc (if running) + relaunch with --remote-debugging-port,
        │               wait for the port, then connect
        │
        └─ Daemon (Unix socket) holds the connection; later commands reuse it
```

- **Port:** `9223` by default. Override with `ARC_DEVTOOLS_PORT`.
- **Escape hatch:** pass `--ws-endpoint <url>` or `--user-data-dir <path>` to skip Arc management entirely and drive Chrome / any Chromium profile.

## A note on Arc Spaces

Arc Spaces aren't exposed by CDP. Only *loaded* tabs appear in `list-pages`; Arc sleeps inactive tabs, and a slept tab must be activated in Arc's UI before it shows up. There's no CDP way to switch spaces — do that in Arc, then `list-pages`.

## Target-first workflow

Every page-level command prints a friendly `[target:red-snake]` name (deterministic from the CDP target ID). Capture it from your first command and pass `--target` to stay pinned to the same tab.

```bash
arc-devtools navigate https://example.com      # → [target:red-snake]
arc-devtools --target red-snake snapshot
arc-devtools --target red-snake click "#submit"
```

Without `--target`, commands default to page index 0.

## Commands

| Group | Commands |
|-------|----------|
| Navigation | `navigate <url>` / `--back` / `--forward` / `--reload`, `new-page`, `close-page`, `select-page`, `list-pages` |
| Inspection | `snapshot`, `screenshot [--output] [--full-page]`, `evaluate <expr>`, `record-video --output <mp4>` (needs ffmpeg) |
| Diagnostics | `network [--navigate <url>] [--duration N] [--filter S]`, `console [--navigate <url>] [--duration N] [--level all\|warn\|error]`, `performance [--navigate <url>]` |
| Interaction | `click`, `fill`, `type-text`, `press-key`, `hover` |
| Other | `resize <w> <h>`, `wait-for <text> [--timeout ms]`, `install` |

## Global options

| Flag | Description |
|------|-------------|
| `--target <name>` | Target page by friendly name or raw ID |
| `--page <index>` | Target page by index |
| `--json` | JSON output |
| `--ws-endpoint <url>` | Explicit WebSocket URL (bypasses Arc management) |
| `--user-data-dir <path>` | Chrome/Chromium profile dir via `DevToolsActivePort` (bypasses Arc management) |
| `--daemon-idle-timeout <v>` | Daemon idle timeout: `30m`, `1h`, `300s`, `never` (env: `ARC_DEVTOOLS_DAEMON_IDLE_TIMEOUT`) |

## Daemon

- **Socket:** `$TMPDIR/arc-devtools-daemon.sock` (separate from a `chrome-devtools` daemon — both can run at once)
- **PID:** `$TMPDIR/arc-devtools-daemon.pid`
- **Idle timeout:** 5 minutes by default
- **Kill:** `pkill -f __daemon__`

## Credits & license

Forked from [opzero1/chrome-devtools-cli](https://github.com/opzero1/chrome-devtools-cli) by Aero. MIT licensed — see [LICENSE](LICENSE).
