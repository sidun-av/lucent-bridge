#!/usr/bin/env bash
# Make DXVK the Direct3D 8-11 implementation for every Wine prefix.
#
# Fedora's wine-dxvk %posttrans runs `vulkaninfo` and, when that fails (as
# it always does in an image build or container without a GPU), registers
# DXVK at alternatives priority 5, below wined3d's 10 - so DXVK is silently
# off on every machine built from such an image. This switches each
# wine-{d3d8,d3d9,d3d10core,d3d11,dxgi} alternative that has a DXVK
# candidate to it. Run as root after installing wine-dxvk*.
set -euo pipefail
shopt -s nullglob
for link in /etc/alternatives/wine-d3d8* /etc/alternatives/wine-d3d9* \
            /etc/alternatives/wine-d3d10core* /etc/alternatives/wine-d3d11* \
            /etc/alternatives/wine-dxgi*; do
    name=${link#/etc/alternatives/}
    dxvk=$(alternatives --display "$name" | sed -n 's/^\(\/[^ ]*dxvk-[^ ]*\.dll\) - .*/\1/p' | head -1)
    if [ -n "$dxvk" ]; then
        alternatives --set "$name" "$dxvk"
        echo "$name -> $dxvk"
    fi
done
