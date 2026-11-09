#!/usr/bin/env bash
# Make the RPM sources in $1: the source tarball (git archive of HEAD,
# exactly what GitHub serves for a tag), the vendored crates tarball (so the
# RPM builds with no network access) and the upstream DXVK release bundled
# into the x86_64 RPM (pinned in packaging/dxvk/SHA256SUMS).
# Needs git, cargo and curl; run from the repository root.
set -euo pipefail
out=$(realpath -m "${1:?usage: sources.sh OUTDIR}")
mkdir -p "$out"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
name=lucent-bridge
git archive --format=tar.gz --prefix="$name-$version/" -o "$out/$name-$version.tar.gz" HEAD
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
git archive --format=tar HEAD | tar -x -C "$work"
(cd "$work" && cargo vendor --locked --versioned-dirs vendor > /dev/null)
tar -C "$work" --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
    -cJf "$out/$name-$version-vendor.tar.xz" vendor

# DXVK: the spec's version and the pinned file must agree.
dxvk_version=$(sed -n 's/^%global dxvk_version //p' packaging/lucent-bridge.spec)
dxvk_file=dxvk-$dxvk_version.tar.gz
grep -q "  $dxvk_file\$" packaging/dxvk/SHA256SUMS ||
    { echo "packaging/dxvk/SHA256SUMS has no entry for $dxvk_file (spec: $dxvk_version)"; exit 1; }
if [ ! -f "$out/$dxvk_file" ]; then
    curl -fsSL --retry 3 -o "$out/$dxvk_file" \
        "https://github.com/doitsujin/dxvk/releases/download/v$dxvk_version/$dxvk_file"
fi
(cd "$out" && grep "  $dxvk_file\$" "$OLDPWD/packaging/dxvk/SHA256SUMS" | sha256sum -c -)
ls -l "$out"
