# ADR-0069: `devstack mcp` can carry the tools of upstream MCP servers, logged in once by the hub

- **Status:** Open
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
0.25.1 the setup prompt offers it, and the dashboard says when it is
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

## Question

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

## Options

- **A. Proxy in the hub, user level only.** `devstack mcp` asks the hub for
  the upstream tools and forwards calls to it (loopback, the user token of
  ADR-0019); the hub speaks to each upstream (stdio or Streamable HTTP),
  with a prefix and a tool allowlist per upstream. Upstreams and logins are
  managed in the dashboard. Smallest that removes the per-harness logins;
  the pool and the stack file stay out. Cost: a second protocol surface in
  the hub, keychain access on three platforms (macOS in preview,
  ADR-0054), an OAuth client implementation.
- **B. A, plus definitions from the pool.** The pool hands upstream
  definitions to its members (never secrets; every member logs in for
  themselves), administered in the pool's dashboard. Cost: precedence and
  opt-out rules (question 1), a pool protocol change.
- **C. B, plus suggestions in the stack file.** A repository names the
  upstreams it works with (non-secret), the dashboard offers to add them.
  Cost: a stack file key (ADR-0023 applies), and a repository steering what
  an agent's harness can reach.
- **D. Do not proxy; manage the harnesses' own configuration.** devstack
  writes the same upstream list into every harness's config (as #317 does
  for `devstack mcp` itself). The lists stop drifting, but every harness
  still logs in and holds tokens on its own: the main problem stays.
- **E. Leave it to a separate gateway** (prior art: MCPJungle, central,
  without OAuth). No work in devstack; no per-user OAuth, and a server to
  run.

## What would decide it

- Which upstreams the maintainers use daily need OAuth, and whether they
  rotate refresh tokens (if none do, D covers most of it).
- Whether the harnesses in use honour `tools/list_changed` (question 3).
- Whether a pool should hand out upstreams at all before user-level
  configuration has been used (B and C can follow A without a break).
- The keychain crates available under an MIT-compatible licence on
  Windows, Linux and macOS.

## Enforced by

Nothing is built; nothing builds on either side until this ADR moves to
`Proposed` or `Accepted`. Where it would live: `crates/devstack-hub`
(upstream connections, OAuth client, token storage, a loopback API for
`devstack mcp`), `crates/devstack/src/mcp.rs` (merging the tool lists,
forwarding calls), `ui/` (upstreams and logins), `docs/dashboard-api.md`.
