# ADR-0010: Promotion mechanism: restart on the fixed port or switch a reverse proxy

- **Status:** Open (leaning: reverse proxy)
- **Date:** 2026-10-14
- **Related:** [ADR-0003](0003-changes-flow-through-candidates.md), [ADR-0008](0008-background-workers-run-only-in-the-active-slot.md)

## Context

A promoted candidate has to end up behind the service's fixed port. Clients
(browser, SPA dev server proxy, tests, other services) only know the fixed port.
The previous orchestrator put a proxy in front of every endpoint, and that proxy
wedged in a way that was invisible and only cleared by a full restart. Any
choice here must not repeat that.

## Question

How does a new artifact take over the fixed port?

## Options

### A. Restart on the fixed port

The service binds the fixed port itself. Promotion = stop old main gracefully,
start the new artifact on the fixed port with workers, wait for health.

- **+** No layer between client and service. Nothing to wedge.
- **+** Trivial to implement and to reason about.
- **−** Downtime per promotion = graceful stop + start-up + health
  (typically seconds, longer for slow hosts). Requests in flight fail; open
  SSE/WebSocket connections drop.
- **−** The candidate's health check ran on another port; the port binding on the
  fixed port is checked only after the switch.
- **−** Lanes cannot be addressed on the fixed port side by side.

### B. Reverse proxy owned by the daemon

The daemon listens on the fixed port and forwards to the current main instance
on an internal port. Promotion = start new main with workers off on its port,
flip the upstream, drain the old instance, then stop the old instance and
switch workers on in the new one (ADR-0008 forbids two instances with workers).

- **+** Near-zero downtime; requests during promotion wait instead of failing.
- **+** Rollback is an upstream flip to a still-running instance (if kept warm).
- **+** Lanes become addressable side by side (e.g. `<lane>.localhost:<port>` or
  a header), which helps phase 3.
- **−** Workers still need a restart or a runtime toggle in the new instance,
  so "zero downtime" applies to HTTP, not to background work.
- **−** HTTP/1.1, HTTP/2, SSE, WebSocket, TLS for dev servers must all pass
  through correctly.
- **−** An extra layer, which is what failed last time.

## What would decide it

- Measured promotion downtime of option A on the reference stack (phase 1).
- Whether the downstream API can toggle workers at runtime (cheap B) or needs a
  restart (B loses much of its advantage for the API, keeps it for pure HTTP
  services).
- How important side-by-side lane addressing becomes in phase 3.

## Conditions a proxy must meet if chosen

1. Health checks always go to the instance's own port, never through the proxy,
   so a proxy fault cannot masquerade as an unhealthy service.
2. The proxy's state (upstream, active connections, last error) is visible in
   the API and dashboard.
3. The proxy can be bypassed per service (`proxy = false`), falling back to
   option A.
4. Long-lived connections (SSE, WebSocket) are drained with a deadline, then
   closed, so clients reconnect to the new instance.

## Enforced by

Nothing yet. Phase 1 implements option A as the baseline, because option B
needs it as its fallback anyway (condition 3).
