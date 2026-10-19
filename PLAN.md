# Liaison — plan

> **liaison** /liˈeɪzɒn/ — a link that keeps two sides talking.
> Liaison is a local desktop messenger between **you** and your **AI agents**
> (Claude Code, opencode, …). The app never runs agents: agents connect to it.

## 1. Goals

- A Teams/Telegram-like desktop app: one **chat box per agent session**, many at once,
  running or not.
- Agents **talk to you** (messages, notifications, questions, permission requests).
- You **talk to agents**; each message is routed to the exact harness + session.
- **Messages persist** forever (until you delete them), even when a connection drops.
- A session whose connection dropped can be **reconnected from the app**.
- A session whose context is gone becomes **read-only**.
- Local only (Arch Linux desktop), single user, installable as a pacman package.

## 2. Decisions

| Topic | Decision |
|---|---|
| Language | Rust (workspace) — the opencode connector plugin is a small JS file (opencode plugins must be JS/TS) |
| UI | `iced` 0.14 (native, no web layer) |
| Database | SQLite (`rusqlite`, bundled, WAL) at `~/.local/share/liaison/liaison.db` |
| Transport | HTTP/1.1 over a **Unix socket** at `$XDG_RUNTIME_DIR/liaison/hub.sock` (no TCP port; file permissions = auth) |
| Agent protocol | **MCP** (stdio, `rmcp`) for agent → you, plus Claude Code **hooks** / opencode **plugin** for delivery |
| Notifications | `notify-rust` (freedesktop), sent by the hub so they work with the window closed |
| Tray icon | `ksni` (StatusNotifierItem) in the hub, icon rendered from the SVG with `resvg`; orange dot = unread |
| Install | `PKGBUILD` → `/usr/bin`, systemd **user** service, `.desktop` entry |
| Config | `~/.config/liaison/config.toml` |

## 3. Architecture

```
┌────────────────────┐                         ┌────────────────────────────────┐
│ liaison-desktop    │◄── HTTP + NDJSON events ─►│ liaison-hub  (systemd --user) │
│ (iced window)      │      unix socket        │ axum · SQLite · notifier ·     │
└────────────────────┘                         │ liveness monitor · launcher    │
                                               └───────────────▲────────────────┘
                                                               │ unix socket
                     ┌─────────────────────────────────────────┴───────────────┐
                     │ liaison (CLI)                                           │
                     │   liaison mcp            → MCP server (stdio)           │
                     │   liaison hook <event>   → Claude Code hooks            │
                     │   liaison connect …      → installs connectors          │
                     │   liaison send/sessions/status → terminal helpers       │
                     └──────────────▲──────────────────────────▲───────────────┘
                          spawned by│                          │ HTTP over unix socket
                       Claude Code (MCP + hooks)     opencode (MCP + liaison.js plugin)
```

### Crates

```
crates/
  liaison-core     shared types, API DTOs, unix-socket HTTP client, paths, /proc helpers
  liaison-hub      daemon (binary: liaison-hub)
  liaison-cli      CLI, MCP server, hooks, connectors (binary: liaison)
  liaison-desktop  iced app (binary: liaison-desktop)
connectors/opencode/liaison.js   opencode plugin (embedded into the CLI, installed by `liaison connect opencode`)
packaging/       PKGBUILD, liaison-hub.service, liaison.desktop, icon
```

## 4. Data model

**sessions**
`id` (ULID) · `harness` (`claude-code` | `opencode` | `custom`) · `external_id` (the harness' own
session id, UNIQUE with harness) · `title` · `cwd` · `transcript_path` · `pid` + `pid_start`
(the harness process, start time guards against PID reuse) · `status` · `status_text` ·
`listen` · `mirror` · `archived` · `awaiting_reply` · `ended_reason` · timestamps.

**messages**
`id` (global sequence) · `session_id` · `direction` (`to_agent` | `from_agent` | `system`) ·
`kind` (`text` | `reply` | `notify` | `question` | `permission` | `event`) · `body` · `meta` (JSON) ·
`delivery` (`queued` → `delivered` → `read`, or `undeliverable`) · timestamps.

**questions** (questions & permission requests that wait for your answer)
`id` · `session_id` · `message_id` · `kind` · `prompt` · `options` · `answer` · `status`
(`pending` | `answered` | `expired`).

## 5. Session lifecycle

```
             ┌────────── turn starts ──────────┐
             ▼                                 │
  ┌──────► active ── turn ends ──► idle ───────┤
  │          ▲                      │ listen on│
  │          └── message arrives ── listening ◄┘
  │
  │  process exits (liveness monitor / SessionEnd)
  │          ▼
  └── reconnect ◄── disconnected ── context gone ──► ended (read-only)
```

| Status | Meaning | Your messages |
|---|---|---|
| `active` | agent is working on a turn | delivered **mid-turn** after the next tool call |
| `idle` | process alive, waiting for terminal input | queued; delivered on the next turn (Claude) or **immediately** (opencode) |
| `listening` | agent is parked waiting for you (listen mode / `chat_wait`) | delivered **immediately** |
| `disconnected` | process gone, context still on disk | **kept queued**; *Reconnect* starts it again and delivers them |
| `ended` | context gone (transcript deleted / opencode session deleted) | input disabled, history read-only, queued → `undeliverable` |

- The hub checks every harness PID (`/proc/<pid>` + start time) every few seconds, so a crash or
  closed terminal is noticed even without a `SessionEnd` event.
- Claude `/compact` keeps the session; `/clear` disconnects the old one and creates a new one.

### Reconnect (from the app)

| Harness | **Background** (no terminal) | **Terminal** |
|---|---|---|
| Claude Code | `claude --bg --resume <id> "<prompt>"`: same session id. Listen mode is turned on so it stays reachable | `<terminal> claude --resume <id> "<prompt>"` in the session's cwd |
| opencode | `opencode run --session <id> --dir <cwd> "<prompt>"` (one turn, then exits) | `<terminal> opencode <cwd> --session <id> --prompt "<prompt>"` |

- The prompt only says "you were reconnected, the user's messages are attached". The queued
  messages themselves are taken by the agent's own hook (`UserPromptSubmit`, or opencode's
  `chat.message`) once it is really running, so a crash during start-up cannot lose them.
- If the session has not registered again 45 s after a reconnect, the chat says so and the
  messages stay queued.
- The app or CLI starts the process (not the hub), because they run with your desktop
  environment (PATH, display). `liaison connect` records the agent's absolute path in config.toml.
- Claude Code only resumes in the background inside folders it already trusts.
- The terminal is auto-detected (`$TERMINAL`, kitty, alacritty, foot, wezterm, …) or set in config.

## 6. Connectors

### Claude Code — `liaison connect claude-code`
Adds the MCP server (`liaison mcp`, user scope) and these hooks to `~/.claude/settings.json`:

| Hook | Action |
|---|---|
| `SessionStart` | register/upsert session (pid, cwd, transcript); inject a short "you are connected" note + any queued messages |
| `UserPromptSubmit` | status → active; queued messages added as context |
| `PostToolUse` | queued messages added as context (**mid-turn delivery**) |
| `Stop` | if messages queued → block the stop and hand them over; if **listen** is on → wait for you; mirror the final reply when the turn was triggered from the app |
| `PermissionRequest` | Allow/Deny card in the app + notification; no answer within the timeout → normal terminal prompt |
| `Notification` | forwarded to the app |
| `SessionEnd` | status → disconnected |

The MCP server finds its session by walking up its process tree to the harness PID.

### opencode — `liaison connect opencode`
Adds the MCP server to `~/.config/opencode/opencode.jsonc` and installs
`~/.config/opencode/plugins/liaison.js`, which:
- registers sessions and status (`session.created/updated/status/idle/deleted`),
- **pushes** your messages into idle sessions (`client.session.prompt`, via a long-poll on
  `/v1/pids/{pid}/pending`), appends them to tool output during busy turns (`tool.execute.after`),
  and attaches them to prompts typed in opencode or sent by a reconnect (`chat.message`),
- routes permission requests to the app (`permission.ask`),
- injects the session id into `liaison_*` MCP tool calls (`tool.execute.before`).

### MCP tools (both harnesses)

| Tool | Purpose |
|---|---|
| `chat_send(text)` | message to the user |
| `chat_notify(title, body, level)` | highlighted message + desktop notification |
| `chat_ask(question, options?, timeout_secs?)` | waits for the user's answer (buttons or text) |
| `chat_inbox()` | fetch unread messages from the user |
| `chat_wait(timeout_secs?)` | park until the user writes (listen mode on demand) |
| `chat_status(text)` | short status line shown in the chat header |
| `chat_set_title(title)` | rename the chat box |

## 7. Hub API (HTTP over the unix socket, JSON)

Agent side: `POST /v1/sessions/register` · `POST /v1/sessions/{id}/state` · `POST /v1/sessions/{id}/end` ·
`GET /v1/resolve?pid=|harness=&external_id=` · `POST /v1/sessions/{id}/messages` ·
`POST /v1/sessions/{id}/take` · `GET /v1/sessions/{id}/wait?timeout=` ·
`POST /v1/sessions/{id}/questions` · `GET /v1/questions/{id}/wait?timeout=` ·
`GET /v1/pids/{pid}/pending?timeout=`

UI side: `GET /v1/sessions` · `GET /v1/sessions/{id}/messages` · `POST /v1/sessions/{id}/send` ·
`POST /v1/sessions/{id}/read` · `PATCH /v1/sessions/{id}` · `POST /v1/sessions/{id}/reconnect` ·
`DELETE /v1/sessions/{id}` · `POST /v1/questions/{id}/answer` · `POST /v1/ui/presence` ·
`GET /v1/events` (NDJSON stream) · `GET /v1/health`

## 8. Milestones

All done for v0.1.0:

1. ✅ **Workspace + core + hub**: DB, API, liveness monitor, notifications, events. Unit tests cover the routing rules.
2. ✅ **CLI**: `send`, `sessions`, `messages`, `status`, `reconnect`, `open`, `agent *`, `hook *`, `mcp`.
3. ✅ **Claude Code connector**: verified with real sessions (MCP routing, hooks, background
   reconnect with a queued message, listen-mode delivery, disconnect detection).
4. ✅ **Desktop app**: session list grouped by state, chat with markdown, questions/permissions,
   reconnect, listen/mirror toggles, rename, archive, delete, light/dark theme.
5. ✅ **opencode connector**: verified with real sessions (registration, MCP routing, reconnect,
   live push into an idle session).
6. ✅ **Packaging**: PKGBUILD (`makepkg -si`), systemd user unit, desktop entry, icon.

v0.2.0:

7. ✅ **Tray icon** in the hub (always there, even with the window closed): orange dot while
   anything is unread, tooltip with counts, click opens the app (on the first unread chat), menu
   lists unread chats.
8. ✅ **Settings panel**: theme, zoom (80–200 %, Ctrl + / − / 0) and text size (12–24 px, default
   raised from 14 to 16), saved in `[desktop]` of config.toml.

## 9. Known limits

- iced cannot select weights from variable fonts (Inter, Adwaita Sans), so the app uses a static
  font (Noto Sans / DejaVu Sans / Liberation Sans, or `[desktop] font`).
- Message text cannot be selected with the mouse yet (iced limitation); each message has a Copy button.
- If Claude Code forks a resumed session into a copy (new session id), the copy shows up as a new chat.
- opencode's `permission.ask` plugin hook may not fire in every opencode version; the
  terminal prompt still works.

## 10. Later

- Waking a Claude Code session that is idle at its terminal prompt after listen mode timed out
  (Claude Code offers no way to inject a prompt from outside; for now the user wakes it from the
  terminal).
- "Continue in a new session" hand-off for ended sessions (summary + undelivered messages).
- Full-text search (SQLite FTS5), attachments, more harnesses (Codex, Aider, plain scripts).
