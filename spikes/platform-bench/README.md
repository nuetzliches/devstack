# Spike (d): platform benchmark

Answers [ADR-0011](../../docs/adr/0011-platform-windows-native-first.md): how much
faster, if at all, is a .NET + Angular development loop on a Windows Dev Drive
or inside WSL2, compared with plain NTFS?

## What is measured

| Step | Question |
|---|---|
| `fs-micro` | Raw many-small-files cost (write, stat, read, delete 20k files), no toolchain involved |
| `git-status` | What every agent and IDE does constantly |
| `dotnet-build-cold` / `-noop` / `-incremental` | API host build from clean, with nothing changed, after a one-line change in a leaf project |
| `snapshot` | Spike (c): full sync of the source into a lane, no-op sync, sync after 1 and 50 changed files (prototype of ADR-0002's sync) |
| `lane-build-*` | Build from the lane directly into a fresh artifact directory (`-o`): cold, no-op, incremental |
| `lane-build-inplace-*` | Build in the lane with its normal output directory: cold, no-op, incremental |
| `artifact-copy` / `-hardlink` | Publish the lane's build output as an artifact by copying or hard-linking |
| `dotnet-test-build` / `-run` | One unit-test project |
| `npm-install` | Dependency install with a warm package cache |
| `ng-build-cold` / `-warm-cache` | Production SPA build without and with the Angular build cache |
| `ng-serve-rebuild` | Dev server: time to first bundle (`ready`) and to each rebuild after a one-line change (`rebuilds`) |

Not measured here: save-to-promoted (needs devstack itself, phase 1), container bind mounts (the outcome is already known: no change
notifications from NTFS, see the problem statement).

## Setups

The harness clones the reference repository at a pinned commit into each root,
so the developer's working tree is never touched.

| Label | Root | Notes |
|---|---|---|
| `win-ntfs` | `C:\bench\ref` | NTFS, antivirus real-time scanning on |
| `win-refs` | `D:\bench\ref` | ReFS volume labelled as Dev Drive |
| `wsl-ext4` | `~/bench/ref` inside WSL2 | Source on the Linux file system, toolchains installed inside WSL |

The cross-boundary setup (source on NTFS, built from WSL via `/mnt/c`) is
deliberately left out: it is known to be the slowest and nobody would choose it.

## Running

Same Node, .NET SDK and npm versions on every side. Stop the dev stack and other
heavy processes first; record what was running if you cannot.

```bash
# Windows (Git Bash or PowerShell), from this directory
node bench.mjs --scenario scenarios/<name>.local.json --root D:/bench/ref --label win-refs \
  --repo D:/path/to/repo --ref <sha>

# WSL
node bench.mjs --scenario scenarios/<name>.local.json --root ~/bench/ref --label wsl-ext4 \
  --repo /mnt/d/path/to/repo --ref <sha>

# Compare
node report.mjs results/win-ntfs-*.json results/win-refs-*.json results/wsl-ext4-*.json
```

`scenarios/example.json` shows the format. Real scenarios are named
`*.local.json` and are git-ignored, as are `results/`, so nothing about a
downstream stack ends up in this repository. Only the aggregated numbers are
copied into ADR-0011.
