# ADR-0007: One HTTP API is the control surface; CLI, MCP and dashboard are its clients

- **Status:** Accepted (2026-10-14)
- **Date:** 2026-10-14
- **Deciders:** project owner
- **Related:** [plan §5](../plan.md#5-control-api-sketch)

## Context

In the reference stack people used a dashboard, scripts used a launcher with its
own process-killing logic, and agents used an MCP server whose telemetry tools
did not work in that configuration. Each path had slightly different semantics
(a dashboard "restart" did not rebuild; the launcher's stop did). Agents are a
primary user of devstack.

## Decision

The daemon exposes one versioned HTTP API (`/v1/...`) plus an SSE event stream.
The CLI, the MCP server (`devstack mcp`, stdio) and the dashboard are clients
of that API and implement no lifecycle logic of their own. Every MCP tool maps
to exactly one endpoint. The API binds to loopback, requires a per-user token
for mutating requests and checks `Origin`.

## Alternatives considered

- **CLI talks to the supervisor directly (no daemon).** Simpler, but nothing
  survives the terminal, and two CLIs race.
- **MCP as a separate implementation.** Repeats the semantic drift above.

## Consequences

- The daemon must be running for anything to work; `devstack up` starts it.
- An OpenAPI description becomes the contract for all three clients.

## Enforced by

- API module of the daemon (planned); CLI and MCP crates depend only on a
  generated or hand-written API client.

*Paths as built, noted 2026-10-18.* `crates/devstack-daemon/src/api.rs`
(`router`, every `/v1` route behind one `guard`); the hand-written client
`crates/devstack/src/client.rs`, which the CLI and the MCP server
(`crates/devstack/src/mcp.rs`) share; the dashboard reaches the same routes
through the hub (`/hub/stacks/{id}/api/...`, ADR-0021). There is no OpenAPI
description: the contract is [`docs/dashboard-api.md`](../dashboard-api.md).
Not every MCP tool is one endpoint: `up` also starts the daemon and Docker,
`doctor` and `diagnostics` read the machine and the daemon log, as the CLI
does.
