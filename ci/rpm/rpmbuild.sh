#!/usr/bin/env bash
# Build binary + source RPMs from the sources made by sources.sh.
#   $1 directory with the sources, $2 output directory
# Meant to run inside a Fedora container with the BuildRequires installed
# and networking disabled (proves the vendored build is self-contained).
set -euo pipefail
src=$(realpath "${1:?usage: rpmbuild.sh SOURCES OUTDIR}")
out=$(realpath -m "${2:?usage: rpmbuild.sh SOURCES OUTDIR}")
here=$(cd "$(dirname "$0")" && pwd)
top=$(mktemp -d)
trap 'rm -rf "$top"' EXIT
mkdir -p "$top/SOURCES" "$out"
cp "$src"/*.tar.* "$top/SOURCES/"
rpmbuild -ba --define "_topdir $top" "$here/../../packaging/lucent-bridge.spec"
find "$top/RPMS" "$top/SRPMS" -name '*.rpm' -exec cp {} "$out/" \;
ls -l "$out"
