# ADR-0042: A dev server's rebuilds show like builds, read from its output

- **Status:** Accepted (2026-10-20)
- **Date:** 2026-10-20
- **Deciders:** project owner
- **Related:** [ADR-0003](0003-changes-flow-through-candidates.md) (candidates), [ADR-0023](0023-stack-file-compatibility.md), [ADR-0037](0037-telemetry-goes-to-the-pools-collector-with-consent.md) (point 4: what is measured), plan §3.4a (dev servers)

## Context

A built service shows each change on its way: devstack watches the tree,
builds a candidate and switches to it, and the dashboard says "changed",
"building", "starting" or why it failed (ADR-0003). A dev server
(`kind = "devserver"`: `ng serve`, `vite`, `next dev`, `webpack serve`,
`dotnet watch`, `tsc --watch`) builds and serves in one process: it sees
the change itself and rebuilds. devstack starts it and keeps its output in
the service's log, and that is all it knows. A developer who saved a file
sees a built service's rebuild as a card on the service, a dev server's
only by reading its log; a failed rebuild of a dev server leaves the
service "healthy", because its process still answers.

Every such tool says on its output when it starts a rebuild, when it is
done (most with the time it took) and when it failed. Which tool runs is
not always in the stack file: `run = "npm run serve"` hides it behind a
package script.

## Decision

1. **devstack reads a dev server's output for its rebuilds.** While a dev
   server runs, its new log lines (ANSI colours removed) are matched
   against the known tools' lines. A rebuild has three states:
   `rebuilding` (since), `ok` (its time) and `failed` (its error lines, the
   first few after the failure marker, from the service's own output).
2. **The tool is recognised from the output itself** (`rebuild = "auto"`,
   the default): the first line that a known tool's marker matches fixes
   the tool for this run. Known: the Angular CLI (application builder and
   webpack), Vite (and what runs on it: Nuxt, SvelteKit, Astro), webpack's
   dev server, Next.js, `dotnet watch`, `tsc --watch`.
3. **The stack file can say more** (`[services.<name>] rebuild`, dev
   servers only): a known tool by name (`"angular"`, `"vite"`, `"webpack"`,
   `"next"`, `"dotnet-watch"`, `"tsc"`), `"none"` for no detection, or its
   own lines as regular expressions, `{ start = "…", ok = "…", failed =
   "…" }` (`ok` and `failed` required, `start` optional; a capture group
   named `ms` or `s` in `ok` gives the time). An additive key: an existing
   stack file loads and behaves as before (ADR-0023).
4. **The service shows it like a build.** The service's view carries
   `rebuild: {state, since, ms?, tool, errors?}`; the dashboard shows it
   where a built service shows its candidate: "Rebuilding…", "Rebuilt in
   11.9 s" (for a while, then it goes), "Rebuild failed" with the error
   lines and a link into the log. Nothing is switched or rolled back: the
   dev server keeps serving what it has.
5. **It is measured** (ADR-0037 point 4): each finished rebuild is a
   diagnostic line `devserver_rebuild` with `ms` (when known) and `result`
   (`ok`, `failed`), and so a `devstack.duration` sample by service.
6. A service in hot-reload mode (plan §3.4a: its toolchain's watcher from
   the source tree, `dotnet watch` say) rebuilds by itself as well: its
   output is read the same way (`auto`).
7. Wherever the dev server runs: the main tree, a lane on this machine, a
   lane on a node (its lane daemon reads its log there, and the lane's view
   carries it).

## Consequences

- A dev server's rebuild is as visible as a build's, and a failed one is
  no longer hidden behind "healthy".
- The markers are the tools' wording: a new release of a tool may change
  it. Each tool's lines are kept as test fixtures taken from real output;
  a stack can name its own lines when a tool says it differently.
- A dev server that prints nothing on a rebuild (some only reload the
  browser) shows nothing, as today.

## Alternatives considered

- **Recognise the tool from `run`.** A package script hides it (`npm run
  serve`); the output says it either way.
- **Have devstack build instead of the dev server.** That is what a
  `service` with `watch` is; a dev server is chosen for hot reload in the
  browser.
- **A plugin per tool (an Angular builder, a Vite plugin) that reports to
  devstack.** Precise, but a change to every project and every tool's
  version; the output is there already.

## Enforced by

`crates/devstack-daemon/src/rebuild.rs` (the markers, the fixtures),
`crates/devstack-config` (`rebuild`), the supervisor's follower of dev
servers' logs, `ServiceView.rebuild`, the service card in `ui/`.
