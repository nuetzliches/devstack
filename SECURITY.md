# Security

## Reporting a vulnerability

Please report security problems privately through GitHub: **Security →
Report a vulnerability** in this repository. Do not open a public issue for
them. You get an answer within a week; a fix and its release are coordinated
with you before anything is published.

## What devstack exposes, and to whom

devstack runs on developer machines and controls processes there, so its
interfaces are deliberately narrow ([ADR-0019](docs/adr/0019-control-api-security.md),
[ADR-0021](docs/adr/0021-a-hub-serves-the-dashboard.md)).

| Interface | Bound to | Who may use it |
| --- | --- | --- |
| Daemon API (one per stack) | `127.0.0.1`, a random port | Holders of the per-user token (`devstack` CLI, MCP server, hub). `Host` header checked against loopback, no CORS |
| Hub (dashboard) | `127.0.0.1` / `::1`, port 4250 | A browser paired once through a one-time code (`devstack ui`): `HttpOnly`, `SameSite=Strict` cookie, the hub's own `Origin` required for every change, `Host` checked. The hub stores a hash of the cookie, never the cookie |
| `GET /hub/ping` | the hub | Anyone on the machine; it answers name and version only. Its CORS header names only the landing page's origin (`https://nuetzliches.github.io`, shared by every GitHub Pages site of the organisation, which therefore can read the version too) plus origins in `DEVSTACK_HUB_PING_ORIGINS` |
| `/hub/pick-folder`, `/hub/inspect`, `/hub/stacks/add`, `/hub/stacks/init` | the hub | The paired browser (cookie, own `Origin`): opens the system's folder dialog on this machine, inspects a folder the user chose or named, registers it, or writes a first `devstack.toml` there, never over an existing file |
| `devstack://hub/start` | the operating system | Any web page can open the link; the browser asks first. It can only start the hub, nothing else |
| MCP server (`devstack mcp`) | stdio of the agent that started it | That agent. Commands with a confirmation text run only with `confirm: true`, which an agent must not send without the user's consent |

The token lives in the user's profile (`%LOCALAPPDATA%\devstack\token`,
`$XDG_STATE_HOME/devstack/token`), readable by that user only. Anything running
as the same user can read it; devstack does not defend against code that
already runs under your account.

## Network access of devstack itself

devstack calls out only to ask GitHub for the latest release (the hub at start
and every 12 hours, `devstack doctor`, `devstack update`), and Docker pulls
images your Compose files name. `DEVSTACK_NO_UPDATE_CHECK=1` turns the release
check off.

## Out of scope

- Processes devstack starts run with your rights. A stack file is code: run
  `devstack up` only in repositories you trust, as you would `npm install`.
- Network access of those processes is not restricted (see the proposal
  [ADR-0022](docs/adr/0022-network-egress-sandbox.md)).
