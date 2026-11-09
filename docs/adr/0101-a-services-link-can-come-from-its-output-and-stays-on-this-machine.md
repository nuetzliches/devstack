# ADR-0101: A service's link can come from its output; it belongs to the running instance and stays on this machine

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-09
- **Deciders:** project owner
- **Related:** [ADR-0023](0023-stack-file-compatibility.md) (a new key is optional), [ADR-0010](0010-promotion-mechanism.md) (instances behind the proxy), [ADR-0037](0037-telemetry-goes-to-the-pools-collector-with-consent.md) (what leaves through the diagnostic log), [ADR-0090](0090-a-service-can-get-a-public-link-by-hand.md) (public links), issue #496

## Context

A native service's dashboard link was `{scheme}://localhost:{port}`, fixed
from the stack file when the daemon started (`ServiceView.url`). A service
whose page needs a token or a path printed at start (Jupyter's
`?token=…`, an admin UI's one-time login link, a tool whose loopback API is
guarded by a token per start) got a link that failed with 401 or opened the
wrong page; the developer had to find the URL in `devstack logs`. A fixed
token in the stack file is no option where the token changes per start.
`url` is read beyond the dashboard: the public-link routers of the hub and
the node take TLS from it, out of the snapshot.

## Decision

The owner approved the review card in the conversation on **2026-11-09**,
all five points:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | contract | A native service takes the optional key `link = { from_log = '<regex>' }`: the first capture group of the first match in its output is the link; `devstack check` compiles it, `LINK_SINCE` with its warning. | A flat `url_from_log`; `url` (confused with `{service.X.url}`); `urls` for native services | A published key goes only as a deprecated alias (ADR-0023) |
| 2 | concept | The link belongs to an instance: read from that instance's log file, taken over with pid and log at `switch_to`; until the first match, and after a restart, the port link. | One link per service, last match wins: a candidate's link would replace main's before its promotion | Medium: supervisor and promotion |
| 3 | trust | It lives in a field of its own, `ServiceView.link`, apart from `url`: public links (TLS, tunnel target), a lane shared with the pool, the journal and telemetry never see it. | Writing it into `url`: the token would travel in snapshots to the hub's and the node's routers | High once it has travelled: the token is out of hand |
| 4 | trust | The dashboard and `devstack status` (CLI and MCP) show it with its token; through MCP it reaches the agent's transcript, at its model's provider. | Masking the token in MCP: the agent could not open the service | Low: a mask can follow; a token given out is gone until the next start |
| 5 | contract | `devstack status` lists the links under the service table (`<name>  <link>`), for services with `link` only. | A `LINK` column (too wide); not in `status` | Low to medium: scripts read it |

As built:

1. **The key** (`crates/devstack-config`): `Service::link: Option<Link>`,
   `Link { from_log }`; `validate` compiles the expression, wants a capture
   group, refuses it for `kind = "static"` (no output of its own);
   `LINK_SINCE = "0.44.0"` and its warning in `validate::warnings`.
2. **Reading** (`supervisor/links.rs`, `follow_links`): a thread per
   supervisor reads the main instance's log (`view.log`) every 400 ms from
   its beginning until the first match whose group is an `http://` or
   `https://` address, then stops reading that file. A new log (a start, a
   switch) is a new instance: its link starts empty. `switch_to` and a
   start clear `view.link` at once, so a snapshot never pairs the new
   instance with the old link. A candidate's log is never read: its link
   shows once it is main.
3. **The port.** A link to the instance's internal port on a loopback host
   is moved to the service's `port` (the proxy, a dev server's forward);
   `0.0.0.0` and `[::]` become `localhost`. Another host or port, or a link
   with credentials, stays as printed.
4. **Where it goes.** `ServiceView.link` (absent until printed) and the
   event `service_link`, sent with `emit_quiet` and listed in `record`'s
   exceptions, so neither reaches the diagnostic log or the collector. The
   dashboard opens it from the service row and page and shows it without
   query and fragment (`linkLabel`); the event stays out of the timeline,
   and the stack view kept in the browser's storage leaves it out.
   `status_text` lists it under `LINK (printed by the service; it may hold
   a token)`.

Decided while building, not on the card:

- Native services only; Compose keeps its static `urls`.
- A service without a `port` gets a link in the dashboard once it printed
  one (`link` instead of `:port`).

## Alternatives considered

- **Reading every instance's log, the candidate's included,** and keeping
  its link beside it: more state for a link nobody can open before the
  promotion; the candidate's file is read whole once it is main.
- **A token placeholder in the stack file** (`{token}` devstack generates
  and hands to `run`): works only for tools that take a token from the
  command line, and the issue's examples do not all.

## Consequences

- Between a switch and the next round (at most 400 ms) the dashboard shows
  the port link.
- A link printed after more than 256 KiB of output per round is found a
  round later per chunk; a service that never prints a match keeps the
  port link, silently.
- The token is in the agent's transcript when the agent reads `status`
  over MCP (point 4).

## Enforced by

- `crates/devstack-config/src/model.rs` (`Link`), `validate.rs`
  (compile, capture group, static, warning), `lib.rs` (`LINK_SINCE`)
- `crates/devstack-daemon/src/supervisor/links.rs`, `replace.rs`
  (`switch_to`), `lifecycle.rs` (start), `mod.rs` (`record`), `state.rs`
  (`ServiceView::link`, `EventBody::ServiceLink`)
- `crates/devstack/src/client.rs` (`status_text`)
- `ui/src/lib/format.ts` (`serviceUrl`, `linkLabel`), `ui/src/stores/stack.ts`
  (event, storage), `ServiceRow.vue`, `ServiceDetailView.vue`
- `docs/stack-file.md`, `docs/dashboard-api.md`

## What this ADR does not say

- That `{service.X.url}` in commands or environments gives the printed
  link: it stays `{scheme}://127.0.0.1:{port}`.
- That a public link opens the printed link: it goes to the port, as
  before.
