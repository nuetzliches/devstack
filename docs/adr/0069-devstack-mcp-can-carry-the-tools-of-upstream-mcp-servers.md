# ADR-0069: `devstack mcp` can carry the tools of upstream MCP servers, logged in once by the hub

- **Status:** Proposed (2026-10-27); amended 2026-10-27 (the keychain spike: `keyring`, a file on Linux without a Secret Service)
- **Date:** 2026-10-27
- **Deciders:** project owner
- **Related:** [#316](https://github.com/nuetzliches/devstack/issues/316), [ADR-0007](0007-one-http-api-is-the-control-surface.md), [ADR-0019](0019-control-api-security.md), [ADR-0021](0021-a-hub-serves-the-dashboard.md), [ADR-0032](0032-the-pool-is-administered-in-the-dashboard.md), [ADR-0054](0054-macos-is-a-developer-platform-in-preview.md), [ADR-0060](0060-the-tray-is-a-client-of-the-hub.md), [ADR-0067](0067-a-change-at-runtime-says-who-made-it.md)

## Context

Developers run several agent harnesses side by side (Claude Code, Codex,
opencode, ...). Each keeps its own list of MCP servers, and for every remote
server that needs OAuth each one runs its own login and keeps its own
tokens. Adding a server, or logging in again, is done once per harness, and
the lists drift apart.

`devstack mcp` is registered in each of these harnesses already (since
0.26.0 the setup prompt offers it, and the dashboard says when it is
missing). The hub runs per user across all stacks from logon, on a fixed
loopback port, without a stack (ADR-0021), and serves the dashboard there.

The proposal (#316): `devstack mcp` exposes, beside its own tools, the tools
of upstream MCP servers the user configured; the hub owns the connections
to those upstreams and their tokens. One login per upstream, the same tools
in every harness.

What is settled by the constraints, whichever way the questions below go:

- **The hub is the owner, not a stack's daemon.** A daemon exists per stack;
  the tokens are the user's. Every harness starts its own `devstack mcp`;
  several processes refreshing at once invalidate each other where an
  upstream rotates refresh tokens. One owner avoids that.
- **No token passthrough** (MCP security best practices): a token a harness
  holds is never sent upstream, an upstream's token never leaves the hub.
  The hub is an OAuth client of each upstream per user (authorization code
  with PKCE, dynamic client registration where offered); tokens in the OS
  keychain. A static bearer token comes from the keychain or the
  environment, never from a file in the repository.
- **Secrets never in shared configuration**: not in the stack file (shared
  with everyone who has the repository), not in a pool profile.
- **An upstream that is down or logged out does not take devstack's own
  tools with it.**
- **Out of scope**: a registry or marketplace, a policy engine, caching or
  rewriting content, a gateway on a server for many users.

## Decision

Option A of the alternatives below: a proxy in the hub, configured per
user. *Decided 2026-10-27; the record of the question it answered is under
"The question as it was open".*

1. **The hub connects to the upstreams.** It speaks MCP to each upstream the
   user configured, over stdio or Streamable HTTP, with a prefix
   (`<upstream>__<tool>`) and a tool allowlist per upstream. `devstack mcp`
   asks the hub for their tools over loopback (the user token of ADR-0019),
   lists them beside its own and forwards their calls to the hub. A call
   for devstack's own tools never goes through an upstream.
2. **Upstreams are configured per user only**, in the hub's settings,
   managed in the dashboard (add, remove, allowlist, log in, log out). The
   pool and the stack file hand out nothing (questions 1; B and C can
   follow without a break).
3. **Identity: OAuth per user.** The hub is an OAuth client of each
   upstream (authorization code with PKCE, dynamic client registration
   where the upstream offers it), its redirect target on the hub's loopback
   port. Tokens in the OS keychain; no token passthrough either way. An
   upstream with a static bearer token takes it from the keychain or the
   environment. The pool's client certificate is not used towards upstreams
   (question 2).
4. **A logged-out upstream stays listed.** Its tools are in the list and
   answer with an error that says to log in from the dashboard (with the
   link); an upstream that is down answers that it is down. When an
   upstream's tools change, `devstack mcp` sends
   `notifications/tools/list_changed`, and works the same with a harness
   that ignores it (question 3).
5. **`devstack mcp` starts the hub** when it is not running, as `devstack
   ui` does; if it cannot, it serves its own tools and says why in each
   upstream tool's error (question 4).
6. **No browser, no OAuth.** On a machine without one (CI, headless,
   `DEVSTACK_NO_HUB`), only upstreams with a static token from the
   environment are used; the others are listed as logged out (question 5).

Before it is built, a spike settles the keychain: crates with an
MIT-compatible licence for Windows (Credential Manager), Linux (Secret
Service, and what to do without one) and macOS (Keychain, ADR-0054). If
none fits, this ADR is amended before any code depends on it.

*Amended 2026-10-27, the keychain spike.* The crate is `keyring` 4 (MIT OR
Apache-2.0, with its stores for Windows, macOS and the Linux Secret
Service; Rust 1.88 at most): every crate it pulls in, on every platform, is
on the licence list of `deny.toml`. Written, read and removed on Windows
(Credential Manager); macOS and Linux run the same test in CI. A Linux
machine without a Secret Service (a server, a headless session, CI) has no
keychain to use: the hub finds that with a probe entry when it opens the
store, and keeps the secrets in `secrets.json` in its state directory
instead, readable by the user alone (`0600`, as the hub's sessions file);
the dashboard is to say so. On Windows and macOS there is no file: a
keychain that refuses is said when a secret is used.

## Alternatives considered

- **A. Proxy in the hub, user level only.** Chosen.
- **B. A, plus definitions from the pool.** The pool hands upstream
  definitions to its members (never secrets), administered in the pool's
  dashboard. Needs precedence and opt-out rules and a pool protocol change
  before the user level has been used; left for later.
- **C. B, plus suggestions in the stack file.** A repository names the
  upstreams it works with. A stack file key (ADR-0023), and a repository
  steering what an agent's harness can reach; left out.
- **D. Do not proxy; write the harnesses' own configuration** (as #317 does
  for `devstack mcp`). The lists stop drifting, but every harness still
  logs in and holds tokens on its own: the main problem stays.
- **E. A separate gateway** (prior art: MCPJungle). Central, without
  per-user OAuth, and a server to run.
- **The pool's client certificate as identity** (question 2): revocation
  for free where an upstream trusts the pool's CA, but it ties upstreams
  to the pool, and no upstream is known to accept one.
- **Hiding a logged-out upstream's tools** (question 3): a harness that
  ignores `list_changed` never sees them come back, and the agent cannot
  tell the user to log in.

## Consequences

- A second protocol surface in the hub, an OAuth client and keychain
  access on three platforms: several pull requests, the keychain first.
- The hub holds every upstream token of the user. Its loopback API stays
  behind the user token (ADR-0019); a token never leaves it.
- Every harness that has `devstack mcp` gets the same upstream tools; one
  that should not, does not register `devstack mcp` or the user narrows the
  allowlist.
- On CI and headless machines only static tokens work.

## The question as it was open

'''
How does `devstack mcp` carry upstream tools, and what is decided for each
of these?

1. **Where upstreams are configured, and which wins.** User level (hub
   settings) only, or also handed out by the pool (ADR-0032, never secrets)
   and suggested by the stack file (non-secret declarations)? May a user
   switch off an upstream the pool hands out?
2. **Identity towards an upstream.** Per-user OAuth only, or may an upstream
   that trusts the pool's CA accept the member's client certificate, so
   that removing a member also revokes access there?
3. **A logged-out upstream in a harness.** Its tools hidden until the user
   logs in, or listed and failing with "log in from the dashboard"? Does
   `devstack mcp` send `notifications/tools/list_changed` when an upstream
   comes or goes, given that harnesses differ in honouring it?
4. **Hub not running.** Does `devstack mcp` start it (as `devstack ui`
   does), or offer only its own tools until it runs?
5. **Machines without a browser** (headless, CI): no OAuth browser flow.
   Static tokens from the environment only, a device-code flow where an
   upstream offers it, or no upstreams there?


## Enforced by

`crates/devstack-hub/src/secrets.rs` (the store, with its test against
the machine's real keychain on every platform). To be built:
`crates/devstack-hub` (upstream
connections, OAuth client and redirect, a loopback API for
`devstack mcp`), `crates/devstack/src/mcp.rs` (merging the tool lists,
forwarding calls, starting the hub), `ui/` (upstreams and logins),
`docs/dashboard-api.md`.
