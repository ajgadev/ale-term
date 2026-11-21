# ale-term

A fast, private terminal for macOS built for working with coding agents.
No telemetry and no network access: the app never opens a connection.

Status: milestone 4. A daily-usable terminal with tabs, splits, workspaces
per project, search, clickable links, shell integration, ligatures, inline
images, a command palette, and a sidebar showing what each coding agent is
doing.

## Build and run

    ./scripts/bundle.sh                 # builds target/release/ale-term.app
    open target/release/ale-term.app

During development: `cargo run -p ale-term`, and `cargo test --workspace`.

## Shortcuts

Every command is also in the command palette (Shift+Cmd+P).

| Keys | Action |
|---|---|
| Shift+Cmd+P | Command palette |
| Cmd+B | Show or hide the sidebar |
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
| Cmd+, | Open the config file |
| Ctrl+Cmd+F | Full screen |

Mouse: drag to select (double-click a word, triple-click a line), drag the
line between panes to resize, middle-click a tab to close it. Hold Shift to
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

Tabs holding an agent get a dot in the state's color. Click a row to jump to
that pane. When an agent needs you or finishes while you are elsewhere,
ale-term plays a sound and, if the window is not in front, shows a macOS
notification (both can be turned off under `[agents]`).

How the status is known:

- **Claude Code** reports it through hooks. ale-term offers to install them
  the first time it sees Claude Code running (a hint at the bottom of the
  sidebar, or "Set Up Claude Code Status Hooks" in the palette). They are
  added to `~/.claude/settings.json`, keep your other settings, leave a
  backup next to the file, and do nothing outside ale-term. "Remove Claude
  Code Status Hooks" takes them out again.
- **Codex, Gemini CLI, Kiro and OpenCode** are recognized by their command
  and followed by watching the pane: output means working, silence means
  done, and a bell or notification request means the agent wants you. This
  cannot tell "finished" from "waiting for an answer" as reliably as hooks.

Status travels over a Unix socket on this Mac; nothing uses the network.

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

## Configuration

`~/.config/ale-term/config.toml`, created on first run with every option
documented. Changes apply when the file is saved.

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
