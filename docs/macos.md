# devstack on macOS (preview)

macOS is a developer platform in preview
([ADR-0054](adr/0054-macos-is-a-developer-platform-in-preview.md)). None of
the maintainers has a Mac: CI builds and tests devstack on macOS (Apple
Silicon, without Docker), and everything else is checked by the people who
use it. A report of what worked is as useful as a report of what did not:
[issue #66](https://github.com/nuetzliches/devstack/issues/66).

## Install

```sh
brew install nuetzliches/tap/devstack      # Homebrew
# or
curl -fsSL https://github.com/nuetzliches/devstack/releases/latest/download/install.sh | sh
```

Apple Silicon and Intel, macOS 11 or later. Docker comes from Docker Desktop,
OrbStack or Colima (whatever `docker` reaches). The binary is not signed;
Homebrew and `install.sh` do not mark it for Gatekeeper. A copy downloaded
with a browser is blocked until `xattr -d com.apple.quarantine devstack`.

## What is there, and what is not

| | |
|---|---|
| CLI, daemon, hub and dashboard; lanes on the Mac | yes |
| The hub at login | a LaunchAgent (`~/Library/LaunchAgents/it.nuetzliche.devstack.hub.plist`), for a Homebrew install too, not `brew services` |
| Folder picker, "Open in" (Finder, Terminal, editors, GitHub Desktop), browsers | `osascript`, `open`, `open -a` |
| Starting Docker | Docker Desktop or OrbStack; Colima: `colima start` yourself |
| `devstack update`, the dashboard's update | a Homebrew copy through `brew upgrade` |
| Requesting a lane on a Linux node of a pool | expected to work, not checked yet |
| Desktop notifications, the `devstack://` link | not yet: both need a signed app bundle |
| A Mac as a pool node | not yet |

## Checklist for testers

What CI cannot show. Each step names what to look for; anything else is a bug.

**Install and update**

1. `brew install nuetzliches/tap/devstack`, then `devstack --version` and
   `devstack doctor`: doctor says "macOS preview" and lists what is not
   there yet, and nothing about Linux (XDG, D-Bus).
2. After a newer release: `devstack update` (and the dashboard's Update) runs
   `brew upgrade`; afterwards `ps -ax | grep 'devstack hub'` shows
   `/opt/homebrew/bin/devstack`, not a path under `Cellar/`. `brew cleanup`
   afterwards does not break the dashboard's Start/Up buttons.
3. `devstack self uninstall` removes the LaunchAgent and devstack's state,
   keeps the binary and says to run `brew uninstall devstack`.

**The hub at login**

4. `devstack hub install`, then
   `launchctl print gui/$(id -u)/it.nuetzliche.devstack.hub`: loaded; the
   dashboard answers on http://127.0.0.1:4250.
5. Log out and in: the hub runs, and the dashboard sees Docker (the agent's
   `PATH` has `/opt/homebrew/bin` and `/usr/local/bin`).
6. `devstack hub uninstall`: the plist is gone and `launchctl print` fails.

**Processes (ADR-0009)**

7. `devstack up` in a stack with a dev server, then `devstack down`: no
   process of the stack is left (`ps -ax`), its ports are free.
8. `kill -9` the daemon (its pid is in `.devstack/daemon.json`): within about a
   second its services end too; `devstack up` again starts cleanly, without
   "port in use".
9. The dashboard's usage figures (CPU, memory per service, the machine's
   memory) are not zero, and roughly match Activity Monitor.

**Files and ports**

10. A stack in `/tmp/...` or in a symlinked project folder: an edit makes a
    candidate (the watcher sees it).
11. Edits from your editor and from a coding agent both make candidates; an
    `npm install` or `git checkout` of many files ends in one rebuild.
12. A program that listens on `0.0.0.0:<port>` that the stack wants: devstack
    says the port is taken, and by whom.

**Desktop**

13. "Add a stack" in the dashboard: the folder dialog comes up in front.
    Cancel does nothing.
14. `devstack ui`, `devstack ui --browser chrome`, `--browser firefox`.
15. "Open in": Finder, Terminal, and your editor open the worktree; GitHub
    Desktop both when it is closed and when it runs.
16. Docker Desktop quit, then `devstack up`: Docker starts in the background.
    With OrbStack instead: OrbStack starts.

**Network**

17. With the hub started at login (step 5), joining or using a pool on your
    LAN works; if macOS asks about Local Network access, say so in the
    report, with what you answered.
