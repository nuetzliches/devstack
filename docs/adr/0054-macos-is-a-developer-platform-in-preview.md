# ADR-0054: macOS is a developer platform in preview, installed by Homebrew, checked by CI and by its testers

- **Status:** Accepted (2026-10-23); noted 2026-10-30: CI and release run on `macos-15` (GitHub retires `macos-14` on 2026-11-02); the build still targets macOS 11 (`MACOSX_DEPLOYMENT_TARGET`)
- **Date:** 2026-10-22
- **Deciders:** project owner
- **Related:** [issue #66](https://github.com/nuetzliches/devstack/issues/66) (what macOS would take), [ADR-0011](0011-platform-windows-native-first.md) (Windows-native first), [ADR-0009](0009-the-daemon-owns-every-process-tree.md) (the daemon owns every process tree), [ADR-0023](0023-stack-file-compatibility.md) (migrations once devstack has outside users), [ADR-0021](0021-a-hub-serves-the-dashboard.md) (the hub), [ADR-0025](0025-a-lane-can-run-on-another-node.md) (lanes on nodes), [ADR-0045](0045-the-hub-notifies-the-desktop.md) (notifications), [ADR-0046](0046-the-hub-opens-a-worktree-in-the-users-programs.md) (opening a worktree)

## Context

devstack runs on Windows and Linux (ADR-0011). Someone outside the
maintainers asked for macOS in issue #66: a stack with a service in Docker, a
dev server and local emulators, worked on by agents in parallel worktrees. They
said that local development alone would be a useful start. They are one
person; this is not a measured demand.

The workspace compiles for `*-apple-darwin`, but it does not work there. The
code knows two platforms, and `cfg(not(windows))` means Linux:

- `crates/devstack-proc/src/unix.rs` reads `/proc`. On macOS the process
  lists come back empty, so `stop()` never escalates to SIGKILL, the shim does
  not notice the daemon dying, the restart cleanup finds no orphans (and their
  ports stay taken), and every usage figure is zero. ADR-0009 does not hold.
- `memory_total`, `memory_available` and `cpu_times` return `None`, so the
  lane memory check never refuses.
- The desktop integration is spread over ten files in three crates (autostart,
  the `devstack://` link, notifications, the folder picker, opening a browser
  or a program, starting Docker, self-uninstall). Each of them has a Windows
  branch and an `else` branch that is Linux (XDG, `xdg-open`, D-Bus).

None of the maintainers has a Mac. GitHub's `macos-14` runners are real
macOS machines (Apple Silicon) and cost nothing for a public repository. They
have no Docker, though, and no desktop session anyone looks at.

On distribution: Windows has winget (`packaging/winget/`, submitted by the
release job; `devstack update` hands over to `winget upgrade`). On macOS the
counterpart is Homebrew, and it brings two things this decision needs:

- **No code signing needed to run.** Homebrew puts no quarantine attribute on
  what a formula installs, so Gatekeeper lets an unsigned binary run. A cask,
  a `.pkg` or a binary downloaded with a browser would need a Developer ID
  signature and notarization (99 USD a year, and a Mac to set up).
- **One channel for macOS and Linux.** The same formula installs on Linux
  (Homebrew on Linux), next to `install.sh`.

## Decision

1. **macOS is a developer platform, in preview.** The CLI, the daemon and the
   hub run on a Mac, with lanes on that Mac, on Apple Silicon and Intel,
   macOS 11 or later. Docker is whatever engine `docker` reaches (Docker
   Desktop, OrbStack, Colima). Requesting a lane on a Linux node from a Mac
   uses nothing platform-specific; it is expected to work, and is checked by
   testers.
2. **Correctness first:**
   - process ownership through libproc (process group, start time, CPU and
     RSS), with the shim noticing its parent's death by polling `getppid()`;
   - machine metrics (`sysctl hw.memsize`, `host_statistics64`,
     `HOST_CPU_LOAD_INFO`);
   - usage of the build job without cgroups;
   - real paths for file watching (FSEvents reports `/private/tmp`, not
     `/tmp`, and the target of a symlinked project folder);
   - a port probe that also connects, since a BSD bind on `127.0.0.1` can
     succeed while something holds `*:port`.
3. **The desktop, where it needs no app bundle:**
   - the hub at login through a LaunchAgent
     (`~/Library/LaunchAgents/…plist`, `launchctl bootstrap gui/$UID`);
   - the folder picker through `osascript` (`choose folder`);
   - opening the dashboard, a browser and the user's programs with `open`
     and `open -a` (ADR-0046);
   - starting Docker with `open -g -a Docker` (or OrbStack);
   - `doctor`, `self uninstall` and `devstack update` knowing all of this.
4. **Homebrew is how devstack is installed on macOS, and an option on
   Linux:**
   - a tap of our own, `nuetzliches/homebrew-tap`, with a formula (not a
     cask) that installs the release's binary for the machine:
     `aarch64-apple-darwin`, `x86_64-apple-darwin`,
     `x86_64-unknown-linux-musl` and the new `aarch64-unknown-linux-musl`;
   - the release job renders the formula from `packaging/homebrew/` with the
     release's checksums and pushes it to the tap (as the `winget` job does,
     a dry run without its token);
   - `devstack update` and the dashboard's update hand over to
     `brew upgrade` for a copy Homebrew installed, as they hand over to winget.
     Whatever starts devstack again (the update, the LaunchAgent) uses the
     stable path `$(brew --prefix)/bin/devstack`, never the versioned path
     under `Cellar/`, which `brew cleanup` removes;
   - the hub's LaunchAgent is devstack's own, written by `install.sh`,
     `devstack hub install` or the first `devstack up` (as on Linux and
     Windows), for a Homebrew install too. The formula has no `service`
     block: `brew services` would start a second hub on the same fixed port;
   - `install.sh` takes macOS and ARM Linux as well, for those without
     Homebrew. Its binary can be blocked by Gatekeeper if it was downloaded by
     a browser, not by `curl`; the README says so.
5. **Left out, and said so:** desktop notifications (ADR-0045) and the
   `devstack://` link. Both need an app bundle, and a bundle needs signing to
   behave. macOS as a pool node is also left out. Each of these reports "not
   on macOS yet" in `devstack doctor` and in the dashboard, rather than
   staying silent. They get their own ADR when the preview has testers.
6. **First, the platform code is split along the operating systems, without
   changing any behaviour:**
   - `devstack-proc` gets `posix.rs` (signals, process groups, `rlimit`),
     `linux.rs` (`/proc`, cgroups) and `macos.rs`.
   - The desktop integration moves into one crate, `devstack-desktop`, with
     one module per operating system that implements the same functions.
   - An operating system without a module fails to build (`compile_error!`)
     instead of compiling to silent `None`s and `else` branches.

   This lands as its own change, before any macOS code.
7. **CI checks the preview; testers check the rest.** `macos-14` joins the
   check matrix (clippy, the tests, the daemon's integration test). The
   process tests in `crates/devstack-proc/tests/` get a macOS path. x86_64 is
   built on the same runner and is not tested. Whatever only a Mac with a
   person in front of it can show is listed in the release notes as
   unchecked, with a checklist for testers: Docker and Compose, the
   LaunchAgent, the folder picker and `open -a`, Local Network privacy, and
   file watching under real editors. Reports from testers are bugs like any
   other.
8. **Its users are outside users.** From the first macOS build handed to
   someone outside the maintainers, devstack has outside users, and
   ADR-0023's second phase holds for the stack file.
9. **The preview ends by the owner's decision.** It ends once testers have
   used it without open defects in its scope. Until then macOS is not
   first-class: a feature is done when it works on Windows and Linux, and it
   must not turn macOS CI red.

## Alternatives considered

- **Only the minimum for local development, without Homebrew** (this ADR's
  first draft). It would leave Gatekeeper to each tester and need its own
  update path, while Homebrew solves both and mirrors winget. Rejected.
- **A cask, or a signed `.pkg`, with an app bundle** (and with it
  notifications and the `devstack://` link). That needs a Developer ID,
  notarization in CI and a Mac to set it up, for a demand of one person.
  Postponed (point 5).
- **`brew services` for the hub.** It is what Homebrew users know, but it
  exists only for Homebrew installs and would race devstack's own autostart
  for the hub's fixed port. Rejected.
- **Homebrew core instead of a tap.** Core builds from source and wants a
  notable project. Not now. A tap can move to core later without users
  noticing more than the name.
- **Wait until a maintainer has a Mac.** That would be the safest way to
  build it, but it costs hardware for an unproven demand. A Mac rented by the
  hour remains possible for a bug only real hardware shows.
- **Keep macOS unsupported.** The code compiles for macOS and fails there
  silently, so a curious user would meet a tool that leaves orphans behind.

## Consequences

- A change to platform code needs a macOS module, or an explicit "not on
  macOS yet". CI gets one more job, and it is the slowest runner.
- Every release publishes four more files of work: the two darwin archives,
  the ARM Linux archive, and the formula in the tap. The tap needs a token
  with write access to it (the secret `HOMEBREW_TAP_TOKEN`).
- Bugs that only real hardware shows arrive slowly, through few people, and
  sometimes cannot be reproduced here.
- The stack file changes only additively from the first macOS build on. The
  first break needs `version = 2` and `devstack migrate`, which does not exist
  yet.
- The split of point 6 changes nothing for Windows and Linux, and makes a
  further platform a module instead of a search through `cfg` branches.
- The ARM Linux archive also gives ARM Linux machines (and, later, a node in
  Docker on a Mac) a devstack.

## Enforced by

Where it will live:

- `.github/workflows/ci.yml`: `macos-14` in the check matrix.
- `.github/workflows/release.yml`: `aarch64-apple-darwin`,
  `x86_64-apple-darwin` (`MACOSX_DEPLOYMENT_TARGET=11.0`) and
  `aarch64-unknown-linux-musl`; the `homebrew` job.
- `packaging/homebrew/devstack.rb`, the tap `nuetzliches/homebrew-tap`.
- `install.sh`, `crates/devstack/src/update.rs`: the new assets, and the
  hand-over to `brew upgrade`.
- `crates/devstack-proc/src/{posix,linux,macos}.rs`, `crates/devstack-proc/tests/`.
- `crates/devstack-desktop`, replacing the platform branches in
  `crates/devstack/src/{integration,selfcmd,hubcmd}.rs`,
  `crates/devstack-hub/src/{open,dialog,lib}.rs`, `crates/devstack-hub/src/notify/`
  and `crates/devstack-compose/src/engine.rs`.
- `crates/devstack-daemon/src/watch.rs` (real paths) and
  `crates/devstack-daemon/src/supervisor.rs` (the port probe).
- `crates/devstack/src/doctor.rs` and the dashboard: "macOS preview" and what
  it leaves out.
- `README.md`: macOS and Homebrew. `AGENTS.md` point 6: macOS CI stays green.

*Paths as built, noted 2026-10-23* (#225):

- `crates/devstack-proc/src/{posix,linux,macos,windows}.rs`; `lib.rs` picks
  `backend` (posix or windows) and `sys` (linux, macos or windows), any
  other OS is a `compile_error!`.
- `crates/devstack-desktop/src/{windows,linux,macos}.rs`, with the pure
  macOS helpers (plist, AppleScript quoting) in `mac.rs`; the install
  channel (winget, Homebrew, stable path) in `channel`.
- `.github/workflows/ci.yml`: `macos-14` in `check` and `integration`, with
  Spotlight turned off on the runner (its indexing took the CPU a node test
  offers). `.github/workflows/release.yml`: jobs `homebrew` and
  `homebrew-tap`.
- `packaging/homebrew/{devstack.rb,render.sh}`; `docs/macos.md`, the
  checklist for testers.
- Found by macOS CI and fixed with it: a worktree is recognised under its
  real path (`/var` is `/private/var`, `crates/devstack/src/client.rs`).
- Not built: `.noindex` names for devstack's own directories. On CI runners
  20 000 files cost Spotlight the same in a hidden, a `.noindex` and a plain
  folder; `docs/macos.md` asks testers to watch for it.

## What this ADR does not say

- That macOS becomes a first-class target like Windows (ADR-0011). It is a
  preview until point 9 ends it.
- That a Mac can be a pool node. The ARM Linux archive is a step towards it,
  and nothing more.
- How a lane isolates tools whose ports live in their own configuration file
  (issue #66 names emulators like that). That is a question for every
  platform.
