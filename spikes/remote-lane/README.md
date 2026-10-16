# Spike M0: a lane in a container on a node (ADR-0025, ADR-0029)

`run.sh` plays a node's agent for the fixture stack (`examples/fixture-stack`):

1. the mirror: the fixture's files on a Docker volume, without `.git` (what a
   requester's snapshot rule sends);
2. the lane's infrastructure: the agent's own Compose project (`m0-lane`);
3. the infrastructure file the daemon reads instead of Docker
   (`DEVSTACK_EXTERNAL_INFRA`): every Compose service's environment, its ports
   as `service:port` on the lane's network, its state;
4. the lane container: `mcr.microsoft.com/dotnet/sdk:10.0` (the stack's
   toolchain), the static Linux devstack mounted read only
   (`build-linux.sh`), the mirror as its source, on the lane's network, no
   Docker socket; `devstack daemon` in it, then `POST /v1/up`.

## Result (2026-10-16, Windows 11, Docker Desktop, Compose 5.6)

| Step | Time |
|---|---:|
| infrastructure up (`db`, `cache`) | 2-4 s |
| daemon in the lane container ready | 1 s |
| `api` built (restore, build) and healthy | 5 s |
| an edit in the mirror is a new build, running | 5 s |

- The environment keeps the Compose names (`Server=db:5432`,
  `redis://cache:6379`), which resolve on the lane's network: host rewriting
  (ADR-0014) maps to the addresses in the agent's file, here the same names.
- File events work on the volume: the daemon's watcher sees the edit, the
  candidate builds and takes over (ADR-0003), unchanged.
- The lane container used about 300 MiB (daemon, API, build server).
- The daemon needed two changes, both in this branch: reading the agent's
  file instead of Docker (`crates/devstack-daemon/src/external.rs`), and a
  snapshot of a directory without `.git` (every file but `.devstack/`).

## Findings for M2 and M3

- **The daemon in the container thinks it is a main tree** (`Jobs__Enabled =
  true`): a remote non-main lane must run without workers (ADR-0025 point 4).
  It needs a lane mode (`devstack daemon --lane <name>`).
- **No port offsets on the node**: every lane container has its own network
  namespace, so its services keep the stack's ports. The offset matters only
  for the forwards on the requester (ADR-0029 point 4).
- **Services and the daemon listen on 127.0.0.1 inside the container**: the
  agent cannot reach them from outside. M3 needs a forward inside the lane
  container (the agent's end of ADR-0029's connection).
- **The daemon's API checks `Host`** (ADR-0019): the agent sets it.
