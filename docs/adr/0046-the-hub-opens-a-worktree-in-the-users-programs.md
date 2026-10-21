# ADR-0046: The hub opens a stack's worktree in the user's own programs, from a fixed list

- **Status:** Accepted (2026-10-21), amended 2026-10-21 (the program opens in front on Windows)
- **Date:** 2026-10-21
- **Deciders:** project owner
- **Related:** [ADR-0011](0011-platform-windows-native-first.md), [ADR-0019](0019-control-api-security.md), [ADR-0021](0021-a-hub-serves-the-dashboard.md), [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md), [ADR-0040](0040-a-shared-lane-can-be-watched-like-ones-own.md)

## Context

The stack header shows where the shown tree lives (`D:\src\app`, or a lane's
worktree). The next step from there is nearly always another program: the
folder in the file manager, a terminal in it, an editor, an agent. A web
page can do none of that: it can copy the path, and it can open a link
whose scheme a program registered (`vscode://file/…`), but it cannot tell
whether that program is there. A link nobody handles does nothing, and the
browser asks before every external scheme until told not to.

The hub is a local program of the user (ADR-0021) and already does for the
page what the page cannot: the folder dialog (`POST /hub/pick-folder`).

Looked at on Windows 11 (2026-10-21): the editors and agents register their
scheme under `HKCR\<scheme>` with a `URL Protocol` value, packaged ones too
(Claude Desktop: `claude`); VS Code `vscode`, the Claude Code CLI
`claude-cli` (`HKCU`, written at its first prompt). Orca registers no scheme;
its CLI (`orca`) brings its window to a worktree (`orca file open … --focus`).
Windows Terminal is the `wt.exe` app alias on `PATH`.

## Decision

1. **The hub opens, the page asks.** `GET /hub/openers` lists what this
   machine can open a folder with; `POST /hub/open {stack, path, with}`
   opens it. Copying the path stays in the page.
2. **A fixed list, found, not configured.** The hub knows each program and
   how to start it; it lists the ones it finds:

   | id | Program | Found by | Started as |
   |---|---|---|---|
   | `files` | Explorer / the file manager | always (Windows); `xdg-open` on `PATH` (Linux) | the folder, opened by the shell / `xdg-open` |
   | `wt` | Windows Terminal | `wt.exe` on `PATH` | `wt -d <path>` |
   | `powershell` | PowerShell | always (Windows); `pwsh` before `powershell` | a new console in the folder |
   | `terminal` | Terminal (Linux) | `$TERMINAL`, `x-terminal-emulator`, then common ones | the terminal in the folder |
   | `vscode`, `vscode-insiders`, `cursor`, `windsurf`, `zed` | the editor | its scheme is registered | `<scheme>://file/<path>` |
   | `claude` | Claude (desktop, Code) | `claude` is registered | `claude://code/new?folder=<path>` |
   | `claude-cli` | Claude Code (terminal) | `claude-cli` is registered | `claude-cli://open?cwd=<path>` |
   | `orca` | Orca | `orca` on `PATH` or in its install folder | `orca file open <stack file> --worktree path:<path> --focus` (the repository added to Orca first, if Orca does not know it) |

   A scheme is "registered" when Windows has `HKCR\<scheme>` with `URL
   Protocol`, or when `xdg-mime` names a handler for
   `x-scheme-handler/<scheme>` (Linux). A link is opened the way the shell
   opens it (`ShellExecuteW`, `xdg-open`), so the program's own handler
   decides how it opens.
3. **Only a stack's own trees.** `path` must be the stack's repository or
   one of its worktrees (`git worktree list`), and `with` must be in the
   list just found. A path the page invents, a program it names or a
   command line it writes are refused (`400`). Like every mutation, the
   request needs the cookie and the hub's own Origin (ADR-0019).
4. **Where it makes no sense, nothing is offered.** A hub without a desktop
   (Linux without `DISPLAY`/`WAYLAND_DISPLAY`) lists nothing; a lane shared
   with the user (ADR-0040) lives on someone else's machine: the page only
   offers to copy its path. A lane that runs on a node has its worktree here
   (ADR-0025) and is opened like any other.

## Alternatives considered

- **Scheme links from the page only.** No hub code, but the page cannot
  know which programs exist, so it offers all of them; a click on one that
  is not installed does nothing, and the browser asks before each.
- **A program the user configures** (a command line in the hub's settings).
  The hub would start whatever a setting says, a setting the page writes:
  one XSS away from running anything. A fixed list cannot be talked into
  more. It may come later for one "own editor" entry, set from the CLI.
- **`$EDITOR`/`$VISUAL`.** Mostly terminal editors, started without a
  terminal.
- **Each program's CLI** (`code`, `cursor`) instead of its scheme. On
  Windows those are `.cmd` files (a console window, and `cmd.exe` parsing
  the path); the scheme handler is the program's own, and the same on every
  platform. Orca has no scheme, so it is the one exception.
- **The file manager by `explorer.exe <path>`.** It exits with 1 when it
  worked; the shell's `open` verb says what happened.

## Consequences

- The hub starts programs on request of the page. Narrowed by point 3: a
  known program, on a known tree of a registered stack.
- `windows-sys` needs `Win32_UI_Shell` (`ShellExecuteW`) and
  `Win32_System_Com`.
- A program the list does not know (JetBrains IDEs, Visual Studio, Sublime)
  is not offered; adding one is a code change. JetBrains' scheme
  (`jetbrains://`) needs Toolbox and a project name, not a path.
- The hub runs in the background, and Windows lets a background process
  neither take the foreground nor give it. *Amended 2026-10-21:* the
  programs opened behind the browser there. The hub now takes the foreground
  first with a window of its own (the synthetic Alt key event its folder
  dialog uses) and, while it has it, lets any process take it next
  (`AllowSetForegroundWindow(ASFW_ANY)`): the program it starts, or the
  running one a link is handed to. A desktop on Linux that prevents focus
  stealing may still open it behind.
- macOS lists nothing until #66.

## Enforced by

- `crates/devstack-hub/src/open.rs` (the list, finding, starting),
  `crates/devstack-hub/src/foreground.rs` (the foreground on Windows), routes
  `/hub/openers` and `/hub/open` in `crates/devstack-hub/src/lib.rs`.
- `crates/devstack-daemon/src/lanes.rs` (`worktree_of`).
- `ui/src/components/StackHeader.vue` (the path's menu).
- `docs/dashboard-api.md` (`/hub/openers`, `/hub/open`).

## What this ADR does not say

- That the hub opens files or lines (a failed build's error in the editor).
  It opens trees; a file could follow with the same list.
- That the CLI gets a `devstack open`: the terminal is already in the tree.
