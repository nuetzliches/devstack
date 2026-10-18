# ADR-0018: The dashboard is a browser page; native integration is a tray in the daemon, not a desktop wrapper

- **Status:** Accepted (2026-10-14); who serves the dashboard amended by [ADR-0021](0021-a-hub-serves-the-dashboard.md) (the hub); the tray is not built yet
- **Date:** 2026-10-14
- **Deciders:** project owner
- **Related:** [ADR-0006](0006-dashboard-vue-same-origin.md), [ADR-0007](0007-one-http-api-is-the-control-surface.md), plan §6

## Context

ADR-0006 settled the dashboard's stack (Vue 3, served same-origin by the
daemon). It left open how the dashboard reaches the user: as a page in the
browser, or as a desktop application, and if so, which kind. devstack has to
work on Windows, Linux and macOS, and its daemon is a single Rust binary
(ADR-0005).

Two things a browser tab cannot do are genuinely useful here: a persistent
status indicator that is visible without a tab open, and operating-system
notifications when something needs attention (a failed build, a held
candidate, a crashed service).

## Decision

1. **The dashboard is a web page served by the daemon.** `devstack ui` opens it
   in the default browser. It talks to the daemon's HTTP API and event stream
   only, never to the CLI or the MCP server, which are clients of the same API
   themselves (ADR-0007).
2. **Native integration lives in the daemon binary, without a web view:** a
   tray icon (stack state as green / yellow / red, menu to open the dashboard,
   promote held candidates, stop the stack) and operating-system notifications.
   Clicking opens the dashboard in the browser. It is optional and can be
   disabled (`--no-tray`), e.g. on headless machines.
3. **Should a dedicated window ever be needed, it is a Tauri shell that loads
   the dashboard from the daemon,** not a bundled copy, so there is exactly one
   dashboard codebase and both ways of opening it always show the same
   version.

The API is described by an OpenAPI document generated from the daemon's Rust
types; the dashboard's TypeScript client is generated from it. Live state
reaches the dashboard through the SSE stream (`/v1/events`). How the browser
authenticates (a one-time code from `devstack ui` exchanged for an
`HttpOnly; SameSite=Strict` cookie, plus an `Origin` check) is decided in the
security ADR planned for phase 1.

## Alternatives considered

- **Electron.** A window without a browser, but it ships its own Chromium and
  Node runtime (around 150 MB per platform), needs its own update and signing
  pipeline, adds an attack surface, and brings a second runtime next to a Rust
  daemon. The browser already provides the window on every platform.
- **Tauri from the start.** Small (operating-system web view, Rust), but still a
  separate application to build, sign and update per platform, for a window the
  browser already provides. Kept as the option in point 3.
- **A terminal UI only.** Cheap and scriptable, but no clickable log search, no
  build timeline, nothing to share a link to. A TUI may still come as a CLI
  feature; it is not the dashboard.
- **The dashboard driving the CLI or MCP.** An extra layer with its own
  semantics, the drift ADR-0007 exists to prevent.

## Consequences

- No installer or updater for the dashboard; it versions with the daemon.
- Browser notifications would require an open tab; that is why notifications
  belong to the tray.
- The tray adds per-platform code (Windows notification area, macOS menu bar,
  Linux StatusNotifierItem, which not every Linux desktop shows). The dashboard
  must therefore never depend on the tray being present.

## Enforced by

- Daemon (planned): serves the dashboard, owns the tray (`tray-icon` crate or
  equivalent), `--no-tray`.
- `ui/` (planned): generated API client only; no other backend.

*Paths as built, noted 2026-10-18.* The hub serves the dashboard, not the
daemon ([ADR-0021](0021-a-hub-serves-the-dashboard.md)): `crates/devstack-hub`,
opened by `devstack ui` (`crates/devstack/src/hubcmd.rs`). The dashboard's
client is hand-written, not generated (`ui/src/api/`: `types.ts`,
`transport.ts`, `real.ts`; no OpenAPI document exists, the contract is
[dashboard-api.md](../dashboard-api.md)), and its live state comes over the
hub's WebSocket. Point 2 is not built: there is no tray, no notification and
no `--no-tray` yet (plan §13).
