#!/bin/sh
# Install or update devstack on Linux (x86_64, static build).
#
#   curl -fsSL https://github.com/nuetzliches/devstack/releases/latest/download/install.sh | sh
#   sh install.sh [--version 0.1.0] [--dir ~/.local/bin]
#
# Uses `gh` when it is installed and logged in (also for a private fork),
# otherwise downloads anonymously from GitHub. Checks the archive against the
# release's SHA256SUMS. Replacing the binary under a running daemon is fine on
# Linux; restart the daemon (`devstack down`, `devstack up`) to use the new one.
set -eu

repo=nuetzliches/devstack
version=""
dir="${HOME}/.local/bin"
while [ $# -gt 0 ]; do
  case "$1" in
    --version) version="${2#v}"; shift 2 ;;
    --dir) dir="$2"; shift 2 ;;
    --repo) repo="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) asset=devstack-x86_64-unknown-linux-musl.tar.gz ;;
  *) echo "no devstack build for $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
  if [ -n "$version" ]; then
    gh release download "v$version" -R "$repo" -p "$asset" -p SHA256SUMS -D "$tmp"
  else
    gh release download -R "$repo" -p "$asset" -p SHA256SUMS -D "$tmp"
  fi
else
  if [ -n "$version" ]; then
    base="https://github.com/$repo/releases/download/v$version"
  else
    base="https://github.com/$repo/releases/latest/download"
  fi
  for f in "$asset" SHA256SUMS; do
    curl -fsSL -o "$tmp/$f" "$base/$f"
  done
fi

(cd "$tmp" && grep " \*\?$asset\$" SHA256SUMS | sha256sum -c -) >/dev/null \
  || { echo "checksum mismatch for $asset" >&2; exit 1; }

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$dir"
# Write next to the target and rename: atomic, and safe under a running daemon.
cp "$tmp/devstack" "$dir/.devstack.new"
chmod 755 "$dir/.devstack.new"
mv -f "$dir/.devstack.new" "$dir/devstack"

echo "Installed: $("$dir/devstack" --version) -> $dir/devstack"
# The dashboard hub starts at logon (XDG autostart) and the dashboard can start
# it (devstack:// link). DEVSTACK_NO_HUB_INTEGRATION=1 skips both.
if [ -z "${DEVSTACK_NO_HUB_INTEGRATION:-}" ]; then
  "$dir/devstack" hub install || echo "Note: could not register the hub with your session (devstack hub install)." >&2
fi
case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "Note: $dir is not on your PATH." ;;
esac
