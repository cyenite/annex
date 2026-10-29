#!/usr/bin/env bash
# Resumable Waydroid image download with mirror rotation; `waydroid init` stalls on a single slow mirror.
# Usage: fetch-waydroid-images.sh <dest-dir>
set -uo pipefail

dest=${1:?dest dir}
mirrors=(yer kumisystems freefr)

fetch() {
  local remote=$1 file=$2 sha=$3 attempt=0
  until echo "$sha  $file" | sha256sum -c --quiet >/dev/null 2>&1; do
    local mirror=${mirrors[$((attempt % ${#mirrors[@]}))]}
    attempt=$((attempt + 1))
    ((attempt > 60)) && { echo "[$file] giving up"; return 1; }
    echo "[$file] attempt $attempt via $mirror ($(du -h "$file" 2>/dev/null | cut -f1))"
    curl -sSL -C - --speed-limit 20000 --speed-time 30 -o "$file" \
      "https://downloads.sourceforge.net/project/waydroid/$remote/$file?use_mirror=$mirror"
  done
  echo "[$file] verified"
}

mkdir -p "$dest" && cd "$dest" || exit 1
fetch images/vendor/waydroid_x86_64 \
  lineage-20.0-20261022-MAINLINE-waydroid_x86_64-vendor.zip \
  91663c337013145208b6c533d7e1fa2a915a9864ddc8fc56966f40cd759648ac &&
  fetch images/system/lineage/waydroid_x86_64 \
    lineage-20.0-20260927-VANILLA-waydroid_x86_64-system.zip \
    053552725bf4ae25e2d3f7e6f1947203294aa9d73dc17fe1f3abe646e5d3bd8a
