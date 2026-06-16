---
name: arc-devtools
description: Use when automating the Arc (or Chrome) browser with the arc-devtools CLI — navigation, screenshots, accessibility snapshots, JavaScript evaluation, clicks, form fills, keyboard input, or page inspection. Prefer this over screenshot-based MCP browser tools.
---

# Arc DevTools CLI

`arc-devtools` is a Rust binary that drives the Arc browser via the Chrome DevTools Protocol, connecting to the user's own Arc with their own credentials. Prefer it over MCP browser tools when a text snapshot or a direct CDP action is enough.

## Prerequisites

Arc and the `arc-devtools` binary installed. The CLI manages Arc automatically:
- If Arc isn't exposing its debug port, the CLI **quits and relaunches Arc** with remote debugging (Arc restores your tabs/spaces). Arc has no in-app debug toggle, so a relaunch is the only way to enable it — expect a one-time browser restart.
- It then starts the CDP daemon and ensures at least one page exists.

Connection is via Arc's live WebSocket URL on port 9223 (override with `ARC_DEVTOOLS_PORT`). To drive Chrome (or any Chromium profile) instead, pass `--user-data-dir` or `--ws-endpoint`.

## ⚠️ CRITICAL — Image dimension rules (prevents session crashes)

Every image in the conversation (screenshots, user-pasted images, tool results) is sent as base64 to the LLM API.
When the conversation accumulates many images (~5+), the API enforces a **2000px max dimension per image**.
Any image exceeding this — screenshots, user pastes, or file reads — will **crash the session with a non-retryable 400 error**.

**This applies to ALL images, not just browser screenshots.** User-pasted images from Retina Macs are often 2000-3000+ pixels and OpenCode does NOT resize them.

### Mandatory rules

1. **ALWAYS resize after capture.** Every `screenshot` command MUST be followed by a `sips` resize in the same bash call:
   ```bash
   arc-devtools --target <name> screenshot --output /tmp/page.png && sips --resampleHeightWidthMax 1600 /tmp/page.png >/dev/null 2>&1
   ```
   This caps the longest dimension to 1600px (safe margin under the 2000px API limit).

2. **NEVER use `--full-page` in sessions with more than ~5 screenshots.** Full-page captures produce images 3000-10000px tall. If you must capture a full page, resize aggressively:
   ```bash
   arc-devtools --target <name> screenshot --full-page --output /tmp/page.png && sips --resampleHeightWidthMax 1200 /tmp/page.png >/dev/null 2>&1
   ```

3. **Prefer `snapshot` over `screenshot`.** The accessibility tree is text-based — zero image tokens, far cheaper, and never crashes. Use `screenshot` only when visual verification is truly needed (checking layout, colors, visual bugs).

4. **Budget: aim for ≤ 20 screenshots per session.** If the task requires more (e.g., testing many pages), ask the user to split into multiple sessions, or rely on `snapshot` + `evaluate` for most checks and only screenshot key states.

5. **Set viewport before screenshotting.** Smaller viewports = smaller images = safer:
   ```bash
   arc-devtools --target <name> resize 1280 720
   ```

### Quick-reference: safe screenshot one-liner
```bash
# Standard viewport screenshot (ALWAYS use this pattern)
arc-devtools --target <name> screenshot --output /tmp/page.png && sips --resampleHeightWidthMax 1600 /tmp/page.png >/dev/null 2>&1

# Full-page (use sparingly, aggressive resize)
arc-devtools --target <name> screenshot --full-page --output /tmp/page.png && sips --resampleHeightWidthMax 1200 /tmp/page.png >/dev/null 2>&1
```

## Core workflow

Every page-level command prints a `[target:word-pair]` line. Capture it and pass `--target` to all subsequent commands to stay on the same tab.

```bash
# Step 1: navigate, capture target name
arc-devtools navigate https://example.com
# Output includes: [target:red-snake]

# Step 2 onward: pin to that page
arc-devtools --target red-snake snapshot
arc-devtools --target red-snake screenshot --output /tmp/page.png && sips --resampleHeightWidthMax 1600 /tmp/page.png >/dev/null 2>&1
arc-devtools --target red-snake click "#submit"
arc-devtools --target red-snake evaluate "document.title"
```

Without `--target`, commands default to tab index 0, which may not be the right page if Arc reorders tabs.

## Commands

### Navigation
```bash
arc-devtools navigate <url>          # Go to URL, wait for load
arc-devtools navigate --back
arc-devtools navigate --forward
arc-devtools navigate --reload
arc-devtools new-page <url>          # Open new tab
arc-devtools close-page <index>
arc-devtools select-page <index>
arc-devtools list-pages              # List all tabs with friendly names
```

### Inspection
```bash
arc-devtools --target <name> snapshot   # Accessibility tree — PREFERRED for understanding page structure
arc-devtools --target <name> screenshot --output /tmp/page.png && sips --resampleHeightWidthMax 1600 /tmp/page.png >/dev/null 2>&1
arc-devtools --target <name> evaluate "document.title"
```

### Interaction
```bash
arc-devtools --target <name> click "#selector"
arc-devtools --target <name> fill "#selector" "value"
arc-devtools --target <name> type-text "Hello world"
arc-devtools --target <name> press-key Enter
arc-devtools --target <name> press-key Control+A
arc-devtools --target <name> hover ".menu-item"
```

### Utilities
```bash
arc-devtools --target <name> wait-for "Success" --timeout 10000
arc-devtools --target <name> resize 1280 720
arc-devtools --target <name> record-video --output /tmp/demo.mp4 --duration 5   # requires ffmpeg
```

## Global flags

| Flag | Description |
|------|-------------|
| `--target <name>` | Target page by friendly name or raw target ID |
| `--page <index>` | Target page by index (for quick one-offs) |
| `--json` | Machine-readable JSON output |
| `--ws-endpoint <url>` | Explicit WebSocket endpoint — bypasses all Arc management |
| `--user-data-dir <path>` | Chrome/Chromium profile dir — drives that browser via `DevToolsActivePort` instead of Arc |
| `--daemon-idle-timeout <v>` | Daemon idle timeout: `30m`, `1h`, `300s`, or `never` |

Passing `--ws-endpoint` or `--user-data-dir` makes `arc-devtools` skip Arc launch/management entirely.

## A note on Arc Spaces

Arc Spaces are an Arc-UI concept and are not exposed via the DevTools Protocol. All *loaded* tabs across every space appear as page targets (`list-pages` sees them), but Arc **sleeps** inactive tabs — a slept tab has no renderer and won't appear as a target until it's activated in Arc's UI. There is no reliable CDP way to switch spaces; do that in Arc directly, then `list-pages`.

## Daemon behavior

`arc-devtools` runs a background daemon that holds a persistent WebSocket connection to Arc. The first command may quit+relaunch Arc to enable debugging and then starts the daemon; all subsequent commands reuse the connection silently and are fast. The daemon shuts down after 5 minutes idle by default (configurable via `--daemon-idle-timeout` or `ARC_DEVTOOLS_DAEMON_IDLE_TIMEOUT`).

- **Socket**: `$TMPDIR/arc-devtools-daemon.sock` (separate from a `chrome-devtools` daemon — both can run at once)
- **Kill manually**: `pkill -f '__daemon__'` or delete the socket
