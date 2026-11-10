# ADR-0069: `devstack mcp` can carry the tools of upstream MCP servers, logged in once by the hub

- **Status:** Accepted (2026-10-27, [card](https://github.com/nuetzliches/devstack/issues/316#issuecomment-6452598449)); amended 2026-10-27 (the keychain spike: `keyring`, a file on Linux without a Secret Service); addendum 2026-10-28 (option B: a pool names upstreams, card approved 2026-10-28); addendum 2026-10-30 (an OAuth app's client secret, card approved 2026-10-30); addendum 2026-11-04 (the upstreams' instructions in `initialize`, card approved 2026-11-04); addendum 2026-11-06 (a pool's upstream an agent has set up itself, card approved 2026-11-06); addendum 2026-11-10 (a filter on a tool's result, card approved 2026-11-10)
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


## Addendum 2026-10-28: a pool names upstreams (option B)

Option B, left for later above, is built; its review card was approved by
the owner on 2026-10-28 (all five points) and is in the description of the
pull request that built it.

1. **A pool names MCP upstreams for its members** (name, URL, prefix,
   allowlist, how it logs in, a public client id), never a secret or a
   token; logging in and the tokens stay in each member's hub (point 3
   above is unchanged).
2. **An entry is a URL, never a command.** If it could be one, whoever holds
   the coordinator or an admin's machine would run code on every member's.
   The URL is `https://` only: a pool's `localhost` is each member's own
   machine. A token login names no variable either: a pool could otherwise
   send a member's environment to its URL.
3. **An entry is used only once the member accepted it** in the dashboard,
   at the URL the pool names then; a pool that changes the URL has it
   accepted again, and the login for the old one goes. Without this, a
   coordinator could point every member's agents, and what they send, at a
   server of its choosing, and feed them tool descriptions.
4. **The member's own configuration goes first:** an entry with the name or
   prefix of one of the member's upstreams is not used, a declined one stays
   declined, and two pools' entries with one prefix are neither used until
   the member gives one of them another.
5. **The list lives with the coordinator** (`upstreams.json` in its
   directory); an admin changes it with `devstack pool upstreams` or in the
   pool area of the dashboard, each change in the audit log. Members read it
   with `GET /v1/upstreams`; a coordinator of an earlier release answers
   `404`, read as an empty list.

Decided while building: the hub asks its pools every minute, off the
requests, and at once after its own admin changed a list; it keeps the
lists in `pool-upstreams.json` and uses the last one of a pool that does not
answer. An accepted entry is `<pool>:<name>` in the hub (its keychain entry,
its log, its tools' description). An entry the pool drops, or of a pool the
machine left, goes with its decision and login. A change of an entry's
prefix, allowlist or login kind by the pool needs no new acceptance; only
its URL does. A pool names at most 64. Building it showed that the Windows
Credential Manager loses writes and deletes made side by side from several
threads: the hub now makes one keychain call at a time.

## Addendum 2026-10-30: an OAuth app's client secret

GitHub's MCP server (`api.githubcopilot.com/mcp`) names its authorization
server (`github.com/login/oauth`, PKCE with S256), but GitHub registers no
clients itself and wants a client secret at its token endpoint even with
PKCE. Its review card was approved by the owner on 2026-10-30 (all four
points) and is in the description of the pull request that built it.

1. **An OAuth upstream may carry a client secret**, with its client id, for
   an authorization server that wants one even with PKCE. It is the secret
   of an app registered for desktop clients, not confidential by nature
   (RFC 8252 §8.5, as with the `gh` CLI), and the hub sends it only to the
   token endpoint the upstream's metadata names (code exchange and
   refresh). A device flow would need no secret, but a second login path
   and its own UI; a personal access token per member would need no change,
   but is kept by hand and never refreshed.
2. **On the user's own upstream it goes into the keychain**, never into
   `upstreams.toml`; the API takes it and only ever answers whether one is
   set.
3. **A pool may name one** with its client id: the coordinator gives it to
   approved members with the list (`GET /v1/upstreams`), their hubs keep it
   with the pool's list (`pool-upstreams.json`), and the dashboard, the CLI
   and the audit log show only that one is set. This is the one exception
   to point 1 of the 2026-10-28 addendum ("never a secret").
4. **API and CLI:** `auth: {kind: "oauth", client_id?, client_secret?,
   scopes?}` on write; reads answer `client_secret: true` instead of the
   value; an edit without a secret keeps the one set, an empty one takes it
   away; `devstack pool upstreams add --client-secret … --scope …`.

Decided while building: the login the hub keeps holds the secret it logged
in with, for its refresh; logging out keeps the client secret, removing the
upstream or giving up OAuth drops it (a keychain entry `<name>#client`
beside the login). A secret is up to 1024 characters without spaces. The
dashboard's forms gained the client secret and the scopes for both the
user's and a pool's upstreams; until now an edit there dropped the scopes.
GitHub refuses at its token endpoint with `200` and an `error`: the hub
takes any answer with an `error` as a refusal, with that reason.
An older coordinator drops the field on a change; an older member's hub
ignores it, and its login fails as before.

## Addendum 2026-11-04: the upstreams' instructions go to the agent

Its review card was approved by the owner on 2026-11-04 (all four points)
and is in the description of the pull request that built it.

An MCP server's `instructions` (its `initialize` answer) are in an agent's
context for the whole session, unlike a skill's description, which a
harness may cut when its listing overflows. `devstack mcp` answered
`initialize` with its own text only, so whatever an upstream asked of
agents was lost on the way.

1. **`devstack mcp` adds each upstream's `instructions` to its own**, in
   its `initialize` answer: under a heading with the prefix of the
   upstream's tools (`## \`tracker__\` (MCP server \`tracker\`)`), each line
   quoted, after a sentence saying that what follows is those servers' and
   not devstack's. An upstream's text cannot then pass for devstack's or
   for another upstream's heading. They come from the session the hub
   opens anyway (`mcpclient.rs`, `stdio.rs`); there is no second client.
2. **`initialize` does not wait on an upstream.** The hub asks upstreams
   whose session is not open side by side for at most 3 s, `devstack mcp`
   waits for the hub at most 5 s in all (its start included). An upstream
   that is slow, down, refuses the login or has none is left out without a
   note (a note could carry its URL); the asking goes on and warms the
   tools for the `tools/list` that follows.
3. **What an upstream said is kept like its tools** (point 4 above): the
   last session's `instructions` stay in `upstream-instructions.json` and
   are used while the upstream's URL or command is the same, so a
   logged-out upstream keeps its guidance beside its listed tools; a new
   URL or command, or removing it, forgets them.
4. **They are fixed for a session.** MCP has no notification for changed
   instructions; tools that change later are announced with
   `list_changed` as before, but an upstream that changes its
   instructions, or comes or is accepted during a session, is seen in the
   next one.
5. **Bounded, and nothing of the hub's**: at most 4000 characters per
   upstream (marked as cut), 16 000 in all (an upstream past that is only
   named), control characters other than line ends and tabs taken out.
   Only the upstream's own text and its name and prefix go out, never a
   URL, a command or a credential.

An upstream's allowlist does not narrow its instructions: they are the
server's, not a tool's. An upstream whose instructions the user does not
want in every session is removed or declined; there is no switch per
upstream. A pool's upstream reaches an agent only after the member
accepted it (addendum 2026-10-28, point 3), as its tool descriptions do.

## Enforced by

`crates/devstack-hub/src/secrets.rs` (the store, with its test against
the machine's real keychain on every platform). To be built:
`crates/devstack-hub` (upstream
connections, OAuth client and redirect, a loopback API for
`devstack mcp`), `crates/devstack/src/mcp.rs` (merging the tool lists,
forwarding calls, starting the hub), `ui/` (upstreams and logins),
`docs/dashboard-api.md`.

*Paths as built, noted 2026-10-27* (#325, #329, #332 and the stdio upstreams):
`crates/devstack-hub/src/secrets.rs` (the keychain, a file without a
Secret Service); `upstreams.rs` (`upstreams.toml`, the OAuth flow written
against `ureq` rather than the `oauth2` crate, `tools()` and `call()` with
prefix, allowlist and the tools last listed kept in `upstream-tools.json`,
`<prefix>__login` for an upstream never listed); `mcpclient.rs`
(Streamable HTTP); `stdio.rs` (a command started with a base environment
and the variables the user set, a kept token in `token_var`, its stderr in
`upstreams/<name>.log`); the routes `/hub/upstreams...` and
`/oauth/upstream/callback` in `lib.rs`; `crates/devstack/src/upstream.rs`
and `mcp.rs` (`devstack mcp` starts the hub only when upstreams are
configured, and looks for a change of their tools once a minute); the page
*MCP upstreams* (`ui/src/views/UpstreamsView.vue`). One user token for
every agent, as the owner chose on 2026-10-27; a token per agent was not
built.

*Option B, noted 2026-10-28:* `crates/devstack-node/src/upstreams.rs` (the
entry and its check, `upstreams.json`), the coordinator's `/v1/upstreams`
and `/v1/admin/upstreams` (`coordinator.rs`), `member.rs`;
`crates/devstack-hub/src/upstreams.rs` (`refresh_pools`, `pooled`,
`accept`, `decline`; the answers as `[[pool]]` in `upstreams.toml`),
`pool.rs` and `lib.rs` (the routes); `crates/devstack/src/poolcmd.rs`
(`devstack pool upstreams`); `ui/src/views/UpstreamsView.vue` and
`ui/src/components/PoolUpstreams.vue`; tests in
`crates/devstack-node/tests/upstreams.rs` and the hub's `upstreams.rs`.

*The instructions, noted 2026-11-04*:
`crates/devstack-hub/src/mcpclient.rs` and `stdio.rs` (the `instructions`
of `initialize`), `upstreams.rs` (`note`, `warm`, `instructions`,
`upstream-instructions.json`, `INSTRUCTIONS_MAX`), the route
`/hub/upstreams/instructions` in `lib.rs`; `crates/devstack/src/upstream.rs`
(`instructions`) and `mcp.rs` (`instructions`, the quoting and the cap in
all); tests in the hub's `upstreams.rs` and `stdio.rs`, `mcp.rs`, and
`crates/devstack/tests/hub.rs`.

## Addendum 2026-11-06: a pool's upstream an agent has set up itself

A member had a pool's upstream set up in Claude Code directly, by the
service's own setup hint, and had never answered the pool's offer. The hub
did not notice: the agent had the tools, a hook bound to the upstream
(ADR-0094) stayed silent for the whole session, and accepting it as well
would have given the agent the tools twice (#473).

The owner approved these points of the card in the conversation on
**2026-11-06** (points 4 and 6 of the pull request's card; point 5 is in
ADR-0094's addendum of the same day):

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 4 | trust | The dispatcher and the hub read the MCP servers configured directly in the agents (Claude Code `.claude.json`, global and per folder, and a working tree's `.mcp.json`; Codex `config.toml`), read only, names and URLs only; nothing leaves the machine, nothing is written. If wrong: nothing gained by an attacker; they are the user's own files and no token is read. | Asking the agent's CLI (`claude mcp list`: slow, starts the servers). | Low. |
| 6 | term | The *MCP upstreams* page shows on a pool's upstream that it is already set up directly in an agent (scope, entry), with the command that removes it; `devstack doctor` warns with the same; the hub never changes an agent's MCP configuration. | A button that accepts and removes the own entry (the hub writing `.claude.json`). | Low: a button can come later. |

As built: `crates/devstack-hub/src/agentmcp.rs` reads Claude Code's
`.claude.json` (in `CLAUDE_CONFIG_DIR`, else the home folder: `mcpServers`,
user scope; `projects.<folder>.mcpServers`, local scope), the `.mcp.json` of
each registered stack's repository (project scope), and Codex's
`config.toml` (`CODEX_HOME`, else `~/.codex`: `[mcp_servers.<name>]` with a
`url`). It keeps a server's name and URL only, never its headers, and skips
one without an http(s) URL. URLs compare with scheme and host in lower case,
the default port, user, query and fragment dropped, and no trailing `/`.
`GET /hub/upstreams` carries the matches as `pool.direct`; the page shows
each under the pool's entry, with advice by whether it is accepted (the
tools twice) or not (use it here, then remove the own entry); `devstack
doctor` warns once per match. The command for Claude Code is `claude mcp
remove --scope <user|local|project> <name>` (in the folder, for local and
project); for Codex, the `[mcp_servers.<name>]` table to delete from the
file.

Decided while building: the page reads the agents' files when it lists the
upstreams, and only when a pool offers any; Claude Code's local scope counts
for a working tree in that folder or below it; Codex has a user scope only.

## Addendum 2026-11-10: a filter on a tool's result

Agents read tickets through upstreams, and a ticket's description often
holds the personal data of the people it is about. What a tool returns is
in the model's context; the only place to redact it **before** an agent
reads it is between the upstream and the agent, in the hub (#502). veilq
0.1.0 has the piece that decides (`veilq mark`, its ADR-0024): it reads a
body, redacts it unless its first line is a matching marker, and returns
the marked body to store back. The context above left rewriting content
out of scope; this addendum takes that back for filters.

The owner approved these points of the card in the conversation on
**2026-11-10** (all six):

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | trust | The hub may replace part of an upstream tool's result with the output of a local command before `devstack mcp` gets it. If wrong: a filter misconfigured to pass content through looks like protection. | Asking the agent to run veilq itself (it has read the content by then) | Medium: an addendum; filters users configured |
| 2 | trust | Filters are configured per user only, on the user's upstream or an accepted pool upstream in `upstreams.toml` and the *MCP upstreams* page, never by a pool or a stack file: a filter runs a command, and a pool runs no command on a member's machine (ADR-0094). | A pool handing out filters | Low: a pool could offer one for acceptance later |
| 3 | contract | A filter names the tool, one or more paths into the result's JSON (the text content parsed as JSON, and `structuredContent` alike; `[*]` steps through arrays, e.g. `issues[*].description`) and the command. The command gets the field's string on stdin and answers JSON with `text` (required) and `write` (optional); exit 0 or 1 count as an answer. | A veilq-specific switch | Medium: the format has users |
| 4 | contract | Optional write-back: a tool of the same upstream and its arguments, built from the call's arguments and the command's `write` (`save_issue`, `{id: "$args.id", description: "$write"}`), only for a path without `[*]`; the hub calls it with the user's login. | Writing back from list results (needs each item's id) | Low: additive |
| 5 | concept | Fail closed: the command missing, failing (another exit code, a timeout), or answering without `text`, or a result that is not JSON where a filter applies, turns the tool call into an error; the raw content is never returned. A path that is absent (the agent did not ask for the field) means nothing to filter. A failed write-back still returns the filtered text and is logged. | Passing the raw result on an error | Low |
| 6 | concept | Images behind URLs and keeping an agent away from other connectors to the tracker are out of scope; comments can be filtered by the same format, only not written back. | Doing everything at once | Low: additive |

As built: `crates/devstack-hub/src/filters.rs` (the filter, its check, the
paths, running the command, applying it to a `CallToolResult`);
`upstreams.rs` keeps them as `[[filter]]` in `upstreams.toml` beside the
upstreams and runs them in `call()`, so they hold for every harness behind
`devstack mcp` and for `devstack mcp` without a hub (`DEVSTACK_NO_HUB`),
which calls the upstreams through the same code. The routes
`GET`/`PUT /hub/upstreams/filters` and `POST /hub/upstreams/filters/test`
in `lib.rs`; the section *Filters on results* on the *MCP upstreams* page
(`ui/src/components/UpstreamFilters.vue`, `ui/src/lib/filters.ts`), with a
preset for veilq on Linear and a field to try a filter on a text.

Decided while building: a command gets 30 s per field and the hub's
environment (it is the user's own command, unlike an upstream that is one,
which gets a base); it runs as a call of its own process tree, ended on the
timeout. Every item of the result's content must be text holding JSON, or
the call fails: an image or a resource could carry the field where no path
reaches it. A field that is `null` is left alone; one that is an object or a
number where a text is expected fails the call. An upstream's error result
is handed on unfiltered: it says why the call failed, not what a ticket
holds. The same text in the content and in `structuredContent` runs the
command once. The write-back runs before the hub answers, once per call and
distinct `write`; a write-back whose `$args.<name>` the call did not have is
skipped. Its tool does not need to be in the upstream's allowlist: the user
named it in the filter. `upstreams/filters.log` notes each write-back and
each refusal with the command's exit and stderr, never the field. A filter
names an upstream that exists when it is saved; removing the user's own
upstream removes its filters. A file of filters that cannot be read fails
every call of the upstreams instead of passing results through.
