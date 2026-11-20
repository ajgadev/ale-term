# ale-term

A fast, private terminal for macOS built for working with coding agents.
No telemetry and no network access: the app never opens a connection.

Status: milestone 1 (a working terminal with tabs and splits). Agent
features come in later milestones.

## Build and run

    ./scripts/bundle.sh                 # builds target/release/ale-term.app
    open target/release/ale-term.app

During development: `cargo run -p ale-term`, and `cargo test --workspace`.

## Shortcuts

| Keys | Action |
|---|---|
| Cmd+T / Cmd+W | New tab / close pane (closes the tab with its last pane) |
| Cmd+D / Cmd+Shift+D | Split side by side / stacked |
| Cmd+Option+Arrows | Move focus between panes |
| Cmd+] / Cmd+[ | Next / previous pane |
| Cmd+1..9, Ctrl+Tab, Cmd+Shift+] / [ | Switch tabs |
| Cmd+C / Cmd+V | Copy selection / paste |
| Cmd+K | Clear scrollback |
| Cmd+= / Cmd+- / Cmd+0 | Font size |
| Shift+PageUp / PageDown | Scroll history |
| Cmd+, | Open the config file |
| Ctrl+Cmd+F | Full screen |

Mouse: drag to select (double-click a word, triple-click a line), drag the
line between panes to resize, middle-click a tab to close it. Hold Shift to
select text in programs that capture the mouse.

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
