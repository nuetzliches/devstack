# devstack dashboard

The web dashboard of devstack, served by the hub on `http://127.0.0.1:4250`
([ADR-0021](../docs/adr/0021-a-hub-serves-the-dashboard.md)). It shows every
registered stack, whether each service's latest change is live yet, logs,
commands, resource usage and the event stream. The API contract is
[`docs/dashboard-api.md`](../docs/dashboard-api.md).

Vue 3, TypeScript, Vite, vue-router, Pinia, vite-plugin-pwa. No UI framework,
no icon font, no CDN; every dependency is pinned to an exact version.

## Run

```sh
npm ci
npm run dev        # against a running hub: Vite proxies /hub and /auth to 127.0.0.1:4250
npm run dev:mock   # no hub needed: an in-browser fake hub with three stacks
npm run build      # type check (vue-tsc) + production build into dist/ (embedded by the hub)
npm run build:mock # the same with the fake transport (VITE_MOCK=1 npm run build works too)
npm run preview    # serve dist/
npm run typecheck
```

With `npm run dev` the browser must be paired with the hub first: run
`devstack ui` once, then open the Vite URL (the session cookie belongs to the
hub's origin, so in practice pairing works best on the hub itself; the dev
server is mainly for working on the mock).

## Mock mode

`npm run dev:mock` swaps the real transport for `src/api/mock/`, which fakes the
hub and three daemons without any network:

- **shop** (running): api, worker, web (dev server), migrate (job), mailbox
  (tool), compose containers db, cache, queue and collector, commands `seed`,
  `reset-db` (asks for confirmation) and `lint`. A scripted timeline repeats:
  a change to `api` goes live (changed, building, starting, switching, new
  build), a `worker` build is superseded, an `api` build fails with C# compiler
  errors, the queue container restarts, then an `api` candidate is **held**
  because it changes schema inputs and waits for **Promote** (which runs the
  migrate job and switches).
- **crm** (running): an unhealthy service, a container that becomes healthy, a
  periodic candidate.
- **docs** (offline): shown from its last known state; **Start stack** brings it up.

Usage events arrive every 2 s and log lines stream for running services.
Switches for the edge screens:

| URL / console | Effect |
|---|---|
| `?mock=unpaired` | `GET /hub/session` answers 401 (the pairing screen) |
| `?mock=hubdown` | the hub is unreachable from the start |
| `devstackMock.hubDown(true / false)` | take the hub away and bring it back at runtime |

The mock is only bundled in mock builds; a normal build contains the real
transport alone.

## How it talks to the hub

- **No polling.** Live data comes only from Server-Sent Events: `/hub/events`
  (stacks), `/hub/stacks/{id}/api/v1/events` (snapshot, then deltas), log and
  run-log streams. A stream that fails reconnects with backoff (1 s doubling to
  15 s). One-off fetches happen only in reaction to something: `/v1/stack`
  after an `infra` event or a finished candidate (debounced 300 ms, for fields
  events do not carry, such as pid and ports), usage history once per
  connection, a command's runs when it is opened, the build log on request.
- **Offline.** The last stack list and the last snapshot of every stack are
  kept in `localStorage` (`devstack.*`). Without the hub the page shows a
  banner ("devstack hub is not reachable — run `devstack ui`") and the last
  known state, greyed; a stopped stack shows its last state with **Start
  stack**. The service worker precaches the app shell, so the page opens
  without the hub.
- **Actions** are same-origin `fetch` with the session cookie and JSON bodies.
  `409 {"error"}` opens a panel with the message rendered as compiler output;
  `428 {"confirm"}` asks with that text and resends with `confirm: true`.
- Routing uses the hash (`/#/s/<stack>/...`) so deep links need nothing from
  the hub beyond `/`.

## Structure

```
src/
  api/          types.ts (the contract), transport.ts (interface, paths),
                real.ts (fetch + EventSource), mock/ (fake hub: data, stack engine, timeline)
  stores/       hub (session, stacks), stack (selected stack: view, events, usage, runs, actions), ui (theme, toasts, dialogs)
  lib/          persist (last state), actions (409/428 handling), status (labels and tones), format, popover
  components/   TopBar, StackSwitcher, PendingIndicator, ServiceRow, ContainerRow, CommandRow,
                CandidatePanel, Diagnostics, LogView, Sparkline, Modal, GlobalDialogs, Toasts, icons/
  views/        Overview, ServiceDetail, Infra, Commands, Usage, Events, StackLayout, Home, Unpaired
  styles/       tokens.css (light/dark design tokens), base.css, components.css
```

Theme: follows `prefers-color-scheme`; the top-bar button cycles system, light
and dark (stored in `localStorage`).

## Conventions

- **Every table is sortable by its column headers.** Use `useSort`
  (`src/lib/sort.ts`) for the rows and `<SortTh>` for each header: a click
  sorts (numbers largest first, text A to Z), a second turns the order round,
  a third returns to the table's own order; the choice is remembered per
  table in the browser. A column of sparklines sorts by its peak.

- **Anything a button does belongs in the command palette** (`CommandPalette.vue`,
  Ctrl+K / Cmd+K) too, through the same store action.

## Test hooks

Key elements carry `data-testid` for Playwright: `stack-switcher`,
`stack-option`, `service-row` (with `data-service`, `data-state`),
`candidate-banner` and `candidate-line` (with `data-state`), `action-start`,
`action-stop`, `action-restart`, `action-promote`, `action-rollback`,
`action-build-log`, `action-stack-start`, `action-stack-stop`,
`action-start-stack`, `action-container-restart`, `log-view`, `log-filter`,
`log-pause`, `log-jump`, `log-marker`, `command-run`, `run-log`,
`confirm-dialog`, `confirm-accept`, `confirm-cancel`, `problem-dialog`,
`pending-indicator`, `connection-indicator`, `hub-unreachable`,
`stack-offline`, `unpaired`, `toast`.
