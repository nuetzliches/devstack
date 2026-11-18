# Spike: a Windows machine's GPU as a pool node, through WSL2

Part of [#168](https://github.com/nuetzliches/devstack/issues/168) (a Windows
machine as a pool node). Not Docker-in-Docker yet: this spike runs the
released Linux node agent in a WSL2 distribution, with Docker Engine and the
NVIDIA Container Toolkit there, to answer the questions it can answer first:

- Does the agent see the GPU (`nvidia-smi` through WSL) and offer it?
- Does a lane that asks for a GPU land there, and does its container get it?
- Do other members reach the agent (mirrored networking, the Hyper-V firewall)?
- What does the pool show: the machine twice (its Windows hub and the agent)?

This branch is not merged. Nothing here needs a devstack build of its own:
the release is enough.

Who does what: the **machine's owner** (or their coding agent) does steps 1
to 5 on the Windows machine; a **pool admin** does step 6; the **maintainers**
do step 7 from their machines. The coordinator's address and fingerprint come
from the maintainers directly, not from this repository.

## 1. Windows: WSL2 with mirrored networking

Needs Windows 11 22H2 or newer and WSL 2.0 or newer (`wsl --version`; update
with `wsl --update`). In `%UserProfile%\.wslconfig` (create it if missing):

```ini
[wsl2]
networkingMode=mirrored
```

Then `wsl --shutdown` and open the distribution again. That ends every
distribution, Docker Desktop's too: start Docker Desktop again afterwards. Inside it,
`wslinfo --networking-mode` prints `mirrored`, and `ip -4 addr` shows the
Windows machine's own LAN address.

Let other machines reach the agent's port. With mirrored networking the
Hyper-V firewall filters what comes in for WSL. In an elevated PowerShell:

```powershell
New-NetFirewallHyperVRule -Name devstack-node -DisplayName "devstack node agent" `
  -Direction Inbound -VMCreatorId '{40E0AC32-46A5-438A-A0B2-2B479E8F2E90}' `
  -Protocol TCP -LocalPorts 4261
```

Remove it after the spike with `Remove-NetFirewallHyperVRule -Name devstack-node`.

## 2. WSL: Docker Engine and the NVIDIA Container Toolkit

Use a distribution of its own (Ubuntu 24.04: `wsl --install -d Ubuntu-24.04`).
If Docker Desktop is installed, turn off its WSL integration for this
distribution (Settings, Resources, WSL integration): the agent needs an engine
in this distribution, whose iptables and file system it shares.

```sh
curl -fsSL https://get.docker.com | sh
sudo usermod -aG docker "$USER"     # then close and reopen the shell (not needed as root)

# The NVIDIA driver comes from Windows; do not install one in WSL.
curl -fsSL https://nvidia.github.io/libnvidia-container/gpgkey \
  | sudo gpg --dearmor -o /usr/share/keyrings/nvidia-container-toolkit-keyring.gpg
curl -fsSL https://nvidia.github.io/libnvidia-container/stable/deb/nvidia-container-toolkit.list \
  | sed 's#deb https://#deb [signed-by=/usr/share/keyrings/nvidia-container-toolkit-keyring.gpg] https://#g' \
  | sudo tee /etc/apt/sources.list.d/nvidia-container-toolkit.list
sudo apt-get update && sudo apt-get install -y nvidia-container-toolkit
sudo nvidia-ctk runtime configure --runtime=docker
sudo systemctl restart docker
```

If `nvidia-smi` is not found, add `/usr/lib/wsl/lib` to `PATH`: the agent
reads the GPUs through it.

## 3. WSL: devstack

```sh
curl -fsSL https://github.com/nuetzliches/devstack/releases/latest/download/install.sh | sh
```

It installs to `~/.local/bin`, which a shell `wsl -d` opens (as root in
particular) may not have on `PATH`; then:

```sh
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc && . ~/.bashrc
devstack --version
```

Use the same version as the pool's coordinator (`--version` of `install.sh`
if they differ).

## 4. Check

From a clone of this branch (`git clone -b spike/168-wsl-gpu-node
https://github.com/nuetzliches/devstack.git`), inside WSL:

```sh
sh scripts/spike/wsl-gpu-node/check.sh
```

Every `FAIL` names what is missing. Fix those first.

## 5. WSL: join and run the agent

Join as a member of its own, with a name that tells it apart from the
machine's Windows side (here `<machine>-wsl`), and agree to telemetry so the
maintainers can follow the spike without access to the machine. Run it
inside the distribution (`wsl -d Ubuntu-24.04`), not in PowerShell: on
Windows, `join` finds the machine's existing membership and answers
"joined … as <its id>" at once, with no new request. Inside WSL it says
"asked to join as <a new id>" and waits for the approval.

```sh
devstack join https://<coordinator>:4260 --fingerprint <fp> --name <machine>-wsl --telemetry
```

Once a pool admin approved it and granted it as a node (step 6), start the
agent and leave the shell open (it runs in the foreground):

```sh
devstack node agent --listen 0.0.0.0:4261 --api https://<windows-lan-address>:4261 \
  --max-cpus 8 --max-memory-gib 8
```

Choose the caps for what the machine can spare, and below what WSL itself
may take: `memory=` in `.wslconfig` caps the whole distribution (with
`memory=12GB`, `--max-memory-gib 8` leaves room for the engine and the
agent). No `--dedicated`: the machine is someone's workplace. Run `check.sh` again: the last line now says the agent
listens.

## 6. Pool admin

```sh
devstack node members approve <machine>-wsl
devstack node members node <machine>-wsl on
```

## 7. Maintainers: a lane that needs the GPU

`devstack pool` lists the node with its GPU. A lane's stack file must be at
the root of its worktree (a stack in a subfolder is no lane), so copy the
two files of `gpu-smoke` into a repository of their own, give it a
worktree, and start the lane from there:

```sh
mkdir gpu-smoke && cp <this branch>/scripts/spike/wsl-gpu-node/gpu-smoke/* gpu-smoke/
cd gpu-smoke && git init -b main && git add . && git commit -m gpu-smoke
git worktree add -b smoke ../gpu-smoke-lane && cd ../gpu-smoke-lane
devstack up --on <pool>:<machine>-wsl
devstack logs smi                  # nvidia-smi's table: the container has the GPU
devstack lane stop
```

## What goes back

Into a comment on #168, without addresses, host or member names: the summary
of `check.sh`, what `devstack pool` showed for the node (GPU, capacity), whether
the lane started and saw the GPU, and what failed on the way.

## When done

On the Windows machine: stop the agent (Ctrl+C), remove the firewall rule,
and set `networkingMode` back if it was not wanted. A pool admin removes the
member (`devstack node members remove <machine>-wsl`).
