#!/usr/bin/env bash
# Make the two RPM sources in $1: the source tarball (git archive of HEAD,
# exactly what GitHub serves for a tag) and the vendored crates tarball, so
# the RPM builds with no network access.
# Needs git and cargo; run from the repository root.
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
ls -l "$out"
