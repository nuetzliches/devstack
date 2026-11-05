#!/bin/sh
# The CHANGELOG.md section of one release, without its heading: the text of the
# GitHub release (release.yml), which winget's ReleaseNotesUrl points at.
#
#   sh scripts/release-notes.sh X.Y.Z [CHANGELOG.md]
#
# Fails when the changelog has no such section or it is empty, so a release
# without notes is refused before it is published (docs/releasing.md).
set -eu

version=${1:?usage: release-notes.sh X.Y.Z [CHANGELOG.md]}
file=${2:-$(dirname "$0")/../CHANGELOG.md}

notes=$(tr -d '\r' <"$file" | awk -v v="$version" '
  /^## \[/ { if (on) exit; on = index($0, "## [" v "]") == 1; next }
  # The link references at the end of the file belong to no section.
  on && /^\[[^]]+\]: / { next }
  on && (started || NF) { started = 1; print }
')

if [ -z "$notes" ]; then
  echo "release-notes.sh: no section '## [$version]' with text in $file" >&2
  exit 1
fi
printf '%s\n' "$notes"
