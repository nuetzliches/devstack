# ADR-0089: A folder scan filters by git and stack file, one level deep by default, and adds a repository without a stack file as not set up

- **Status:** Accepted (card in the pull request); built
- **Date:** 2026-11-03
- **Deciders:** project owner
- **Related:** [ADR-0071](0071-the-dashboard-writes-no-file-of-a-repository.md) (the dashboard writes no file of a repository; the setup is a prompt for a coding agent), [ADR-0055](0055-a-lane-without-a-stack-file-says-so-and-can-be-given-one.md) (a lane without a stack file), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

"Scan a folder of repositories" listed every folder with a `devstack.toml`
at its root, three levels below the folder chosen. A folder of test
fixtures (plain folders with a stack file, no git) showed up next to the
repositories and looked like worktrees to the owner. Three levels were
taken over from the first version of the scan without a decision; the
common layout is one level (`D:\Sources\<repo>`). And a repository without
a stack file could not be added from the scan at all, although the
dashboard knows a registered stack without one ("not set up", with the
coding agent's setup prompt on its page).

## Decision

The owner approved the points in the conversation on **2026-11-03**, all
five; while building, point 5 took the term the dashboard already had:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | concept | A stack can be registered without a `devstack.toml`; it is "not set up", is in the sidebar and offers the setup. | Adding a repository only after its file is written. | Medium: entries without a file would have to be removed or shown as errors. |
| 2 | concept | Once the file appears, the stack is an ordinary one without a further step. | Adding it again or reloading by hand. | Low. |
| 3 | contract | `POST /hub/stacks/scan` gets `git_only` (default `true`) and `configured_only` (default `true`); both `false` is a 400. | The fixed filter "has a `devstack.toml`"; plain folders were listed, by default they no longer are. | Low: optional fields. |
| 4 | contract | The scan's depth is one level by default; `depth` stays 1 to 6, the dashboard offers 1 or 3. | Three levels without a choice. | Low. |
| 5 | term | The state is "not set up" (the term `no_file` already had in the sidebar, the stack's header and banner), not "offline" or "error". | A second word for the same state ("not configured", as on the card). | Low. |

Decided while building, not on the card: the three checkboxes stand above
the list; a changed filter scans again; the one of the first two that would
leave neither on is disabled; the setup is the coding agent's prompt the
stack's page already shows (ADR-0071: the dashboard writes no template);
"One repository" keeps its setup prompt for a folder without a file and
does not register it.

As built:

1. **The scan** (`scan_for_stacks`, `ScanFilter`) lists a folder when it is
   a repository or `git_only` is off, and has a `devstack.toml` or
   `configured_only` is off. As before, a repository is looked at at its
   root only and not entered, a linked worktree (a `.git` file) is skipped,
   and the folders of `SCAN_SKIP` are never entered. Each result has
   `configured`; one without a file is named after its folder.
2. **Registering without a file.** `POST /hub/stacks/add` (and the scan's
   `add`) registers any folder; without a stack file the entry's name is the
   folder's. The registry and `StackEntry` are unchanged: `no_file` is what
   the sidebar shows as "not set up".
3. **The name follows the file.** The hub's once-a-second look at the
   registry remembers the stacks it saw without a file; when one's file
   appears and loads, the name it gives replaces the folder's in the
   registry. `devstack up` writes it too.
4. **The dashboard.** Above the scan's list: "Only git repositories" (on),
   "Only configured (`devstack.toml` exists)" (on), "Search subfolders
   (three levels)" (off). A folder without a file is tagged "not set up"
   and can be chosen like a new one.

## Consequences

- A scan of a folder that groups repositories (`D:\Sources\<customer>\<repo>`)
  finds nothing until "Search subfolders" is on.
- A stack the scan added without a file starts nothing until it is set up;
  its page says so and offers the prompt, its menu removes it.
- A stack whose file is removed again keeps the name the file gave.
