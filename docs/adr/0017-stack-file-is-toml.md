# ADR-0017: The stack file is TOML, without profiles in version 1

- **Status:** Accepted (confirmed 2026-10-15 after the reference stack ran end to end; compatibility: [ADR-0023](0023-stack-file-compatibility.md))
- **Date:** 2026-10-14
- **Deciders:** project owner
- **Related:** plan §4, [ADR-0004](0004-infrastructure-is-delegated-to-compose.md), [ADR-0014](0014-native-services-take-their-configuration-from-compose.md), [ADR-0015](0015-devstack-is-optional.md)

## Context

The stack file is devstack's main published contract: everything a downstream
project writes, and the thing ADR-0015 requires to stay readable by humans when
devstack is not around. Its format is expensive to change once projects depend
on it.

What it has to express was derived from the reference stack's orchestrator
configuration (an inventory, kept private). Beyond services, ports and health
checks it needs:

- paths relative to the repository (storage roots, fixtures) and directories that
  must exist before a service starts,
- references between services and to published Compose ports (a mock's URL in
  the API's environment),
- commands: shell steps, HTTP calls, lifecycle steps, enabled only while a
  service is healthy, some asking for confirmation,
- services that exist on one platform only, services that start on demand,
- a mode switch for one subsystem (a local AI backend as mock, GPU or external).

Profiles (one stack, several environments) were planned. The reference project
is reducing to a single development environment, so no current user needs them.

## Decision

The stack file is **TOML**, `devstack.toml` in the repository root, with a
`version = 1` key. Unknown keys are errors.

**No profiles in version 1.** A future second environment is expressed as an
additional stack file layered over the first (`devstack -f devstack.toml -f
devstack.other.toml`), merged by key like Compose files. Version 1 avoids
constructs that would prevent that: services, commands and Compose groups are
keyed tables, never anonymous list entries.

**No conditionals.** Mode switches are expressed through Compose profiles and
values in the development env file, not through expressions in the stack file.
A service can be limited to platforms (`platforms = ["windows"]`).

**Interpolation is explicit and closed:** `{repo}`, `{artifact}`, `{port}`,
`{service.<name>.url}`, `{compose.<group>.<service>.<container port>}` (the
published host address), and `${VAR}` / `${VAR:-default}` for values from the
environment and the group's env files. An unknown placeholder is an error.

The schema is drafted in plan §4 and published as JSON Schema with the first
implementation, so editors (taplo, VS Code) validate the file.

## Alternatives considered

- **YAML.** Compose is YAML, so one language for both. Rejected for its
  indentation and implicit-typing traps and the weaker Rust tooling; the stack
  file is short compared with Compose files.
- **An own DSL** (a `Devstackfile`, as sibling projects have a `Croniqfile` and a
  `Hookaidofile`). Best error messages and consistent with those projects, but a
  parser, editor support and documentation to build before the first user.
  Remains the main candidate if TOML proves unreadable.
- **Profiles in version 1.** No user needs them now; designing them without one
  would guess the semantics.

## Consequences

- Deeply nested structures read poorly in TOML; the schema keeps nesting to two
  levels and uses inline tables only for short values.
- Comments and key order survive edits by tools only with a format-preserving
  library (`toml_edit`), which devstack must use if it ever writes the file.

## Revisit

This decision is provisional. It is reviewed, and either confirmed or
superseded, when the first downstream stack file is complete and devstack has
run it end to end (end of phase 1). Until then the format may change without a
migration path. After that, a change of format ships with a converter.

**Revisited 2026-10-15:** confirmed. The reference stack (two Compose groups,
eleven services, eight commands) is written in it and runs end to end; nothing
asked for another format. When a change needs a converter is decided by
[ADR-0023](0023-stack-file-compatibility.md).

## Enforced by

- Stack file loader (planned): strict deserialisation, `version` check.
- JSON Schema in `schema/` (planned), `devstack check`.
