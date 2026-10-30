#!/usr/bin/env bash
# Prints the Markdown table of Rust crates compiled into annex and Annex Manager, with each crate's copyright line
# from its own license files, for THIRD_PARTY_NOTICES.md. Run it after changing dependencies and
# paste the output over the crate table in that file.
set -euo pipefail
cd "$(dirname "$0")/.."
registry="${CARGO_HOME:-$HOME/.cargo}/registry/src"
echo "| Crate | Version | License | Copyright |"
echo "|---|---|---|---|"
cargo tree --edges normal --target x86_64-unknown-linux-gnu --all-features --prefix none \
    --format '{p}|{l}' \
  | grep -v '^annex ' \
  | sed -E 's/ \(\*\)$//; s/ \(proc-macro\)//' \
  | sort -u \
  | while IFS='|' read -r package license; do
      name="${package%% *}"
      version="${package##* v}"
      source=$(ls -d "$registry"/*/"$name-$version" 2>/dev/null | head -1)
      copyright=$(cat "$source"/LICENSE* "$source"/COPYRIGHT* 2>/dev/null \
        | grep -m1 -iE '^\s*copyright (\(c\)|©)?\s*[0-9]' \
        | sed -E 's/^\s+//; s/\|/\\|/g' || true)
      echo "| $name | $version | $license | ${copyright:-the $name authors (no notice in its license file)} |"
    done
