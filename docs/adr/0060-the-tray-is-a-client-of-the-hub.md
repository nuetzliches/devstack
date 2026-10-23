# ADR-0060: The tray is a client of the hub, a process of its own, on every desktop

- **Status:** Accepted (2026-10-23)
- **Date:** 2026-10-23
- **Deciders:** project owner
- **Related:** [#1](https://github.com/nuetzliches/devstack/issues/1) (the tray), [#248](https://github.com/nuetzliches/devstack/pull/248) (a macOS menu bar client, which this replaces), [ADR-0007](0007-one-http-api-is-the-control-surface.md), [ADR-0018](0018-dashboard-is-a-browser-page.md) (point 2 amended here), [ADR-0021](0021-a-hub-serves-the-dashboard.md) (where the tray lives, amended here), [ADR-0045](0045-the-hub-notifies-the-desktop.md), [ADR-0054](0054-macos-is-a-developer-platform-in-preview.md)

## Context

ADR-0018 point 2 put a tray icon in the daemon: the stack's state as green,
yellow or red, a menu to open the dashboard and stop the stack. ADR-0021
moved it to the hub, the one long-lived process per user. ADR-0045 gave the
notifications to the hub without a tray, because the usual Rust tray crates
link GTK and AppIndicator, which the static Linux binary cannot carry (#1).
What is left of the tray is the icon: one glance that says whether
something needs attention, and a way to the dashboard and to starting or
stopping a stack without a terminal.

A contribution for macOS (#248) built that as a separate process,
`devstack menubar`, and showed why it is one. AppKit's status item must
live on the process's main thread, and the hub's main thread runs its
server. It also showed what goes wrong when the client works out a stack's
state for itself: it read each daemon's `/v1/stack` every five seconds and
judged services, containers, exit codes and profile skips on its own, a
third reading of the state beside the dashboard's and the notifications',
which would drift from them.

The pieces a tray needs exist on every platform without a GUI runtime:

- Windows: `Shell_NotifyIconW` and a popup menu, in `windows-sys`.
- Linux: `org.kde.StatusNotifierItem` and `com.canonical.dbusmenu` on the
  session bus, in pure Rust over `zbus` (already used for ADR-0045), so the
  binary stays static. KDE, Xfce, Cinnamon, and GNOME with the AppIndicator
  extension (Ubuntu's default) show it.
- macOS: `NSStatusItem` through `objc2`, linked on macOS only.

## Decision

1. **The tray is its own process, `devstack tray`, on every platform.** One
   per user (a lock beside the hub's state); a second one exits. It is a
   client of the hub as the dashboard and the CLI are (ADR-0007): it never
   talks to a daemon, starts a stack itself or works out a state. Its main
   thread runs the platform's tray; a worker does the requests.
2. **The hub starts it.** When the hub starts it runs `devstack tray` in the
   background, unless the tray is switched off (`devstack hub tray off`,
   kept per machine as `tray.json` beside the hub's state, or
   `DEVSTACK_NO_TRAY`) or there is no desktop (Linux: no session bus). So it
   comes at logon with the hub (ADR-0021), with no autostart of its own. The
   tray outlives a hub that stops: it then says so and offers to start it.
   A tray of an older devstack than the hub's starts the new one and exits.
   *Quit* in its menu ends it until the hub's next start; `devstack hub tray
   off` ends it for good, `on` starts it again.
3. **The hub says what the tray shows: `GET /hub/summary`.** Per registered
   stack whether it runs, a start or stop under way, its services running of
   all, its started lanes, and what needs attention; and one state over all
   of them: `attention`, `starting`, `ok` (something runs) or `idle`. What
   needs attention is what ADR-0045 notifies, read from the same streams the
   hub follows already: a service that failed (not one the user stopped, not
   one held back by another's failure), a candidate held for `promote` or
   whose build failed, in the main tree and in every started lane; and a
   failed start from the dashboard. Containers are the dashboard's: the
   daemon's stream carries their state only in its snapshot.
4. **The icon is the dashboard's mark, in the state's colour:** green when
   something runs and nothing needs attention, amber while a stack starts or
   stops, red when something needs attention, grey when nothing runs. On
   macOS it is a template image, so it follows the menu bar's light and
   dark, tinted orange or red for those two states and plain otherwise, as
   menu bar icons are. The tooltip says the same in words.
5. **The menu:** the overall state; *Open Dashboard*; per stack a submenu
   with its state, what needs attention (a click opens the dashboard on that
   service), *Open in Dashboard*, *Start* or *Stop*, *Open Folder*; *Start
   Hub* while the hub does not run; *Quit*. Every action is a request to the
   hub: `POST /hub/stacks/{id}/up`, `…/down` (as the dashboard's Stop:
   lanes keep running), `POST /hub/open` with `files`, a paired browser as
   `devstack ui` opens it. Nothing the menu does stops the hub or a lane.
6. **The model is the same everywhere, the drawing is per platform.** The
   menu is a tree of labels, actions, separators and submenus with an icon
   state and a tooltip, made once in the CLI; `devstack-desktop` shows it
   with one module per operating system (ADR-0054 point 6).

ADR-0018 point 2 is amended: the tray is a client process the hub starts,
not part of the daemon or the hub; it is on by default and switched off
with `devstack hub tray off` instead of `--no-tray`; it does not promote
candidates (the dashboard does, where the diff is shown). ADR-0021's "the
tray belongs to the hub" now means the hub starts it and tells it what to
show.

## Alternatives considered

- **The tray inside the hub (ADR-0021 as written).** One process less, but
  on macOS the hub's main thread would have to become AppKit's and its
  server move to another thread, and a hub that restarts for an update
  takes the icon with it. On Windows and Linux it would work; one model for
  all three is simpler.
- **A macOS-only menu bar client (#248).** The same need exists on Windows
  first (#1), and its own reading of every daemon's state would differ from
  the dashboard's.
- **Its own autostart at logon.** A second entry to register and remove
  per platform, for a process that is useless without the hub.
- **A tray crate (`tray-icon`, `ksni`).** `tray-icon` links GTK on Linux;
  `ksni` would bring a second `zbus` version. The three platform calls are
  small.
- **The tray following `/hub/events` instead of polling `/hub/summary`.**
  The summary is cheap and local; a poll every two seconds keeps the client
  simple and survives a hub restart without reconnect logic.

## Consequences

- A user who never opens a terminal sees whether their stacks run, and
  starts one, from the icon.
- GNOME without the AppIndicator extension shows no icon: the tray runs and
  says nothing. Notifications (ADR-0045) do not depend on it.
- macOS gets the icon without an app bundle: as in #248, the process makes
  itself an accessory application, so it has no Dock icon.

## Enforced by

- `crates/devstack-desktop/src/tray/`: the menu model, the icon, and
  `windows.rs`, `linux.rs`, `macos.rs`.
- `crates/devstack/src/tray.rs`: `devstack tray`, the menu from
  `/hub/summary` and its actions.
- `crates/devstack-hub/src/summary.rs`: `GET /hub/summary`; `tray.rs`: the
  start at the hub's start and `tray.json`.
