#!/bin/sh
# Spike for #168: is this WSL2 distribution ready to run a devstack node
# agent that offers the machine's NVIDIA GPU to the pool?
#
#   sh scripts/spike/wsl-gpu-node/check.sh
#
# Run it inside the WSL distribution, before and after starting the agent.
# It changes nothing (it pulls two small images). Every line is OK, WARN or
# FAIL; the summary at the end is what goes back to the maintainers. It
# prints no addresses: they stay on this machine.
set -u

fails=0
warns=0
ok() { echo "OK    $*"; }
warn() { echo "WARN  $*"; warns=$((warns + 1)); }
fail() { echo "FAIL  $*"; fails=$((fails + 1)); }

# 1. WSL2, mirrored networking: other machines in the LAN reach port 4261 of
#    the distribution at the Windows machine's own address.
if grep -qi microsoft /proc/version 2>/dev/null; then
  ok "running in WSL"
else
  fail "not WSL: this check is for a WSL2 distribution on Windows"
fi
if command -v wslinfo >/dev/null 2>&1; then
  mode=$(wslinfo --networking-mode 2>/dev/null || echo unknown)
  if [ "$mode" = "mirrored" ]; then
    ok "networking mode: mirrored"
  else
    fail "networking mode: $mode (set networkingMode=mirrored in %UserProfile%\\.wslconfig, then wsl --shutdown)"
  fi
else
  warn "wslinfo missing: WSL too old to tell the networking mode (wsl --update)"
fi

# 2. The GPU, as WSL passes it through (the driver lives on Windows; WSL
#    mounts its nvidia-smi under /usr/lib/wsl/lib).
if command -v nvidia-smi >/dev/null 2>&1 || [ -x /usr/lib/wsl/lib/nvidia-smi ]; then
  smi=$(command -v nvidia-smi || echo /usr/lib/wsl/lib/nvidia-smi)
  gpus=$("$smi" --query-gpu=name,memory.total --format=csv,noheader 2>/dev/null)
  if [ -n "$gpus" ]; then
    echo "$gpus" | while IFS= read -r g; do ok "GPU: $g"; done
  else
    fail "nvidia-smi finds no GPU (Windows NVIDIA driver installed and current?)"
  fi
  case ":$PATH:" in
    *:/usr/lib/wsl/lib:*) ;;
    *) command -v nvidia-smi >/dev/null 2>&1 || warn "nvidia-smi is not on PATH; the agent reads GPUs through it (add /usr/lib/wsl/lib)" ;;
  esac
else
  fail "no nvidia-smi: no NVIDIA GPU visible in WSL"
fi

# 3. Docker: an engine of this distribution, not Docker Desktop's. The agent
#    sets iptables rules for the lanes on the engine's host, and resolves the
#    lanes' mounts in this file system.
if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  os=$(docker info --format '{{.OperatingSystem}}' 2>/dev/null)
  case "$os" in
    *"Docker Desktop"*) fail "docker talks to Docker Desktop: install Docker Engine in this distribution and turn off Desktop's WSL integration for it" ;;
    *) ok "Docker Engine: $os" ;;
  esac
  if docker info --format '{{json .Runtimes}}' 2>/dev/null | grep -q nvidia; then
    ok "nvidia runtime registered"
  else
    warn "no nvidia runtime in docker info (nvidia-ctk runtime configure --runtime=docker, restart docker)"
  fi
  if docker run --rm --gpus all nvidia/cuda:12.6.3-base-ubuntu24.04 nvidia-smi -L >/dev/null 2>&1; then
    ok "a container sees the GPU (docker run --gpus all)"
  else
    fail "a container does not see the GPU (NVIDIA Container Toolkit installed and configured?)"
  fi
  if docker run --rm --network host --cap-add NET_ADMIN alpine:3 \
    sh -c 'apk add -q --no-cache iptables >/dev/null && iptables -S DOCKER-USER' >/dev/null 2>&1; then
    ok "iptables usable on the engine's host (the agent's egress rules)"
  else
    fail "iptables not usable from a host-network container (the agent cannot set egress rules)"
  fi
else
  fail "docker not reachable (installed? the user in the docker group? the service running?)"
fi

# 4. devstack and the agent.
if command -v devstack >/dev/null 2>&1; then
  ok "$(devstack --version)"
else
  fail "devstack not installed (curl -fsSL https://github.com/nuetzliches/devstack/releases/latest/download/install.sh | sh)"
fi
if command -v ss >/dev/null 2>&1 && ss -ltn 2>/dev/null | grep -q ':4261 '; then
  ok "something listens on 4261 (the agent, once started)"
else
  warn "nothing listens on 4261 yet (expected before the agent runs)"
fi

echo
echo "summary: $fails FAIL, $warns WARN"
[ "$fails" -eq 0 ]
