# ADR-0023: Stack files stay readable; migrations once devstack has outside users

- **Status:** Accepted
- **Date:** 2026-10-15
- **Deciders:** project owner
- **Related:** [ADR-0017](0017-stack-file-is-toml.md), plan §4

## Context

A stack file lives in a repository and outlives the devstack version that
wrote it. A newer devstack must read it, or say exactly what to change. An
older devstack must not misread a newer file.

What exists: `version = 1` (the schema version, checked exactly), `[stack]
requires` (the oldest devstack that understands the file; an older one asks to
be updated before it parses anything), and strict parsing (an unknown key is an
error). Every change so far was additive: new optional keys whose default keeps
the old behaviour.

While only the maintainers use devstack, a migration tool for a format nobody
else has would be work without a user. Once others use it, a release that stops
reading their files is a defect.

## Decision

**Two phases, switched by the project owner.**

1. **Until devstack has users outside its maintainers** (now): the format may
   change without a migration. A change that breaks existing files raises
   `[stack] requires` in the files the maintainers own and is named in the
   release notes. **Before such a change, ask the project owner whether
   devstack has outside users by now** (AGENTS.md); the answer decides the
   phase.
2. **Once it has**, these rules hold:
   - Within `version = 1`, changes are **additive only**: a new key is optional
     and its default keeps the old behaviour. A changed default counts as a
     break.
   - A **renamed or removed key** stays readable for at least two minor
     releases: the model accepts it (`#[serde(alias)]` or an unused optional
     field) and `compat::DEPRECATED` lists it, so `devstack check` and
     `devstack doctor` warn with its replacement.
   - A **break** is `version = 2`: devstack reads both versions for a
     transition period, and `devstack migrate` rewrites a file with
     `toml_edit`, keeping comments and order (ADR-0017).

**Always, in both phases:** `crates/devstack-config/tests/compat/` holds frozen
copies of stack files as released versions wrote them
(`v<schema>-<devstack>-<what>.toml`). A test loads every one; a red test means a
change breaks existing files, which is fine in phase 1 only on purpose (replace
the frozen copy and say so in the commit), and not at all in phase 2.

## Alternatives considered

- **Migrations from the start.** Correct, but a converter for a format only the
  maintainers use costs more than rewriting their two files by hand.
- **No version field, only `requires`.** `requires` says which devstack is too
  old, not which files a devstack can no longer read; a schema version is what
  a migration keys on.

## Enforced by

- `devstack-config`: `SCHEMA_VERSION` check, `check_requires`, strict parsing,
  `compat::DEPRECATED` and `compat::warnings` (empty table until phase 2 needs it).
- `tests/compat.rs` in `devstack-config`.
- AGENTS.md: ask the owner before a breaking change.
