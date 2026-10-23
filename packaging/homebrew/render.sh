#!/bin/sh
# The formula of this directory with a release's version and the checksums of
# its archives, on stdout (release.yml, jobs homebrew and homebrew-tap).
#
#   sh packaging/homebrew/render.sh <version> <directory with the archives> [<url base>]
#
# <url base> replaces the release's download URL: a test passes
# file://<directory> to install the archives it just built.
set -eu

version=$1
dir=$2
release="https://github.com/nuetzliches/devstack/releases/download/v{{version}}"
base=${3:-https://github.com/nuetzliches/devstack/releases/download/v$version}

sum() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1
}

script="s|$release|$base|g; s|{{version}}|$version|g"
for target in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-musl x86_64-unknown-linux-musl; do
  archive="$dir/devstack-$target.tar.gz"
  [ -f "$archive" ] || { echo "render.sh: $archive is missing" >&2; exit 1; }
  script="$script; s|{{sha256_$(echo "$target" | tr - _)}}|$(sum "$archive")|g"
done

formula=$(sed -e "$script" "$(dirname "$0")/devstack.rb")
case "$formula" in
  *'{{'*) echo "render.sh: a placeholder is left" >&2; exit 1 ;;
esac
printf '%s\n' "$formula"
