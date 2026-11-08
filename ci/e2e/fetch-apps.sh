#!/usr/bin/env bash
# Download the test installers listed in apps.tsv into $1 and verify their
# pinned SHA-256. Runs on the CI host (the tests themselves run offline).
set -euo pipefail
out=${1:?usage: fetch-apps.sh OUTDIR}
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$out"
while IFS=$'\t' read -r file sha url; do
    [[ -z "$file" || "$file" == \#* ]] && continue
    if ! echo "$sha  $out/$file" | sha256sum -c --status 2>/dev/null; then
        curl -fsSL --retry 5 --retry-all-errors -o "$out/$file.part" "$url"
        mv "$out/$file.part" "$out/$file"
    fi
    echo "$sha  $out/$file" | sha256sum -c
done < "$here/apps.tsv"
