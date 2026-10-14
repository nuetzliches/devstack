# ADR-0011: Windows-native first; WSL is decided by measurement

- **Status:** Proposed (was Open; measured 2026-10-14, recommendation below awaits acceptance)
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

## Question (as recorded while Open)

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

## Measurements (2026-10-14, spike (d) and (c))

One machine (8 cores / 16 threads, 64 GB, two NVMe SSDs, Windows 11), the
reference stack at one pinned commit, cloned into each setup, three repetitions,
median in seconds. Docker stopped, system otherwise idle. Windows: antivirus
real-time scanning on; the ReFS volume carries a Dev Drive label, whether it is
registered as a Dev Drive with antivirus performance mode could not be verified
without elevation. WSL: 3.0.3, kernel 6.18, limited to 12 CPUs / 32 GB by the
user's `.wslconfig`.

| Step | NTFS | ReFS | WSL ext4 |
|---|---:|---:|---:|
| 20k small files: write / delete | 4.19 / 1.26 | 2.06 / 0.56 | 0.54 / 0.23 |
| `git status` | 0.08 | 0.07 | 0.05 |
| API build: cold / no-op / one-line change | 15.5 / 1.9 / 2.0 | 12.7 / 1.8 / 1.9 | 11.8 / 1.6 / 1.5 |
| Unit tests (140) | 1.5 | 1.5 | 1.0 |
| `npm install` (warm cache) | 41 | 89 | 11 |
| SPA production build: cold / warm cache | 28.4 / 20.0 | 30.7 / 19.8 | 23.7 / 16.5 |
| Dev server: ready / first rebuild / later rebuilds | 25.9 / 19.3 / 1.2 | 25.3 / 20.0 / 1.3 | 23.3 / 18.6 / 1.3 |
| Snapshot: full (7,140 files) / 50 changed | 3.4 / 0.21 | 2.4 / 0.24 | 1.0 / 0.07 |

**Stability.** Over the afternoon the WSL virtual machine went down four times,
three of them under build load in the development distribution (on WSL 2.2.4
and once more after updating to 3.0.3), never reproducibly. Each time it took
every container of the running Docker Desktop with it, since both share one VM.
Two of the crashes left files written shortly before as **empty files**: 38 in
the NuGet package cache, 22,448 in `node_modules`, causing later builds to fail
with misleading errors ("Root element is missing", a CLI that exits 0 without
output). No Windows run failed; one transient `EBUSY` file lock during a
SPA build on ReFS was seen once in a probe run, never in the measured runs.

## Reading

- **The inner loop is equal.** A one-line change costs 1.5–2.0 s to rebuild
  the API and 1.2–1.3 s to rebuild the SPA on all three setups. That is what a
  developer waits for many times an hour.
- **WSL wins where many files are written at once:** `npm install` 4–8×,
  cold builds 10–25 %, raw file operations 3–8×. These happen a few times a day.
- **ReFS is not uniformly faster than NTFS here:** better for .NET cold builds
  and raw writes, markedly worse for `npm install`. Without confirmed antivirus
  performance mode the Dev Drive result is inconclusive.
- **WSL as operated today is not reliable enough to hold source and build
  output:** a crash costs the containers *and* silently corrupts recently
  written files.

## Recommendation

Option **B**: devstack is Windows-native first and stays platform-neutral in
design, so WSL remains possible. For the first downstream stack: source, lanes
and artifacts on Windows; WSL only through Docker Desktop for containers. The
WSL path is reconsidered when its stability is shown over a longer period, and
the Dev Drive question when performance mode can be verified.

## Enforced by

Regardless of the outcome: `AGENTS.md` requires every feature to work on native
Windows, and CI (planned) builds and tests on `windows-latest` and
`ubuntu-latest`.
