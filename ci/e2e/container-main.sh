#!/usr/bin/env bash
# Entry point inside the e2e container (started as root, network disabled).
#   /src   lucent-bridge checkout (read-only, already built: target/release)
#   /apps  verified installers (read-only)
#   /out   results, logs, screenshots (written)
# Installs lucent-bridge the way the RPM does, builds the test programs, then
# runs the tests as an ordinary user.
set -euo pipefail
exec > >(tee -a /out/container.log) 2>&1

echo "== host: $(uname -m), page size $(getconf PAGESIZE), $(nproc) CPUs"
cat /etc/lucent-e2e-packages.txt
wine --version
alternatives --display wine-d3d11 2>/dev/null | head -3 || true
readlink -f "$(command -v wine)"
ls -d /usr/lib*/wine* /usr/lib*/wine*/wine/* 2>/dev/null
rpm -ql fex-emu-wine 2>/dev/null | grep -i '\.dll$' || true
command -v FEXInterpreter || true

if [ -n "${LB_RPM_DIR:-}" ] && compgen -G "$LB_RPM_DIR/lucent-bridge-*.$(uname -m).rpm" >/dev/null; then
    # The RPM built in this run; plain rpm (no --nodeps), so its Requires
    # and Conflicts are checked against the Lucent OS package set.
    # Installed the way lucent-os/build_files/build.sh does it (dnf, local
    # file); repos are off because the container has no network.
    rpmfile=$(ls "$LB_RPM_DIR"/lucent-bridge-[0-9]*."$(uname -m)".rpm)
    if ! dnf -y -q --disablerepo='*' install "$rpmfile"; then
        echo "NOTE: dnf refused the unsigned RPM; the OS image needs --nogpgcheck (or a signed RPM)"
        dnf -y -q --disablerepo='*' --nogpgcheck install "$rpmfile"
    fi
    rpm -qR lucent-bridge | grep -v '^rpmlib' | tr '\n' ' '; echo
else
    install -Dm755 /src/target/release/lucent-bridge /usr/bin/lucent-bridge
    install -d /usr/libexec/lucent-bridge /usr/share/lucent-bridge/compat.d
    ln -sf /usr/bin/lucent-bridge /usr/libexec/lucent-bridge/lucent-bridge-binfmt
fi
lucent-bridge --version

# Test programs, built from source in this repository.
mkdir -p /opt/lb-tests
x86_64-w64-mingw32-gcc -O2 -mwindows -o /opt/lb-tests/d3d11_sample.exe \
    /src/ci/e2e/d3d11-sample/d3d11_sample.c -ld3d11 -ldxgi -luuid
x86_64-w64-mingw32-gcc -O2 -o /opt/lb-tests/hello.exe /src/ci/e2e/d3d11-sample/hello.c
# What a program sees about its rights; the second copy asks for
# administrator rights in its manifest.
x86_64-w64-mingw32-gcc -O2 -municode -mwindows -o /opt/lb-tests/admincheck.exe /src/ci/e2e/d3d11-sample/admincheck.c -ladvapi32
(cd /src/ci/e2e/d3d11-sample && x86_64-w64-mingw32-windres -O coff -o /tmp/admincheck-admin.res admincheck-admin.rc)
x86_64-w64-mingw32-gcc -O2 -municode -mwindows -o /opt/lb-tests/admincheck-admin.exe /src/ci/e2e/d3d11-sample/admincheck.c /tmp/admincheck-admin.res -ladvapi32
file /opt/lb-tests/*.exe

# Optional: the prefix template the OS image would ship (built as root, as
# in the image build).
if [ "${LB_TEMPLATE:-0}" = 1 ]; then
    start=$(date +%s.%N)
    lucent-bridge build-template /usr/share/lucent-bridge/prefix-template
    end=$(date +%s.%N)
    echo "template build: $(awk "BEGIN{printf \"%.1f\", $end - $start}") s, size $(du -sh /usr/share/lucent-bridge/prefix-template | cut -f1)"
fi

chown -R tester: /out
runuser -u tester -- env -i PATH=/usr/bin:/bin HOME=/home/tester USER=tester LOGNAME=tester \
    LB_ARCH="$(uname -m)" LB_TEMPLATE="${LB_TEMPLATE:-0}" LUCENT_BRIDGE_APPEARANCE="${LUCENT_BRIDGE_APPEARANCE:-auto}" \
    bash /src/ci/e2e/tests.sh /apps /opt/lb-tests /out
