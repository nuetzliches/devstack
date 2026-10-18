#!/bin/sh
# Install or update devstack on Linux (x86_64, static build).
#
#   curl -fsSL https://github.com/nuetzliches/devstack/releases/latest/download/install.sh | sh
#   sh install.sh [--version 0.1.0] [--dir ~/.local/bin]
#
# Uses `gh` when it is installed and logged in (also for a private fork),
# otherwise downloads anonymously from GitHub (HTTPS only, TLS 1.2 or newer).
#
# What is checked: the archive against the release's SHA256SUMS, i.e. that it
# is the archive the release lists, not damaged or swapped on the way. That
# does not prove who built it (both files come from the same release); with a
# logged-in `gh`, `gh attestation verify` also checks the build provenance:
# built by this repository's release workflow (releases from 0.10.0 on).
#
# Replacing the binary under a running daemon is fine on Linux; restart the
# daemon (`devstack down`, `devstack up`) to use the new one. A running hub is
# moved onto the new binary.
#
# DEVSTACK_NO_HUB_INTEGRATION=1: no hub at logon, no devstack:// link; the
# choice is kept (`devstack up` does not register them later either).
set -eu

# Everything happens in main, called on the last line: a download cut off
# half-way runs nothing.
main() {
  repo=nuetzliches/devstack
  version=""
  dir="${HOME}/.local/bin"
  first_attested=0.10.0
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

  have_gh=""
  if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
    have_gh=1
  fi

  # One release for both files: `latest` resolved to its tag first, so a
  # release published in between cannot pair an archive with other sums.
  if [ -z "$version" ]; then
    if [ -n "$have_gh" ]; then
      version=$(gh release view -R "$repo" --json tagName --jq .tagName)
    else
      version=$(fetch_head "https://github.com/$repo/releases/latest" |
        sed -n 's|.*/releases/tag/\([^/?#]*\).*|\1|p')
    fi
    version="${version#v}"
    [ -n "$version" ] || { echo "could not find the latest release of $repo" >&2; exit 1; }
  fi

  if [ -n "$have_gh" ]; then
    gh release download "v$version" -R "$repo" -p "$asset" -p SHA256SUMS -D "$tmp"
  else
    base="https://github.com/$repo/releases/download/v$version"
    for f in "$asset" SHA256SUMS; do
      fetch "$base/$f" "$tmp/$f"
    done
  fi

  (cd "$tmp" && grep " \*\?$asset\$" SHA256SUMS | sha256sum -c -) >/dev/null \
    || { echo "checksum mismatch for $asset" >&2; exit 1; }
  echo "SHA-256 matches (the archive is the one release v$version lists)"

  if [ -z "$have_gh" ]; then
    echo "Note: build provenance not checked (needs a logged-in gh: https://cli.github.com)."
  elif older "$version" "$first_attested"; then
    echo "Note: build provenance not checked (v$version predates attestations)."
  else
    gh attestation verify "$tmp/$asset" --repo "$repo" >/dev/null \
      || { echo "the build provenance of $asset does not verify (gh attestation verify)" >&2; exit 1; }
    echo "Built by $repo's release workflow (attestation verified)"
  fi

  port="${DEVSTACK_HUB_PORT:-4250}"
  hub_was_running=""
  if hub_answers "$port"; then hub_was_running=1; fi

  tar -xzf "$tmp/$asset" -C "$tmp"
  mkdir -p "$dir"
  # Write next to the target and rename: atomic, and safe under a running daemon.
  cp "$tmp/devstack" "$dir/.devstack.new"
  chmod 755 "$dir/.devstack.new"
  mv -f "$dir/.devstack.new" "$dir/devstack"

  echo "Installed: $("$dir/devstack" --version) -> $dir/devstack"
  # The dashboard hub starts at logon (XDG autostart) and the dashboard can
  # start it (devstack:// link). DEVSTACK_NO_HUB_INTEGRATION=1 skips both and
  # records it (`hub uninstall` leaves the marker `devstack up` respects).
  if [ -z "${DEVSTACK_NO_HUB_INTEGRATION:-}" ] || [ "${DEVSTACK_NO_HUB_INTEGRATION}" = 0 ]; then
    "$dir/devstack" hub install >/dev/null \
      || echo "Note: could not register the hub with your session (devstack hub install)." >&2
  else
    "$dir/devstack" hub uninstall >/dev/null \
      || echo "Note: could not record the opt-out (devstack hub uninstall)." >&2
  fi
  # The running hub serves from the old binary: move it onto the new one.
  if [ -n "$hub_was_running" ]; then
    "$dir/devstack" hub stop >/dev/null 2>&1 || true
    i=0
    while [ "$i" -lt 25 ] && hub_answers "$port"; do sleep 0.2; i=$((i + 1)); done
    "$dir/devstack" hub start >/dev/null \
      || echo "Note: start the hub again with \`devstack hub start\`." >&2
  fi
  case ":$PATH:" in
    *":$dir:"*) ;;
    *) echo "Note: $dir is not on your PATH." ;;
  esac
}

fetch() {
  curl --proto '=https' --tlsv1.2 -fsSL -o "$2" "$1"
}

# Where the URL redirects to (not followed).
fetch_head() {
  curl --proto '=https' --tlsv1.2 -fsS -o /dev/null -w '%{redirect_url}\n' "$1"
}

hub_answers() {
  curl -fsS -m 2 -o /dev/null "http://127.0.0.1:$1/hub/ping" 2>/dev/null
}

# `older 0.9.8 0.10.0`: true when the first version is lower.
older() {
  [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "${1%%-*}" "${2%%-*}" | sort -V | head -n1)" = "${1%%-*}" ]
}

main "$@"
