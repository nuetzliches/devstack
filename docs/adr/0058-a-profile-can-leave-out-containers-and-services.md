# ADR-0058: A profile can leave out containers, whole Compose groups and services

- **Status:** Accepted (2026-10-23)
- **Date:** 2026-10-23
- **Deciders:** project owner
- **Related:** [#244](https://github.com/nuetzliches/devstack/issues/244), [ADR-0004](0004-infrastructure-is-delegated-to-compose.md), [ADR-0023](0023-stack-file-compatibility.md), [ADR-0025](0025-a-lane-can-run-on-another-node.md), [ADR-0030](0030-each-lane-has-its-own-infrastructure.md), [ADR-0041](0041-a-profile-says-how-a-lane-runs-for-a-purpose.md), [ADR-0048](0048-a-containers-main-port-can-move-with-the-main-slot.md)

## Context

A downstream stack has two Compose groups: one with a single search
engine (about 2 GB of memory), one with about a dozen containers, among
them a virus scanner, a document converter, an address geocoder and a mock
AI service. Daily work on most branches needs none of these. The team wants
a light profile that leaves them out.

A profile's `autostart` (ADR-0041 point 1) takes `[services]` entries only.
A Compose group always starts every container in its `services` list, and
nothing in a profile or a group leaves one out. A Compose `profiles:` entry
in the dev overlay does not help either: devstack starts containers by name,
and Compose starts a container named on its command line even if it belongs
to a Compose profile. ADR-0041 meant a profile to choose which of a group's
services start ("What this ADR does not say"), but it never gave it a key
for that.

Three facts of the daemon shape the decision:

- A service that waits `after` a container that does not run starts at once
  (the container has no published port to check).
- A `{compose.<group>.<service>.<port>}` placeholder, or a `rewrite_hosts`
  entry, naming a container that does not run fails the service's start
  ("port … is not published").
- Containers outlive `devstack down` (ADR-0004). A container the last start
  ran keeps running, and keeps its memory, unless something stops it.

## Decision

1. **`[profiles.<name>] skip`** lists what does not start with the profile:
   - `compose.<group>.<service>`: a container of a group's `services`;
   - `compose.<group>`: every container of the group;
   - `<service>`: a `[services]` entry, as if its `autostart` were `false`.

   The list adds to `autostart`: `autostart` says which services start,
   `skip` takes things away. A name in both is an error.
2. **The tree's start leaves skipped containers out.** `docker compose up`
   gets the group's other services. A group with nothing left is not
   started at all, and the event says the profile left it out. A skipped
   container of this tree's project that still runs from an earlier start
   is stopped (`docker compose stop`), so the profile frees what it leaves
   out. Its port held for the main slot (ADR-0048) stays held and refuses
   connections while it is down.
3. **A skipped container stays part of the stack.** It is still in the
   group's `services`, so `devstack start compose.<group>.<service>` (the
   dashboard, MCP) starts it on demand, and `down --infra` removes it.
   `skip` only changes what starts with the tree.
4. **Checked with the profile (ADR-0041 point 3).** Errors: an entry that
   names no group, no container of a group's `services` or no service; a
   name in both `autostart` and `skip`. A service that starts with the
   profile and needs a skipped container's address is also an error, since
   its start would fail: a `{compose.<group>.<service>…}` placeholder in its
   `run`, `env`, `workers.env` or `ensure_dirs` (the profile's `env`
   included), or a `rewrite_hosts` entry. The fix is to skip that service
   too, or to keep the container. `after` on a skipped container is not an
   error: the service starts without waiting for it.
5. **A lane on a node** (ADR-0025): the node's agent applies the profile's
   `skip` to the groups it creates and starts. A skipped container is not
   created there, and a group with nothing left is not made at all. The
   lane's daemon does not wait for a group its profile leaves out.
6. **Format.** `skip` is new and optional, so existing files are unchanged
   (ADR-0023). A file that uses it raises `[stack] requires` to the release
   that brings it, and `devstack check` warns otherwise.

## Alternatives considered

- **`autostart` takes containers too.** It reads naturally, but today a
  profile's `autostart = ["api", "web"]` starts every container. Making
  it start only the containers it names would change existing profiles.
  Limiting that rule to the groups the list mentions would keep them, but
  then whether a container starts depends on whether another container of
  its group is named.
- **A per-container `autostart = false` in the group** (on demand, like a
  service). It is useful, but it is a property of the stack, not of a
  purpose: it leaves the container out of every start. It can come later as
  its own key. `skip` is the profile's side of the same idea.
- **Compose `profiles:` in the overlay.** Compose ignores them for services
  named on its command line, and devstack names every service it starts.
- **Drop a skipped container's placeholders, or start the service anyway.**
  A service would then run without an address it was written to need. The
  check says so instead.

## Consequences

- A light profile frees the memory of what it leaves out, also for
  containers an earlier start left running.
- A service that needs a skipped container's address cannot start with the
  profile: skip it too, or keep the container.
- On a node a skipped container is not created, so it cannot be started on
  demand during that run. Here it can.
- A `shared` container (ADR-0030) that the main tree's profile skips is not
  there for the lanes either: they use the main tree's.

## Enforced by

- `crates/devstack-config`: `Profile::skip`, `Stack::apply_profile` (the
  references, `autostart = false`), `Active::skip` and `Stack::skips`;
  `validate.rs` (what a starting service needs of a skipped container; the
  `requires` warning, `SKIP_SINCE`); `tests/profiles.rs`.
- `crates/devstack-compose`: `Docker::stop_selected`.
- `crates/devstack-daemon/src/supervisor/infra.rs`: `infra_up` leaves the
  skipped containers out and stops the ones that run; `node_infra_up` does
  not wait for a group that is left out completely.
- `crates/devstack-node/src/lanerun.rs`: the groups a lane on a node
  creates and starts.
- `docs/stack-file.md`, `skills/devstack-config/SKILL.md`.
