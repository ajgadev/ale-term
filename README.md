# ale-term

A fast, private terminal for macOS built for working with coding agents.
No telemetry and no network access: the app never opens a connection.

A terminal with tabs, splits and a workspace per project, plus a sidebar,
board and graph showing what each coding agent is doing. About 45 MB of
memory with a tab open, and a first frame in about 120 ms.

## Build and run

Needs macOS and a Rust toolchain (`brew install rustup && rustup default
stable`).

    ./scripts/bundle.sh                 # builds target/release/ale-term.app
    open target/release/ale-term.app

To keep it, drag `target/release/ale-term.app` into Applications. The app
is signed for this Mac only; to give it to someone else, have them build it
or sign it with a Developer ID.

During development: `cargo run -p ale-term`, and `cargo test --workspace`.
`scripts/make-icon.py` regenerates the app icon.

## Shortcuts

Every command is also in the command palette (Shift+Cmd+P).

| Keys | Action |
|---|---|
| Shift+Cmd+P | Command palette |
| Cmd+B | Show or hide the sidebar |
| Shift+Cmd+B | Show or hide the agent board |
| Option+Cmd+G | Show or hide the agent graph beside the focused agent |
| Shift+Cmd+T | New session: a page to give an agent a task, or open a plain terminal |
| Shift+Cmd+A | Go to the next agent that needs you |
| Cmd+P | Switch workspace (also: click the workspace name) |
| Ctrl+Cmd+Right / Left | Next / previous workspace |
| Cmd+T / Cmd+W | New tab / close pane (closes the tab with its last pane) |
| Shift+Cmd+W | Close tab |
| Cmd+D / Shift+Cmd+D | Split right / down |
| Option+Cmd+Arrows | Move focus between panes |
| Cmd+] / Cmd+[ | Next / previous pane |
| Cmd+1..9, Ctrl+Tab, Shift+Cmd+] / [ | Switch tabs |
| Cmd+C / Cmd+V | Copy selection / paste |
| Cmd+F | Find in scrollback (Enter: older match, Shift+Enter: newer) |
| Cmd+G / Shift+Cmd+G | Find next / previous |
| Cmd+Up / Cmd+Down | Jump to previous / next prompt |
| Cmd+Home / Cmd+End | Scroll to top / bottom |
| Shift+PageUp / PageDown | Scroll a page |
| Cmd+K | Clear scrollback |
| Cmd+= / Cmd+- / Cmd+0 | Font size |
| Cmd+, | Settings |
| Ctrl+Cmd+F | Full screen |

Mouse: drag to select (double-click a word, triple-click a line), drag the
line between panes to resize, click the × on a tab (or middle-click it) to
close it. Hold Shift to
select text in programs that capture the mouse. Hold Cmd and click a web
address or file path to open it.

## Agents

The sidebar lists every agent and shell, grouped by project (or by status:
the button next to the search box). Each agent shows its task, what it is
doing right now, and how long it has been in its current state:

| State | Meaning |
|---|---|
| Working | Busy with your request |
| Needs you | Blocked on a permission prompt or a question |
| Done | Finished its turn and you have not looked yet |
| Idle | Waiting for a prompt |
| Error / Rate limited | The turn ended with a failure or a usage limit |
| Waiting on subagents | The main agent stopped; subagents are still running |

Tabs and sidebar rows for an agent show a small mark for that agent with a
dot in the state's color. (The marks are simple shapes in each agent's
color, not the vendors' logos.) Click a row to jump to that pane.

When an agent is stuck waiting for you (a permission prompt, a question, an
error) while you are elsewhere, ale-term plays a sound and, if the window is
not in front, shows a macOS notification. Both can be turned off under
`[agents]`. A finished turn is never announced; it shows as Done on the tab
and in the sidebar.

**New session** (Shift+Cmd+T, or the button at the top of the sidebar) opens
a page asking what to build. Type a task and press Enter to start the
selected agent on it; Tab switches agent; Cmd+Enter, or "or open a
terminal", gives a plain shell instead. Nothing runs until you choose.
Closing the last tab brings this page back instead of quitting; Cmd+Q quits.

How the status is known:

- **Claude Code** reports it through hooks. ale-term offers to install them
  the first time it sees Claude Code running (a hint at the bottom of the
  sidebar, or "Set Up Claude Code Status Hooks" in the palette). They are
  added to `~/.claude/settings.json`, keep your other settings, leave a
  backup next to the file, and do nothing outside ale-term. "Remove Claude
  Code Status Hooks" takes them out again.
- **Without hooks** (Codex, Gemini CLI, Kiro, OpenCode, and Claude Code
  until its hooks are installed) the agent is recognized by its command and
  followed by watching the pane. A steady stream of updates lasting over a
  second (a spinner) means working, silence afterwards means done, and a
  bell or notification request means the agent wants you. This is a guess:
  it cannot tell "finished" from "waiting for an answer", and it shows
  Idle for an agent that works without animating anything.

Status travels over a Unix socket on this Mac; nothing uses the network.

**Board** (Shift+Cmd+B): every agent as a card in four columns, Needs you,
Working, Done and Idle. Click a card, or move with the arrow keys and press
Enter, to jump to that agent.

**Graph** (Option+Cmd+G, or click an agent in the sidebar that has
subagents): a panel beside the agent's terminal showing the agent, the
subagents it launched, the ones those launched in turn, and its background
tasks, as a tree. Each node shows what it was asked to do and what it is
doing now, or its final answer once finished. Click a node to read its
recent output below the tree: what it was asked, what it said, and the tools
it called. This works for Claude Code with status hooks installed; the
output is read from Claude Code's own conversation files under `~/.claude`.

Claude Code offers no way for another program to send a message to a
subagent or stop one, so the graph is for watching. When a subagent's
request needs your approval the prompt appears in the agent's terminal,
which is right beside the panel.

**Archive**: when an agent's session ends it is filed under "Archived" at
the bottom of the sidebar. Picking one opens a tab in the folder it ran in;
Claude Code sessions are resumed with `claude --resume <id>`, and for other
agents the start command is typed for you to run. Only the task name, agent,
folder and session id are kept, never anything that was printed. "Clear
Session Archive" in the palette empties it.

**After a restart**, a pane that had Claude Code running gets `claude
--resume <id>` typed at its prompt; press Enter to continue that
conversation.

Codex, Gemini CLI and Kiro have hook systems of their own, and `ale-term
hook <agent>` already understands their events, but installing into their
settings is not automated because it could not be tested here. To wire one
up by hand, register this as the command for each of the agent's hook
events (replace `codex` with `gemini` or `kiro`):

    "${ALE_TERM_BIN:-true}" hook codex

## Workspaces

Tabs are grouped by project: a tab belongs to the workspace of the git
repository its folder is in, and the tab bar shows one workspace at a time.
`cd` into another project and the tab moves to that project's workspace (and
the view follows). Tabs outside any repository live in `general`.

From the command palette you can also rename tabs and workspaces, move a tab
to another workspace (it then stays there), and create a workspace by hand.

**Templates** open a project's usual tabs in one go. Define them in the
config file:

    [[workspace]]
    root = "~/Dev/my-project"
    tabs = [
      { title = "agent", command = "claude" },
      { title = "server", command = "npm run dev" },
      { title = "git" },
    ]

and pick the project in the switcher (Cmd+P). A repository can instead carry
its own `.ale-term.toml` with the same `tabs = [...]` list; run "Set Up
Workspace Tabs from Template" to use it. ale-term shows the commands and asks
before running them, and asks again whenever that file changes.

**Session restore**: workspaces, tabs, splits and folders are reopened at
launch. A command that was running is typed at the prompt but not run; press
Enter to start it again. Only folders and command lines are saved, never
terminal contents. Turn it off with `restore = false` under `[window]`.

## Shell integration

For zsh, ale-term loads a small script (without touching your dotfiles) so
the shell reports its folder, where prompts are, and which command is
running. This gives accurate tab titles, new tabs in the right folder and
prompt jumping. Turn it off with `shell_integration = false`.

## Inline images

Both the iTerm2 protocol (`imgcat`) and the kitty graphics protocol
(`kitten icat`, `timg`, `chafa`, `yazi`) are supported for PNG, JPEG and GIF
(first frame). kitty's file and shared-memory transfer modes, animation and
Unicode placeholders are not. Sixel is not supported.

## Settings

Cmd+, opens the settings screen: arrow keys to move and change values,
Enter to toggle or to edit text. Changes apply immediately.

The screen is a view onto one file, `~/.config/ale-term/config.toml`, which
is created on first run with every option documented. Editing the file
directly works just as well and is how colors and workspace templates are
set; changes apply when it is saved. Changes made from the settings screen
rewrite only the affected line, leaving your comments and layout alone. If
the file has an error, ale-term says so and keeps the previous settings.

## Privacy

ale-term makes no network connections: there is no telemetry, no update
check and no crash reporting, and no networking library is linked in.
Agents report their status over a Unix socket on this Mac.

What it stores, all under `~/.config/ale-term/`:

| File | Contents |
|---|---|
| `config.toml` | Your settings |
| `state/session.json` | Open workspaces, tabs, splits, folders and running command lines, for restoring |
| `state/archive.json` | Ended agent sessions: task name, agent, folder, session id |
| `state/approved.json` | Per-project template files you allowed to run |
| `shell-integration/` | The zsh script it loads |

Nothing a terminal or an agent printed is ever written to disk. It reads
Claude Code's own conversation files under `~/.claude` to show a subagent's
output, and writes to `~/.claude/settings.json` only if you ask it to
install the status hooks.

## Rendering and memory

By default the window is painted on the CPU into memory shared with the
macOS compositor, repainting only the parts that changed. This keeps memory
low (about 45 MB with a tab open, whether idle or busy).

Set `renderer = "gpu"` under `[window]` to draw with the graphics card
instead. That is faster for very large windows under heavy output, but on
Apple GPUs the graphics driver holds about 90 MB of working memory while the
screen is changing (about 150 MB in total).

## Layout

| Crate | Role |
|---|---|
| `crates/core` | Shell sessions: PTY plus terminal emulation (`alacritty_terminal`) |
| `crates/render` | Drawing rectangles, glyphs and images: a CPU renderer (default) and a GPU one (`wgpu`); text via `cosmic-text` |
| `crates/config` | Config file and themes |
| `crates/agents` | Agent detection, status reports and session state |
| `crates/app` | Window, tabs, splits, keyboard and mouse |

## License

MIT or Apache-2.0, at your option.
