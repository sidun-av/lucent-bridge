#!/usr/bin/env bash
# Make DXVK the Direct3D 8-11 implementation for every Wine prefix.
#
# Fedora's wine-dxvk %posttrans runs `vulkaninfo` and, when that fails (as
# it always does in an image build or container without a GPU), registers
# DXVK at alternatives priority 5, below wined3d's 10 - so DXVK is silently
# off on every machine built from such an image. This switches each
# wine-{d3d8,d3d9,d3d10core,d3d11,dxgi} alternative that has a DXVK
# candidate to it. Run as root after installing wine-dxvk*.
set -uo pipefail
shopt -s nullglob
ls -l /etc/alternatives | grep -i 'wine-d3d\|wine-dxgi' || echo 'no wine d3d alternatives'
for link in /etc/alternatives/wine-d3d8* /etc/alternatives/wine-d3d9* \
            /etc/alternatives/wine-d3d10core* /etc/alternatives/wine-d3d11* \
            /etc/alternatives/wine-dxgi*; do
    name=${link#/etc/alternatives/}
    if ! info=$(alternatives --display "$name" 2>&1); then
        echo "skipping $name: $info"
        continue
    fi
    dxvk=$(printf '%s\n' "$info" | sed -n 's/^\(\/[^ ]*dxvk-[^ ]*\.dll\) - .*/\1/p' | head -n1)
    if [ -z "$dxvk" ]; then
        echo "no DXVK candidate for $name:"; printf '%s\n' "$info"
    else
        alternatives --set "$name" "$dxvk"
        echo "$name -> $dxvk"
    fi
done
exit 0
