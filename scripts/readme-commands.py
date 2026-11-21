#!/usr/bin/env python3
"""Regenerates the command table in README.md from the command palette's
list (`COMMANDS` in crates/app/src/actions.rs).

The table sits between `<!-- commands:start -->` and `<!-- commands:end -->`
in the README; everything else in the file is left alone. A test in
actions.rs fails when a command is missing from the README, and says to run
this script.

    python3 scripts/readme-commands.py
"""

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ACTIONS = ROOT / "crates" / "app" / "src" / "actions.rs"
README = ROOT / "README.md"
START, END = "<!-- commands:start -->", "<!-- commands:end -->"

# Which group each command is listed under, by its `Action` name. A command
# not listed here goes under "Other", so none is ever left out.
GROUPS = [
    ("Tabs and panes", [
        "NewTab", "ClosePane", "CloseTab", "RenameTab", "SplitRight", "SplitDown",
        "NextTab", "PreviousTab", "NextPane", "PreviousPane", "FocusPane",
    ]),
    ("Workspaces", [
        "SwitchWorkspace", "NextWorkspace", "PreviousWorkspace", "NewWorkspace",
        "RenameWorkspace", "MoveTab", "SetUpWorkspace",
    ]),
    ("Agents", [
        "NewAgentSession", "NextAttention", "ToggleSidebar", "SwitchLayout", "ToggleBoard",
        "ToggleGraph", "ShowArchive", "ClearArchive", "SetUpHooks", "RemoveHooks",
        "SetUpKiroHooks", "RemoveKiroHooks",
    ]),
    ("Text, search and scrolling", [
        "Copy", "Paste", "Find", "FindNext", "FindPrevious", "PreviousPrompt", "NextPrompt",
        "ScrollPageUp", "ScrollPageDown", "ScrollToTop", "ScrollToBottom", "ClearHistory",
    ]),
    ("Window and app", [
        "ZoomIn", "ZoomOut", "ZoomReset", "ToggleFullscreen", "CommandPalette", "Settings",
        "OpenConfig", "Quit",
    ]),
]


def commands():
    """(action, name, shortcut) for every palette command, in order."""
    source = ACTIONS.read_text()
    body = source.split("pub const COMMANDS", 1)[1].split("];", 1)[0]
    pattern = re.compile(r'\(Action::(\w+)(?:\([^)]*\))?,\s*"([^"]+)",\s*"([^"]*)"\)')
    return pattern.findall(body)


def table():
    found = commands()
    if not found:
        sys.exit("no commands found in actions.rs")
    grouped = {title: [] for title, _ in GROUPS}
    grouped["Other"] = []
    for action, name, keys in found:
        title = next((t for t, actions in GROUPS if action in actions), "Other")
        grouped[title].append((name, keys))
    lines = [START, "", "| Command | Shortcut |", "|---|---|"]
    for title, rows in grouped.items():
        if not rows:
            continue
        lines.append(f"| **{title}** | |")
        for name, keys in rows:
            shown = f"`{keys}`" if keys else ""
            lines.append(f"| {name} | {shown} |")
    lines += ["", END]
    return "\n".join(lines)


def main():
    text = README.read_text()
    if START not in text or END not in text:
        sys.exit(f"README.md has no {START} … {END} section")
    before, rest = text.split(START, 1)
    _, after = rest.split(END, 1)
    README.write_text(before + table() + after)
    print(f"Wrote {len(commands())} commands to {README.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
