# ADR-0044: A lane's prepared directories can be linked into its worktree

- **Status:** Proposed
- **Date:** 2026-10-21
- **Deciders:** project owner
- **Related:** [#159](https://github.com/nuetzliches/devstack/issues/159), [ADR-0002](0002-processes-run-from-immutable-artifacts.md), [ADR-0015](0015-devstack-is-optional.md), [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md) (a lane runs from a copy), plan §3.5

## Context

A lane runs from a copy of its worktree (ADR-0024), and `prepare` (`npm ci`)
runs in that copy. The worktree itself never gets a `node_modules`, by design.

Git does not know about the copy: `git commit` runs the repository's hooks in
the worktree. A pre-commit hook that lints staged files with `npx eslint`
fails in every lane worktree (`Cannot find module` from `eslint.config.js`),
although the lane has a prepared `node_modules` a few directories away. The
way out, `npm ci` by hand in the worktree, took three minutes and a second
copy of `node_modules` on a downstream Windows machine.

The hook cannot find the copy itself without depending on devstack's
internal layout (`.devstack/lane-<name>/lanes/main/src`), which is no
contract, and an application that depends on devstack breaks ADR-0015.

Measured on Windows (Git 2.43): `git worktree remove` follows a junction and
empties the directory it points to. A junction named `node_modules` is
ignored by a `node_modules/` rule; a symbolic link is not a directory to git
elsewhere, and a rule with a trailing slash does not ignore it.

## Decision

`prepare` takes `link`, directories relative to the repository
(`link = ["web/node_modules"]`). In a lane on this machine, after `prepare`
ran (or found its inputs unchanged), devstack links each of them in the
worktree to the same directory in the lane's copy: a junction on Windows,
which needs no privilege, a symbolic link elsewhere.

1. Only where the worktree has nothing of that name: a developer's own
   install, or a link they made, stays and is never touched.
2. A link git does not ignore is removed again at once, and the diagnostic
   log says how to ignore it (no trailing slash): `git add -A` would
   commit it otherwise.
3. When the lane stops (`lane stop`, its removal, a move to a node, the
   daemon stopping) devstack removes the links that point into its copy,
   and nothing else, so a `git worktree remove` after it cannot follow one.
4. A linked directory that is gone from the copy runs `prepare` again,
   even with unchanged inputs.
5. The main working tree runs in place with its own directories, and a
   lane on a node has no worktree there: neither links.

## Alternatives considered

- **The hook finds the copy.** Couples the downstream repository to a
  layout devstack may change, and makes it depend on devstack (ADR-0015).
- **`devstack lane path <name>`**, a stable command a hook opts into. A
  contract instead of a guess, but every repository's hooks have to be
  written for devstack, and a hook that runs without it has to handle both.
  The link serves the common case without the repository knowing.
- **`prepare` in the worktree too.** The second install the issue is about:
  minutes per lane, and a process of the lane writing inside a worktree that
  git or a tool may remove at any moment (ADR-0024).
- **Adding the link paths to `.git/info/exclude`.** Shared by every
  worktree of the repository, and a file of the developer's that devstack
  would edit. Checking with git and saying what to change keeps devstack
  out of it.

## Consequences

- A `git worktree remove` *while the lane runs* follows the junction (on
  Windows) and empties the copy's directory: the running dev server loses
  it, and on Windows the removal can fail on files the dev server holds.
  Point 4 repairs the copy at the next start. The opt-in `link` is what
  makes this possible: without it the worktree holds nothing of the lane.
  The docs say: `devstack lane stop` before removing the worktree.
- A daemon that is killed leaves its links; the next start of the lane
  finds them (point 1) and its stop removes them.
- On Linux and macOS a `node_modules/` rule does not ignore the link:
  such a repository needs `node_modules` (no slash) for links to be made.
- Not shown in `status` or the dashboard yet; the diagnostic log has
  `prepare_linked`, `prepare_not_linked` (with why) and `prepare_unlinked`.

## Enforced by

- `crates/devstack-config/src/model.rs` (`Prepare::link`),
  `crates/devstack-config/src/validate.rs` (inside the repository, literal).
- `crates/devstack-daemon/src/supervisor/copy.rs` (`link_prepared`,
  `unlink_prepared`, `prepare_job` for point 4), called from `copy_ready`,
  `artifacts.rs` (after a build's `prepare`), `handover.rs` (`close`) and
  `lanes.rs` (`down_all`).
- `crates/devstack/tests/daemon_lifecycle.rs` (`a_worktree_runs_as_a_lane`).
- `docs/stack-file.md` (`prepare`).

## What this ADR does not say

- That anything of the lane runs in its worktree: a link is a name in the
  worktree, the processes still run from the copy.
- That devstack links files, or paths `prepare` did not make: `link` names
  directories in the copy, and one missing there is reported, not created.
