# Spike (a): process ownership

Answers plan §7.1 for [ADR-0009](../../docs/adr/0009-the-daemon-owns-every-process-tree.md)
and [ADR-0016](../../docs/adr/0016-a-daemon-restart-is-a-clean-restart.md): can a
supervisor own whole process trees on Windows, stop them gracefully one service
at a time, and guarantee that nothing survives its own death?

## Pieces

| Path | What |
|---|---|
| `src/win.rs` | One Job Object per service (`KILL_ON_JOB_CLOSE`, no breakaway, non-inheritable handle), two ways to start and stop a service |
| `src/main.rs` | `procspike run`: start services, record job membership, hold, stop one by one, escalate to kill; OS file lock for single instance |
| `fixtures/dotnet-host` | Generic host with a background worker; writes `dotnet.stopped` only on a regular shutdown |
| `fixtures/node-app` | `npm run start` → `node server.js` → grandchild; roles `stubborn` (ignores signals) and `detached` (spawns a detached child) |
| `run-tests.mjs` | Scenarios, checked from outside: markers, process liveness, exit codes |

```bash
cargo build --release
dotnet build fixtures/dotnet-host
node run-tests.mjs            # all scenarios; or name some: node run-tests.mjs a4 a8
```

## Two ways to stop a service gracefully

| Method | How | Requirement |
|---|---|---|
| `break` | Service started with `CREATE_NEW_PROCESS_GROUP` on the supervisor's console; `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, group)` | The supervisor has a console (its own hidden one when it runs detached) |
| `conpty` | One pseudo console per service; Ctrl+C written into its input | ConPTY's cursor-position query (`ESC[6n`) must be answered, or the console blocks at start |

## Results (Windows 11, 2026-10-14)

| # | Scenario | Result |
|---|---|---|
| a1 | Trees recorded per service from job membership (`dotnet run` → host: 3 processes; `npm` → `node` → grandchild: 5) | pass |
| a2 | `break`: .NET host and node tree incl. grandchild stop gracefully (`StopAsync` ran, `SIGBREAK` received), 100 ms each; the signal reaches only its own service | pass |
| a2 | `conpty`: .NET host stops gracefully in 100 ms; **node never receives the Ctrl+C** (plain ETX and win32-input-mode sequence tried) | **fail, open** |
| a2b | `break`, supervisor started detached **without** a console | fails as expected: `GenerateConsoleCtrlEvent` → invalid handle, and every service opens a console of its own |
| a2b | same, supervisor allocates a **hidden console** at start | pass, 100 ms |
| a3 | Service ignoring every signal: killed after the timeout, whole tree gone | pass (3.0 s with a 3 s timeout) |
| a4 | Supervisor killed hard (`taskkill /F`): all 12 recorded processes gone, including a detached, unref'd grandchild | pass |
| a5 | Negative control: job handle made inheritable → all 5 processes **survive** the supervisor | confirms the condition |
| a6 | Child requests `CREATE_BREAKAWAY_FROM_JOB` → refused (access denied) | pass |
| a6 | Negative control: breakaway allowed → child escapes and survives | confirms the condition |
| a7 | `dotnet watch` (creates its own jobs) inside ours, then hard kill → nothing survives | pass |
| a8 | Second instance on the same state directory refused (exit 3); after a hard kill of the first, the lock is free immediately | pass |

## Findings for the daemon

1. **Stop method: `break`, with a hidden console of the daemon's own.** It works for
   .NET and node trees alike, needs no terminal emulation, and keeps services
   independent. ConPTY stays an option for colourful output capture, but not
   for stopping until the node case is understood.
2. **`cmd /s /c` needs the whole command line wrapped in one more pair of
   quotes**, or a command that starts with a quoted path is mangled.
3. **Create suspended, assign, resume.** std's `Child` exposes no main-thread
   handle, so the spike resumes by enumerating threads; the daemon can call
   `CreateProcessW` itself, or start processes directly in the job
   (`PROC_THREAD_ATTRIBUTE_JOB_LIST`), and also restrict inherited handles
   (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`).
4. **Exact membership beats tree walks.** `JobObjectBasicProcessIdList` lists
   what belongs to a service, including processes whose parent already exited.

## Not covered yet

- **Linux half** (process groups, cgroup v2, restart cleanup by recorded group
  and start time, a8 on Linux). Needs a Linux host or container; the WSL
  distribution used for spike (d) was removed after ADR-0011.
- ConPTY with node (see above).
