# Liaison

**Liaison** is a desktop messenger between **you** and your **AI agents**, like Teams or Telegram
for agents. Every agent session (Claude Code, opencode, your own scripts) gets its own chat box.
Agents send you messages, notifications, questions and permission requests. You reply from the
app, and your message reaches that exact session.

The app never runs agents. Agents connect to it through MCP plus a small hook or plugin.

```
 you ⇄ Liaison app ⇄ liaison-hub (SQLite, unix socket) ⇄ MCP + hooks/plugin ⇄ Claude Code / opencode
```

- **Many sessions at once**, running or not. Each one is a chat box.
- **Messages persist.** When an agent disconnects, its history stays and your new messages are
  queued until it comes back.
- **Reconnect from the app.** An offline session can be resumed in the background
  (`claude --bg --resume`, `opencode run --session`) or in a terminal. Your queued messages are
  handed over once the agent is really running.
- **Ended sessions are read-only.** When the context is gone (transcript deleted, opencode
  session deleted), the chat says so and stops accepting messages.
- **Listen mode.** When its turn ends the agent stays parked waiting for you, so your messages
  reach it instantly with no terminal involved.
- **Questions and permissions.** Agents can ask you something with answer buttons. Permission
  prompts (Allow/Deny) go to the app when nobody is at the agent's terminal, and the notification
  itself has the buttons.
- **Tray icon.** Liaison sits in your status bar (waybar's `tray` module, KDE, …) with an orange
  dot while messages are unread. Click it to open the app; its menu lists the chats waiting for you.
- **Zoom and text size** in *Settings* (or Ctrl + / Ctrl − / Ctrl 0).
- Fully local: one unix socket, no network port. Native UI ([iced](https://iced.rs)), no web view.

## Install (Arch Linux)

```sh
cd packaging
makepkg -si                                   # builds and installs the `liaison` package
systemctl --user enable --now liaison-hub     # start the hub now and at every login
liaison connect                               # hook up Claude Code and/or opencode
```

Open **Liaison** from your launcher. Sessions started after `liaison connect` show up
automatically; restart sessions that were already running.

> Run `liaison connect` again after installing the package if you had connected a development
> build. The hooks store the path of the `liaison` binary that installed them.

## How messages reach an agent

| Session state | Your message is delivered… |
|---|---|
| **working** (in a turn) | after its next tool call (Claude `PostToolUse` hook, opencode `tool.execute.after`) |
| **idle** – Claude Code | when its next turn starts (`UserPromptSubmit` hook); turn on **Listen** for instant delivery |
| **idle** – opencode | immediately (the plugin starts a turn with your message) |
| **listening** | immediately |
| **offline** | kept; delivered when it reconnects (use **Reconnect** in the app) |
| **ended** | not accepted. The context is gone, so the chat is read-only. |

When a turn was started by your message, the agent's final answer is mirrored into the chat
automatically. Turn on **All replies** to mirror every turn.

## The agent's tools (MCP server `liaison`)

| Tool | Purpose |
|---|---|
| `chat_send(text)` | message you |
| `chat_notify(title, body, level)` | highlighted message and desktop notification |
| `chat_ask(question, options?, timeout_secs?)` | ask and wait for your answer |
| `chat_inbox()` | fetch your unread messages |
| `chat_wait(timeout_secs?)` | wait for your next message |
| `chat_status(text)` | status line in the chat header |
| `chat_set_title(title)` | rename the chat |

## Command line

```sh
liaison status                     # hub and connectors
liaison sessions [--all]           # list chats
liaison messages <session> [-n 20]
liaison send <session> <text…>     # write to a session as the user
liaison reconnect <session> [--terminal]
liaison open [session]             # open or focus the app
liaison connect [claude-code|opencode] / liaison disconnect <…>
```

`<session>` can be an id, an id prefix, or part of the title.

### Your own scripts

```sh
id=$(liaison agent register backup-job)      # a chat that goes offline when the script exits
liaison agent say "$id" "Backup started"
liaison agent notify "$id" "Backup done" "42 GB in 3m10s" --level success
answer=$(liaison agent ask "$id" "Delete old snapshots?" --option Yes --option No)
liaison agent wait "$id" --timeout 600       # wait for your next message
```

## Configuration

`~/.config/liaison/config.toml` (every key is optional):

```toml
[notifications]
enabled = true
messages = true          # also notify for plain chat messages

[timeouts]
permission_secs = 120    # then the agent falls back to its terminal prompt
question_secs = 900      # default wait for chat_ask
listen_secs = 3000       # how long a listening agent stays parked

[permissions]
route = "auto"           # auto = when nobody is at the terminal | always | never

[commands]
# recorded by `liaison connect`; used to restart sessions from the app
# claude = "/home/you/.local/bin/claude"
# opencode = "/home/you/.local/bin/opencode"
# terminal = ["kitty", "--directory", "{cwd}", "--"]

[desktop]
theme = "system"         # system | dark | light
zoom = 1.0               # whole window, 0.8 – 2.0 (Ctrl + / Ctrl − / Ctrl 0)
text_size = 16           # chat text in px, 12 – 24; other text scales with it
tray = true              # status-bar icon (read by the hub: restart it after changing)
# font = "Noto Sans"     # a static (non-variable) font
```

Files: database `~/.local/share/liaison/liaison.db`, socket `$XDG_RUNTIME_DIR/liaison/hub.sock`,
logs `journalctl --user -u liaison-hub`.

**Hyprland, sway and other window managers:** to let notification clicks open the app, the
systemd user session needs your display variables. Most setups already have them; if not, add
`exec-once = dbus-update-activation-environment --systemd --all` to your Hyprland config.

## Development

```sh
cargo build                 # liaison, liaison-hub, liaison-desktop in target/debug
cargo test --workspace
./target/debug/liaison-hub  # run the hub in the foreground (RUST_LOG=liaison_hub=debug)
```

Use `LIAISON_RUNTIME_DIR`, `LIAISON_DATA_DIR` and `LIAISON_CONFIG_DIR` to run a second,
isolated hub next to the installed one.

| Path | What |
|---|---|
| `crates/liaison-core` | shared types, hub API client, config, reconnect launcher |
| `crates/liaison-hub` | the daemon: SQLite, HTTP API over a unix socket, liveness monitor, notifications |
| `crates/liaison-cli` | `liaison`: MCP server, Claude Code hooks, connectors, CLI |
| `crates/liaison-desktop` | `liaison-desktop`: the iced app |
| `connectors/opencode/liaison.js` | opencode plugin (embedded in the CLI, installed by `liaison connect`) |
| `packaging/` | PKGBUILD, systemd user unit, desktop entry, icon |

See [PLAN.md](PLAN.md) for the design.
