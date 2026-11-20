# ale-term

A fast, private terminal for macOS built for working with coding agents.
No telemetry and no network access: the app never opens a connection.

Status: milestone 3. A daily-usable terminal with tabs, splits, workspaces
per project, search, clickable links, shell integration, ligatures, inline
images and a command palette. Agent features come in later milestones.

## Build and run

    ./scripts/bundle.sh                 # builds target/release/ale-term.app
    open target/release/ale-term.app

During development: `cargo run -p ale-term`, and `cargo test --workspace`.

## Shortcuts

Every command is also in the command palette (Shift+Cmd+P).

| Keys | Action |
|---|---|
| Shift+Cmd+P | Command palette |
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

## Layout

| Crate | Role |
|---|---|
| `crates/core` | Shell sessions: PTY plus terminal emulation (`alacritty_terminal`) |
| `crates/render` | GPU drawing of rectangles and glyphs (`wgpu`, `cosmic-text`) |
| `crates/config` | Config file and themes |
| `crates/app` | Window, tabs, splits, keyboard and mouse |

## License

MIT or Apache-2.0, at your option.
