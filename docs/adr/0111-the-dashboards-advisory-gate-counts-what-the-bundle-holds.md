# ADR-0111: The dashboard's advisory gate counts what the bundle holds

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-17
- **Deciders:** project owner
- **Related:** [ADR-0006](0006-dashboard-vue-same-origin.md) (the dashboard), [ADR-0021](0021-a-hub-serves-the-dashboard.md) (the hub embeds it), `docs/releasing.md` ("No known advisory")

## Context

The release workflow refused a release whose dashboard dependencies had a
known advisory: `npm audit --omit=dev` in `ui/`, a warning in CI and
required in the release. On 2026-11-17 the tag `v0.49.0` failed there: an
advisory for picomatch (GHSA-fc5q-39xc-vjpf, every version, no fix yet)
reached the dashboard's "runtime" tree because vue-router 5 lists picomatch,
vite and unplugin as `dependencies` for its build plugin (file-based
routing). None of them has code in the bundle the hub embeds. npm cannot
tell the two apart (`--omit=peer` and `--omit=optional` change nothing), so
the gate refused what does not ship, with no way out but waiting for the
advisory's fix.

## Decision

The owner approved the review card in the conversation on **2026-11-17**:
points 2 to 4, and point 1 in a second form (1′) after the first proved
not to hold.

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1′ | trust | "Shipped" is a package with code in the built bundle: a Vite plugin writes the packages whose modules Rollup puts into the bundle to `ui/bundle-packages.json` (outside `dist`) at `npm run build`; the gate refuses an advisory only for a package on that list; no list fails the gate. | Counting `npm ls --omit=dev` (build plugins a package lists as a dependency count); a hand-kept list of exceptions | Low: plugin and script go |
| 2 | trust | It fails when it cannot tell: no readable answer from `npm audit` fails the check. | A script that lets through on a tool error | Low |
| 3 | contract | CI (a warning) and the release (required) run the same `ui/scripts/audit-shipped.mjs` (Node, Windows and Linux); `docs/releasing.md` names it as the local check before the tag. | Two rules that drift apart | Low |
| 4 | term | The tag `v0.49.0` stays and is not published; the same content is released as 0.49.1, its changelog section headed `[0.49.1]` with a line that 0.49.0 was tagged but not published; `FORWARD_SINCE` and the docs keep `0.49.0`, which 0.49.1 satisfies. | Deleting and re-setting `v0.49.0` (`releasing.md` rules it out) | — |

The first form of point 1 counted a package in `npm ls --omit=dev`. It was
approved on an analysis made without `node_modules` installed, where
`npm ls` lists nothing; installed, picomatch is in that tree through
vue-router. The check as first written would also have let every advisory
through when nothing was installed; it now refuses that (point 2).

As built:

1. **The list** (`ui/vite.config.ts`, plugin `devstack:bundle-packages`,
   build only): every chunk's module ids below `node_modules`, as package
   names (`@scope/name` kept whole); written at `closeBundle`, sorted.
   Two kinds count as shipped although Rollup does not show them: every
   installed `workbox-*` package (the service worker is bundled by workbox
   in a build of its own), and every declared `dependency` (`vue` ships as
   `@vue/*`, and an advisory names `vue`).
2. **The gate** (`ui/scripts/audit-shipped.mjs`): reads the list (none:
   exit 2), runs `npm audit --omit=dev --json` (no JSON or no
   `vulnerabilities`: exit 2), and fails (exit 1) for each entry whose
   advisory is its own (an object in `via`, not a "through" name) and whose
   package is on the list. Every other entry is printed as "not shipped".
3. **Where it runs:** the `ui` job of `ci.yml` (with `continue-on-error`)
   and of `release.yml`, after `npm ci && npm run build`.

Decided while building, not on the card:

- An entry npm marks only "through" another package does not count even
  when its own package ships (vue-router, through its build plugin's
  dependencies): the advisory is for the other package, which is checked
  on its own.

## Alternatives considered

- **A list of advisory ids to ignore,** each with a reason: has to be kept
  by hand, and an id stays ignored after the package starts to ship.
- **Waiting for the advisory's fix:** the release waits on a package that
  does not ship.

## Consequences

- An advisory for a build tool shows in the log of CI and the release, but
  no longer holds a release.
- The list is only as good as the build that wrote it: run the gate after
  `npm run build` of the same tree, as both workflows do.
