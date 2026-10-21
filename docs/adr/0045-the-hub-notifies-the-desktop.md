# ADR-0045: The hub notifies the desktop of what needs attention, without a tray

- **Status:** Accepted (2026-10-21)
- **Date:** 2026-10-21
- **Deciders:** project owner
- **Related:** [#156](https://github.com/nuetzliches/devstack/issues/156), [#1](https://github.com/nuetzliches/devstack/issues/1) (the tray), [ADR-0018](0018-dashboard-is-a-browser-page.md) (point 2 amended here), [ADR-0019](0019-control-api-security.md), [ADR-0021](0021-a-hub-serves-the-dashboard.md), [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md), [ADR-0025](0025-a-lane-can-run-on-another-node.md)

## Context

A failed build, a candidate held for `promote` (ADR-0013) or a service that
keeps crashing shows in the dashboard and in `devstack status`, and nowhere
else. Someone working in an editor, or an agent's lane running beside them,
finds out when they look. ADR-0018 point 2 puts operating-system
notifications in a tray icon, and the tray is deferred (#1): the usual Rust
tray crates link GTK and AppIndicator, which the static Linux binary cannot
carry. Notifications do not need a tray, on either platform.

The hub runs per user from logon (ADR-0021) and reaches every stack's
daemon. Each daemon's event stream (`/v1/events`) already says what changed;
a lane's stream is under `/v1/lanes/<lane>/v1/events` of the same daemon,
for a lane on a node too (ADR-0025, passed through).

Measured on Windows 11 (2026-10-21): a WinRT toast shows for an unpackaged
program whose AppUserModelID is registered under
`HKCU\Software\Classes\AppUserModelId\<id>` (`DisplayName`), without a
Start-menu shortcut; the key gives it its name. Whether the user switched
the program's notifications off can be asked only once it has shown one:
before that Windows answers "not found".

## Decision

1. **The hub notifies.** It follows the event stream of every running stack
   and of every started lane of it, on this machine or on a node, as the
   dashboard does. Daemons, node agents and coordinators never notify; a
   machine without a hub (a server node) gets none.
2. **What notifies**, each kind on or off:

   | Kind | When | Default |
   |---|---|---|
   | `build_failed` | a candidate's build failed, or a service's build at its start | on |
   | `held` | a candidate is held and waits for `promote` | on |
   | `service_failed` | a service exited by itself, or failed to start | on |
   | `lane_failed` | a lane's start on a node or its move failed | on |
   | `main_ports` | a lane took or gave back the main ports | off |
   | `live` | a candidate went live | off |
   | `guest_lane` | a lane of another member started running on this machine | on |

   A dev server's rebuild (ADR-0042) does not notify: it fails on every
   unfinished edit, and its own overlay says so.
3. **Only what the hub sees change.** The state when the hub connects to a
   stream (at its start, after a daemon restart) is the baseline: what had
   already failed then does not notify. The same kind for the same service
   of the same stack or lane notifies once in five minutes: a crash loop is
   one notification, not one per restart.
4. **Windows:** a WinRT toast. The hub registers its AppUserModelID
   (`devstack.hub`) under `HKCU\Software\Classes\AppUserModelId` when it
   starts; no Start-menu shortcut, no admin rights. `devstack self uninstall`
   removes it.
5. **Linux:** `org.freedesktop.Notifications` on the session bus, in pure
   Rust (`zbus`), so the binary stays static. Without a session bus (a
   server, a container) notifications are off and nothing fails.
6. **A click opens the dashboard** on the stack, lane and service the
   notification is about. Windows: the toast launches
   `devstack://open?stack=…&lane=…&service=…`, which pairs the browser as
   `devstack ui` does; without the registered link, the hub's own address.
   Linux: the hub hears the click on the bus and does the same. The
   `devstack://` link (ADR-0021) therefore knows a second action, `open`;
   like `hub/start` it only shows the user something, and its values are
   checked to be names before they become a dashboard path.
7. **The settings are the person's, per machine:** the hub keeps them
   (`notify.json` beside its state), not the stack file. Each kind on or
   off, and a mute per stack or per lane. `GET`/`PUT /hub/notify` and
   `POST /hub/notify/test`; the dashboard shows them behind a bell in its
   top bar, the CLI as `devstack hub notify`.
8. **ADR-0018 point 2 is amended:** notifications come from the hub, not
   from a tray. A tray (#1), when it comes, adds the icon and uses the same
   notifications.

## Alternatives considered

- **Notifications in the tray, as ADR-0018 said.** Ties them to the part
  that cannot be built without losing the static Linux binary (#1).
- **The daemon notifies.** One daemon per stack, a lane's in the main
  tree's, a lane on a node in a container there: several senders that know
  nothing of each other, and settings in every one. The hub is one per user
  and already sees all of them.
- **The browser's Notification API.** Needs an open dashboard tab, the
  case where a notification matters least. It may come later, using the
  same settings, so nothing shows twice.
- **A Start-menu shortcut carrying the AppUserModelID** (the documented way
  for desktop programs). A file in the user's Start menu for a program that
  has no window to start; the registry key is enough, measured above.
- **`notify-send` on Linux.** No dependency, but it is a separate package,
  often missing, and its click handling (`--wait --action`) holds a process
  per notification.
- **A `[notify]` table in the stack file.** Whether someone wants to be
  told is theirs, not the repository's; an agent's lane is muted from the
  dashboard, not by a commit.

## Consequences

- The hub holds a connection to every running stack and started lane even
  when no dashboard is open: one more stream per daemon, and, for a lane on
  a node, through the tunnel to it.
- A notification needs the hub. Without autostart (`DEVSTACK_NO_HUB_INTEGRATION`)
  there are none until something starts it.
- Two new dependencies: `windows` (WinRT, Windows only) and `zbus` (Linux
  only), both MIT-licensed.
- A click on Windows goes through the `devstack://` link when it is
  registered; it then works whichever browser was paired before, at the cost
  of a pairing when that browser was never paired.
- macOS has none until #66.

## Enforced by

- `crates/devstack-hub/src/notify/` (`mod.rs`: following the streams,
  settings, folding; `rules.rs`: what notifies; `windows.rs`, `linux.rs`),
  started from `serve` in `crates/devstack-hub/src/lib.rs`; `/hub/notify`
  routes there; `/auth` takes `next`.
- `crates/devstack/src/integration.rs` (`open_url`: `devstack://open`),
  `crates/devstack/src/selfcmd.rs` (removes the AppUserModelID),
  `crates/devstack/src/dispatch/hub.rs` (`devstack hub notify`).
- `ui/src/components/NotifyMenu.vue`; `ui/src/views/StackLayout.vue`
  (`?lane=` in a link).
- `docs/dashboard-api.md` (`/hub/notify`).

## What this ADR does not say

- That devstack notifies about the pool (a member waiting for approval, a
  node going offline): the kinds above are about one's own stacks.
- That a notification carries anything to act on (promote, restart): a
  click opens the dashboard, where the action is.
