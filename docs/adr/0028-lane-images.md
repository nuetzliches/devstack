# ADR-0028: A remote lane runs in a stack-provided toolchain image; the node supplies devstack

- **Status:** Proposed (local lanes are built, ADR-0024; this extends them)
- **Date:** 2026-10-15
- **Deciders:** project owner
- **Related:** [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md), [ADR-0002](0002-processes-run-from-immutable-artifacts.md), [ADR-0023](0023-stack-file-compatibility.md), [ADR-0025](0025-a-lane-can-run-on-another-node.md), [ADR-0026](0026-placement-by-the-requester-admission-by-the-node.md)

## Context

The lane container of ADR-0025 has to build and run the stack's services: it
needs their toolchains (.NET SDK, Node, …) and, to start quickly, their
dependency caches (NuGet packages, `node_modules`). Installing these on every
lane start costs minutes; a cold first build in a fresh snapshot costs about
five times a warm one (ADR-0002).

Toolchains belong to the project and change with it. devstack's own version
belongs to the node and the requester and must match their protocol.

## Decision

1. **The stack names a lane image** in the stack file (a new, optional key, for
   example `[lanes] image = "<registry>/<name>:<tag>"`, next to `port_step`
   of ADR-0024). The image is an
   ordinary Linux image with the stack's toolchains; it does not contain
   devstack. A stack without the key cannot place lanes remotely.
2. **The node supplies `devstackd`**: the agent mounts its own static Linux
   build of `devstackd` read-only into the lane container and starts it. The
   project image never has to be rebuilt for a devstack update, and the version
   inside a lane always matches the agent's.
3. **Layers are ordered for reuse**, and the docs and `devstack init` recommend
   that order: toolchains; dependency caches keyed by the lockfiles; optionally
   a warm build state of the main branch (`obj/`, framework caches), which the
   lane's snapshot then starts from.
4. **The project builds and publishes the image** (its CI, its registry); nodes
   pull it. A node that already holds its layers is preferred by placement
   (ADR-0026).
5. The new key is additive: files without it load and behave as before
   (ADR-0023).

## Alternatives considered

- **devstack installs toolchains itself** (a list of SDK versions in the stack
  file). One file less for the project, but devstack would become a package
  manager for every ecosystem.
- **A devstack base image the project derives from.** Couples every project
  image to a devstack version; an update of devstack would mean rebuilding
  every lane image.
- **Dev Container definitions (`devcontainer.json`)** as the image source.
  Reuses an existing format where a project has one; can be supported as a
  second way to name the image later, it does not change this decision.

## Consequences

- A project that wants remote lanes maintains a Dockerfile and publishes an
  image. `devstack init` can generate a first version.
- The static Linux `devstackd` must run on any common base image (musl, no
  dynamic dependencies).
- A stale warm-state layer only costs build time: the snapshot overwrites what
  changed, and the build id is still the hash of the inputs (ADR-0002).

## Enforced by

Will live in: `crates/devstack-config` (`[lanes] image`, with a frozen compat
file), `crates/devstack-node` (pull, mount of `devstackd`), `devstack
init` (Dockerfile template), the release workflow (static Linux build).
