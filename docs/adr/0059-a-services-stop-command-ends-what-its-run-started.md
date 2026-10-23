# ADR-0059: A service's `stop` command ends what its `run` started outside its process tree

- **Status:** Proposed
- **Date:** 2026-10-23
- **Deciders:** project owner
- **Related:** [#245](https://github.com/nuetzliches/devstack/issues/245), [ADR-0009](0009-the-daemon-owns-every-process-tree.md), [ADR-0016](0016-a-daemon-restart-is-a-clean-restart.md), [ADR-0004](0004-infrastructure-is-delegated-to-compose.md), [ADR-0041](0041-a-profile-says-how-a-lane-runs-for-a-purpose.md)

## Context

A service developed in the repository whose runtime needs Linux tools (an
API with OCR libraries, say) cannot run natively on a Windows machine. It
can run as a service with `build = "docker build -t img ..."` and `run =
"docker run --rm -p {port}:8000 img"`, and so get `watch`, candidates and a
lane's own `{port}`, which a Compose container does not get.

ADR-0009 owns the `docker` CLI's process tree, not the container: that
lives in Docker's VM. Measured with devstack 0.20.0 on Windows 11 and
Docker Desktop (#245):

- the graceful stop reaches the container: `CTRL_BREAK` to the service's
  process group, which the `docker run` CLI forwards to the container as
  `SIGINT` (`--sig-proxy`, the default). With `docker run --rm --init alpine
  sleep 300` the stop took 0 s and the container was gone, for `devstack
  stop` and `devstack down` alike;
- without `--init`, `sleep` is the container's PID 1, which has no default
  action for `SIGINT` and ignores it. devstack waited the 20 s, killed the
  CLI, and the container kept running.

So a container whose process handles the signal ends with its service
today. What still leaves one behind is everything that ends the CLI
without the container: a process that ignores the signal (the kill after
the timeout), the CLI killed or crashed on its own, a start that failed its
health check, and a daemon that died (its job object kills the CLI, ADR-0009,
and nothing tells the container). The container left running holds memory
and CPU, and its published port: the next instance on the same port fails
with "port is already allocated", a failed start although the code is fine.
`docker rm -f` before `docker run` in `run` is no way out: with a fixed name
it removes the main instance while a candidate starts.

## Decision

1. **`[services.<name>] stop`** is a command line devstack runs whenever an
   instance of the service ends: a stop, a restart, a `down`, the old
   instance a candidate replaced, a start or candidate that failed or was
   cancelled, and an instance that ended on its own (its root exited, a
   job included). It runs **before** the graceful signal, so a process that
   ignores the signal does not cost the timeout, and the command decides how
   politely (`docker stop -t 10 ...` or `docker rm -f ...`).
2. **It addresses the instance that ends.** It is expanded when the
   instance starts, with the instance's placeholders, the same as `run`:
   `{port}` (the instance's own port, internal behind the proxy),
   `{artifact}`, `{repo}`, `{lane}`, `{lane_suffix}`, `{is_main}`,
   `{service.…}`, `{compose.…}` and `${VAR}`. It runs in the instance's
   working directory with its environment. A container named
   `api{lane_suffix}-{port}` in `run` is that name in `stop`, for the main
   instance, a candidate and every lane.
3. **It is bounded and never blocks the stop.** It may run 30 s; its output
   goes into the instance's log. A non-zero exit or the timeout is written
   there and reported as a diagnostic; the stop goes on as ADR-0009 says:
   graceful signal, wait, kill.
4. **A daemon that died still runs it.** The expanded command, its working
   directory and the instance's log are recorded with the instance's
   cleanup record in `state.json` (ADR-0016). The next start of that tree's
   supervisor (the daemon's start for the main tree, the lane's start for a
   lane) runs every recorded `stop` before anything starts, then forgets
   them. It runs with devstack's own environment, not the service's: the
   service's environment can hold secrets and is not written to disk.
5. **A profile may set it** (`[profiles.<name>.services.<service>] stop`),
   like `run`. `kind = "static"` has no process and no `stop`; a profile
   that makes a service static drops it. In hot-reload mode the service runs
   its `hot_reload` command instead of `run`, and `stop` does not apply.
6. **Format.** `stop` is new and optional; files without it are unchanged
   (ADR-0023). A file that uses it says `requires = "0.21.0"`, and `devstack
   check` warns when it does not.

## Alternatives considered

- **Documentation only (`--init`, a process that handles `SIGINT`).** It is
  documented as well, and is enough for a clean stop. It does not cover a
  CLI killed on its own, a failed start, the kill after the timeout or a
  dead daemon, which are exactly the cases that leave a port taken.
- **Run `stop` only after the graceful signal timed out.** A process that
  ignores the signal would cost every stop and every replacement 20 s, and
  the case of a CLI that already ended would not be covered.
- **devstack recognises `docker run`** and adds `--cidfile` or a label to
  remove the container itself. It rewrites the user's command line, knows
  one tool only, and makes devstack a container runtime beside Compose
  (ADR-0004).
- **A Compose service with a build that `watch` rebuilds.** That is the
  "image" variant ADR-0041 left for later: no candidate, no promotion, a
  different decision.

## Consequences

- Every end of an instance of a service with `stop` runs one more program,
  a `docker` call of about a second, also where the container is gone
  already (a `docker rm -f` of no container fails harmlessly, and says so in
  the log).
- After a daemon crash the `stop` runs without the service's environment:
  a `stop` that needs a variable only the service gets (not `PATH`,
  `DOCKER_HOST`) fails then, and says so in the log of the instance.
- A lane that is not started again after a crash keeps what its instances
  left until it is (`devstack lane prune` does not run `stop`).

## Enforced by

- `crates/devstack-config`: `Service::stop` and `ServiceProfile::stop`
  (`model.rs`, the profile merge), the checks and the `requires` warning
  (`validate.rs`, `STOP_SINCE`); `docs/stack-file.md`.
- `crates/devstack-daemon`: the expanded `stop` with each instance
  (`supervisor/lifecycle.rs`: `launch`; one place ends an instance, `end`),
  every site that ended an instance with `tree.stop`/`tree.kill` going
  through it (`lifecycle.rs`, `replace.rs`), the record in `state.json`
  (`state.rs`: `Persisted::stops`) and the run at a supervisor's start
  (`supervisor/mod.rs`).
- `skills/` (the stack-file skill) and `crates/devstack-skills`.

## What this ADR does not say

- That devstack runs containers as services. A `docker run` service is a
  native process that happens to start one; Compose stays the infra plane
  (ADR-0004), and a lane on a node, which has no Docker, cannot run it.
- That `stop` replaces the graceful signal. Both happen, in that order.
