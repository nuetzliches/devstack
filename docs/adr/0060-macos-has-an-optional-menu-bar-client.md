# ADR-0060: macOS has an optional menu bar client of the existing control surface

- **Status:** Proposed (2026-10-23)
- **Date:** 2026-10-23
- **Deciders:** project owner (pending review)
- **Related:** [ADR-0007](0007-one-http-api-is-the-control-surface.md), [ADR-0018](0018-dashboard-is-a-browser-page.md), [ADR-0021](0021-a-hub-serves-the-dashboard.md), [ADR-0045](0045-the-hub-notifies-the-desktop.md), [ADR-0054](0054-macos-is-a-developer-platform-in-preview.md)

## Context

The macOS preview has a dashboard hub but no persistent entry in the menu
bar. Opening a terminal to reach that dashboard or start a registered stack
interrupts work in another application. AppKit needs the process's main
thread, while the hub already has a server runtime and serves every stack.

## Decision

1. Add an opt-in `devstack menubar` command on macOS. It is a native AppKit
   status item, without a window, web view, app bundle or extra executable.
   Other platforms report that this command is macOS-only. Existing hub
   startup, installers and login integration remain unchanged.
2. The menu shows the hub's availability and registered local stacks. It
   opens the paired dashboard, starts or stops the hub, starts or stops an
   individual stack and its Compose infrastructure, opens a stack's folder,
   refreshes status, opens GitHub's feedback issue chooser, and exits the
   menu client. Exiting leaves hub and stacks
   running; there is no ambiguous global "quit everything" action.
3. This is another client, not another supervisor. It reads the existing
   registry and authenticated API, and invokes the existing CLI lifecycle
   commands with argument arrays. It implements no process killing, Docker
   ownership, browser pairing or stack lifecycle logic of its own.
4. Network requests and child commands run off the AppKit main thread. The
   main thread owns only the status item and menus; channels carry snapshots
   and user actions. Menu actions show progress and failures. Status refreshes
   periodically; a closed menu client performs no further background work.
5. AppKit bindings are dependencies for macOS only. The Windows binary and
   static Linux binary acquire no new GUI runtime dependency. Unit tests of
   menu decisions run without a desktop; a real Mac checks the native menu.

The status item uses a transparent template version of the existing
dashboard logo. GitHub feedback opens a fixed page without attaching logs
or submitting a report.

This proposes refining ADR-0018's unbuilt per-daemon tray into one optional
menu client for the user's hub. Notifications remain the hub's concern as
decided by ADR-0045.

## Alternatives considered

- A tray per daemon: one icon per repository and none when its daemon stops.
- AppKit inside the hub: entangles the server's lifetime with a GUI run loop
  and changes existing headless and login workflows.
- A separate Swift application: a second build, packaging and update path
  for actions the existing Rust binary can provide.
- A cross-platform tray crate: brings Linux desktop runtime dependencies for
  a macOS preview feature and puts a wider platform decision into this change.

## Enforced by

- `crates/devstack-desktop/src/menubar/`: portable menu model and macOS
  AppKit adapter.
- `crates/devstack/src/menubar.rs`: status collection and command dispatch.
- `crates/devstack/src/main.rs`, `dispatch/`: opt-in CLI entry point.
- `docs/macos.md`: usage, lifecycle and real-Mac verification checklist.
