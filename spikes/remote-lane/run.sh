#!/usr/bin/env bash
# Spike M0 (ADR-0025, ADR-0029): this script plays a node's agent and runs a
# lane of the fixture stack the way a node will: the mirror on a volume
# without .git, the infrastructure as the agent's Compose project, devstackd
# from the static Linux build in the stack's toolchain image, without Docker,
# reading the agent's infrastructure file. Then it checks the environment and
# that an edit in the mirror builds. Needs Docker and Python; run
# build-linux.sh first. Results: README.md.
set -euo pipefail
export MSYS_NO_PATHCONV=1
ROOT="$(cd "$(dirname "$0")/../.." && (pwd -W 2>/dev/null || pwd))"
BIN="$ROOT/spikes/remote-lane/bin"   # the static Linux build (build-linux.sh)
FIX="$ROOT/examples/fixture-stack"
P=m0-lane
IMG=mcr.microsoft.com/dotnet/sdk:10.0
t0=$(date +%s)
say() { echo "[$(( $(date +%s) - t0 ))s] $*"; }

docker rm -f $P-lane >/dev/null 2>&1 || true
docker compose -p $P down -v >/dev/null 2>&1 || true
docker volume rm $P-mirror >/dev/null 2>&1 || true

# 1. The mirror: what the requester's snapshot rule sends (tracked and
#    untracked non-ignored files), on the node's own file system, no .git.
say "mirror"
docker volume create $P-mirror >/dev/null
docker run --rm -v $P-mirror:/m -v "$FIX:/h:ro" alpine sh -c "cp -a /h/. /m/ && rm -rf /m/.devstack /m/src/Api/bin /m/src/Api/obj && mkdir -p /m/.devstack"

# 2. The lane's infrastructure, by the agent, as its own Compose project.
say "infra up"
cd "$FIX"
docker compose -p $P -f deploy/compose.yml -f dev/compose.dev.yml --env-file dev/.env.defaults up -d --wait --no-deps db cache >/dev/null
cfg=$(docker compose -p $P -f deploy/compose.yml -f dev/compose.dev.yml --env-file dev/.env.defaults --profile '*' config --format json)
ps=$(docker compose -p $P -f deploy/compose.yml -f dev/compose.dev.yml --env-file dev/.env.defaults ps --format json)

# 3. The file the daemon reads instead of Docker: environments, addresses on
#    the Compose network (service name, container port), states.
say "infra file"
python - "$cfg" "$ps" <<'PY' | docker run --rm -i -v $P-mirror:/m alpine sh -c "cat > /m/.devstack/external-infra.json"
import json, sys
cfg = json.loads(sys.argv[1]); ps = [json.loads(l) for l in sys.argv[2].splitlines() if l.strip().startswith('{')]
state = {c['Service']: (c['State'], c.get('Health', '')) for c in ps}
svcs = {}
for name, s in cfg['services'].items():
    svcs[name] = {
        'environment': s.get('environment') or {},
        'ports': {str(p['target']): f"{name}:{p['target']}" for p in s.get('ports') or []},
        'state': state.get(name, ('', ''))[0],
        'health': state.get(name, ('', ''))[1],
    }
print(json.dumps({'groups': {'app': {'project': cfg.get('name', ''), 'services': svcs}}}, indent=1))
PY

# 4. The lane container: the stack's toolchain image, devstackd mounted read
#    only, the mirror as its source, on the lane's network, no Docker.
say "lane container"
docker run -d --name $P-lane --network ${P}_default \
  -v $P-mirror:/src -v "$BIN:/opt/devstack:ro" \
  -e DEVSTACK_EXTERNAL_INFRA=/src/.devstack/external-infra.json \
  -e DEVSTACK_NO_HUB=1 -e DEVSTACK_NO_UPDATE_CHECK=1 \
  -w /src $IMG /opt/devstack/devstack daemon -f /src/devstack.toml >/dev/null

api() { # METHOD PATH: the daemon's API from inside the container
  docker exec $P-lane sh -c "port=\$(sed -n 's/.*\"port\": *\([0-9]*\).*/\1/p' /src/.devstack/daemon.json); tok=\$(cat \$HOME/.local/state/devstack/token); curl -s -X $1 -H \"Authorization: Bearer \$tok\" -H \"Host: 127.0.0.1:\$port\" http://127.0.0.1:\$port$2"
}
for i in $(seq 1 60); do docker exec $P-lane test -f /src/.devstack/daemon.json 2>/dev/null && break; sleep 1; done
say "daemon up; POST /v1/up"
api POST /v1/up >/dev/null
for i in $(seq 1 300); do
  st=$(api GET /v1/stack | python -c "import json,sys; v=json.load(sys.stdin); print(next(s['status']['state'] for s in v['services'] if s['name']=='api'))" 2>/dev/null || echo "?")
  [ "$st" = healthy ] && break
  [ "$st" = failed ] && { api GET /v1/stack; exit 1; }
  sleep 2
done
say "api: $st"
get() { docker exec $P-lane curl -s "http://127.0.0.1:5080$1"; }
say "env ConnectionStrings__Db = $(get /env/ConnectionStrings__Db)"
say "env Cache__Url            = $(get /env/Cache__Url)"
say "env Jobs__Enabled         = $(get /env/Jobs__Enabled)"
say "db resolves in the lane:   $(docker exec $P-lane getent hosts db | awk '{print $1}')"
b1=$(get /info | python -c "import json,sys; print(json.load(sys.stdin)['build'])")
say "build $b1; edit Program.cs in the mirror"
docker exec $P-lane sh -c 'echo "// m0 edit" >> /src/src/Api/Program.cs'
for i in $(seq 1 120); do
  b2=$(get /info 2>/dev/null | python -c "import json,sys; print(json.load(sys.stdin)['build'])" 2>/dev/null || echo "$b1")
  [ "$b2" != "$b1" ] && break
  sleep 2
done
say "after the edit: build $b2"
docker stats --no-stream --format '{{.Name}} {{.MemUsage}}' $P-lane

docker rm -f $P-lane >/dev/null; docker compose -p $P down -v >/dev/null 2>&1; docker volume rm $P-mirror >/dev/null
