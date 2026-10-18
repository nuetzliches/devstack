# ADR-0006: The dashboard is a Vue 3 SPA served same-origin by the daemon

- **Status:** Accepted; who serves the dashboard replaced by [ADR-0021](0021-a-hub-serves-the-dashboard.md) (the hub, for every stack); the rest holds
- **Date:** 2026-10-14
- **Deciders:** project owner
- **Related:** [ADR-0007](0007-one-http-api-is-the-control-surface.md)

## Context

Developers need one screen for stack state, build history and logs; the
reference orchestrator's dashboard was the part people liked. The organisation's
frontends, including Croniq's dashboard, are Vue 3 + TypeScript (Nuxt UI,
Tailwind 4, TanStack Query, Pinia, Vite). The daemon exposes a control API that
can stop processes and run commands, so the dashboard must not open a
cross-origin path to it.

## Decision

The dashboard is a Vue 3 + TypeScript SPA under `ui/`, using the same stack as
Croniq's dashboard. The daemon serves the built bundle from the same origin as
its API and event stream. The bundle is embedded into the binary at release
build time, so a single binary is the whole product. The dashboard loads nothing
from third-party origins.

## Alternatives considered

- **TUI only.** Cheap, but no clickable logs, no build timeline, nothing to
  share a link to.
- **Separate dev server for the UI with CORS.** Opens the API to cross-origin
  requests from the browser; rejected for the reason Croniq rejected it.

## Consequences

- Node toolchain needed to build releases, not to run devstack.
- Dashboard and API version together.

## Enforced by

- `ui/` (planned), embedding in the daemon crate (planned).

*Paths as built, noted 2026-10-18.* `ui/` (Vue 3, Vite, TypeScript; `npm
test` runs its unit tests). The bundle is embedded into the hub, not the
daemon (ADR-0021): `crates/devstack-hub/build.rs` copies `ui/dist` (a placeholder page without
a build) and
`include_dir!` in `crates/devstack-hub/src/lib.rs` (`UI`, `asset`) serves it
from the hub's origin, with a `Content-Security-Policy` that allows scripts
and connections from that origin only (`page_headers`).
