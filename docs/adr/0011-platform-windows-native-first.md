# ADR-0011: Windows-native first; WSL is decided by measurement

- **Status:** Open
- **Date:** 2026-10-14
- **Related:** plan phase 0 spike (d)

## Context

The first downstream team develops on Windows 11: the repository lives on NTFS,
the IDE is a Windows application, some components are Windows-only (a desktop
helper using Windows APIs), browsers run on Windows. Docker Desktop already runs
a WSL2 virtual machine for all containers.

WSL2 is often faster for exactly this kind of workload. The reasons, and their
limits:

| Effect | Native Windows (NTFS) | Source inside WSL2 (ext4) |
|---|---|---|
| Many-small-file I/O (`dotnet build`, `npm`, `ng build`) | Slower; real-time antivirus scanning of build output is a major factor | Fast |
| File-change notifications in containers | Do **not** cross the NTFS → Linux bind-mount boundary | Work (inotify) |
| Bind-mount performance into containers | Slow (cross-VM file sharing) | Native |
| Windows IDEs (Visual Studio) | Native | Over `\\wsl$` (9P): slow, watchers unreliable; Rider/VS Code have remote modes |
| Windows-only processes | Run | Cannot run inside WSL |
| AI agents and tools running on Windows | Native paths | Cross the 9P boundary on every file access |
| Browser → service | `localhost` | `localhost` via WSL port forwarding (mirrored networking recommended) |

The worst configuration is a split one: source on NTFS built from inside WSL, or
the reverse. Every file access crosses the VM boundary.

There is also a native middle ground: a **Windows Dev Drive** (ReFS, antivirus
in performance mode) for source, snapshots and artifacts removes much of the
NTFS overhead without leaving Windows.

## Question

Does devstack target Windows-native operation, WSL operation, or both, and which
does it recommend to the first downstream stack?

## Options

- **A. Windows-native only.** Matches today's setup; leaves performance on the
  table.
- **B. Windows-native first, WSL supported.** The daemon runs where the source
  lives. Nothing in the design assumes either platform (paths, process
  control, watching, line endings). Windows-only services in a WSL setup would
  need a Windows-side helper, which is out of scope until someone needs it.
- **C. WSL only.** Fastest builds, but excludes Windows-only components and the
  Windows IDE workflow.

## What would decide it

Phase 0 spike (d), on the reference stack, same machine, three setups:
NTFS, NTFS on Dev Drive, WSL2 ext4. Measured:

1. Cold and incremental build of the API host.
2. Production build of the SPA and dev-server rebuild after a one-line change.
3. devstack snapshot sync for a one-file and a fifty-file change.
4. Time from save to promoted main.
5. Backend test suite runtime.

If Dev Drive closes most of the gap, B with a Dev Drive recommendation wins
without changing anyone's workflow.

## Enforced by

Regardless of the outcome: `AGENTS.md` requires every feature to work on native
Windows, and CI (planned) builds and tests on `windows-latest` and
`ubuntu-latest`.
