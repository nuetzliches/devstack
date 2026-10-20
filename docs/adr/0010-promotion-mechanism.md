# ADR-0010: Promotion mechanism: restart on the fixed port or switch a reverse proxy

- **Status:** Accepted (2026-10-15): option B, the reverse proxy, measured (see "Comparison")
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

## Comparison (2026-10-15)

Measured on Windows with `spikes/promote-proxy` (an L7 proxy with hyper that
runs two instances on internal ports, picks the upstream per request, passes
SSE and WebSocket through and tags every response with `x-devstack-upstream`)
and `spikes/promote-proxy/bench.py` (pollers every 50 ms, an SSE client and a
WebSocket client that reconnect at once). A = `devstack restart` as built in
phase 1; B = the spike's promote. No code change between the instances, so
only the switch is measured.

| Scenario | Failed requests | Longest outage | Long-lived connections |
|---|---|---|---|
| A, fixture API, SSE + WebSocket open | 9 (each a 2 s Windows connect retry) | **18.4 s** for new connections | SSE kept the old instance in its graceful shutdown until devstack killed it at the 20 s stop deadline; Kestrel refuses new connections meanwhile |
| B, fixture API, SSE + WebSocket open | **0** | 0 (max latency 40 ms) | each dropped once when the old instance stopped, back on the new one after 0.2-0.3 s |
| A, reference API (no stream open) | 78 of 347 through the SPA dev server proxy (502) | **3.9 s** | the reference API's SSE stream ends only when the client leaves, not at shutdown: with a browser tab open every restart waits for the stop deadline (as in row 1) |
| B, reference API | **0** (direct and through the SPA dev server proxy) | 0 (max latency 40 ms) | new instance healthy after 4.8 s while the old one served; old stopped gracefully |

Findings beyond the numbers:

- Under A, an open stream turns a restart of a few seconds into one of the stop
  deadline. Browsers keep such streams open as a matter of course.
- The proxy cost no measurable latency (40 ms maximum, the same as direct).
- At the drain deadline the old instance still held the streams and could not
  stop gracefully; the proxy owns the client side of those connections and
  should close them itself (condition 4), then the old instance stops at once.
- Workers (ADR-0008): the reference API switches its workers only at start.
  Under B the promoted instance starts with workers off; switching them on
  needs a restart behind the proxy (requests wait, HTTP keeps working once the
  proxy holds requests during it) or a runtime switch in the service, which is
  the smooth variant and a small change downstream.

## Recommendation

B, with A as the per-service fallback (condition 3). It is the only option that
meets the goal of a smooth experience: the developer and an agent keep a
working service while a change builds and starts, and the switch is invisible
to requests. Transparency comes from the same place: the proxy knows which
instance serves, so `x-devstack-build` on every response, `status`, MCP and the
dashboard can say "a newer build is pending / building / failed" while the old
one still answers.

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

*Paths as built, noted 2026-10-18.* Option B since 0.2.0:
`crates/devstack-daemon/src/proxy.rs` (the reverse proxy on a service's fixed
port; `x-devstack-build` and `x-devstack-pending` on every answer; upstream
connections pooled per port; drain with a deadline; `ProxyView` with
upstream, connections and last error, conditions 2 and 4),
`crates/devstack-daemon/src/supervisor/` (`ensure_proxy`, `switch_to`;
health checks go to the instance's own port, condition 1; `proxy = false`
restarts in place on the fixed port, condition 3), `tcpfwd.rs` (the TCP
forward `promote --lane` uses for dev servers), and the integration test
`edits_become_candidates` (no failed request across a switch).

*Noted 2026-10-20.* Dev servers sit behind devstack too, with a forward of
bytes instead of the HTTP proxy (TLS, WebSockets, HTTP/2 pass unchanged):
[ADR-0043](0043-a-dev-servers-fixed-port-is-devstacks.md).
