#!/usr/bin/env bash
# apt-get install on a GitHub runner whose resolver cannot resolve the Ubuntu
# archive (2026-11-11, for hours: `Temporary failure resolving
# 'azure.archive.ubuntu.com'`, then 'archive.ubuntu.com' too, while other
# names resolved). Then the archive's names are looked up over DNS-over-HTTPS
# at an IP address, which needs no resolver, and written to /etc/hosts.
# Usage: apt-install.sh <package>...
set -euo pipefail

hosts=(archive.ubuntu.com security.ubuntu.com ports.ubuntu.com)

if ! getent hosts azure.archive.ubuntu.com > /dev/null; then
  echo "the runner does not resolve azure.archive.ubuntu.com: the Ubuntu archive instead"
  case "$(dpkg --print-architecture)" in
    amd64 | i386) mirror="http://archive.ubuntu.com/ubuntu/" ;;
    *) mirror="http://ports.ubuntu.com/ubuntu-ports/" ;;
  esac
  echo "$mirror" | sudo tee /etc/apt/apt-mirrors.txt > /dev/null
  for h in "${hosts[@]}"; do
    getent hosts "$h" > /dev/null && continue
    ip=$(curl -fsS --max-time 10 -H 'accept: application/dns-json' \
      "https://1.1.1.1/dns-query?name=$h&type=A" \
      | jq -r '[.Answer[]? | select(.type == 1) | .data][0] // empty') || ip=""
    if [ -n "$ip" ]; then
      echo "$ip $h" | sudo tee -a /etc/hosts
    else
      echo "no address for $h over DNS-over-HTTPS either"
    fi
  done
fi

sudo apt-get update -qq
sudo apt-get install -y -qq "$@"
