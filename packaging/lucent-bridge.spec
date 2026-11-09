# Lucent Bridge for Lucent OS (Fedora Atomic). Builds offline: Source1 is
# the vendored crates tarball and Source2 the upstream DXVK release, both
# made by ci/rpm/sources.sh (attached to every GitHub release next to the
# RPMs).

# Upstream DXVK for x86_64 (Fedora 45's 2.7.1 crashes on 0 Hz displays);
# pinned in packaging/dxvk/SHA256SUMS. AArch64 uses its Wine's ARM64EC DXVK.
%global dxvk_version 3.1.1

Name:           lucent-bridge
Version:        0.2.1
Release:        1%{?dist}
Summary:        Run Windows programs without a virtual machine
# DXVK (x86_64 only): Zlib
License:        Apache-2.0 AND Zlib
URL:            https://github.com/sidun-av/lucent-bridge
Source0:        %{url}/archive/v%{version}/%{name}-%{version}.tar.gz
Source1:        %{url}/releases/download/v%{version}/%{name}-%{version}-vendor.tar.xz
Source2:        https://github.com/doitsujin/dxvk/releases/download/v%{dxvk_version}/dxvk-%{dxvk_version}.tar.gz

ExclusiveArch:  x86_64 aarch64

BuildRequires:  cargo >= 1.88
BuildRequires:  rust >= 1.88
BuildRequires:  gcc
BuildRequires:  desktop-file-utils
BuildRequires:  systemd-rpm-macros
# lucent-start.exe (windows/lucent-start.c), an x86-64 Windows program
BuildRequires:  mingw64-gcc

# Not the `wine` metapackage: it pulls wine-desktop -> wine-systemd, whose
# binfmt entry for MZ executables competes with ours.
Requires:       wine-core
Conflicts:      wine-systemd
# Installed locally so first launch never asks to download them.
Requires:       wine-mono
%ifarch x86_64
Recommends:     mingw64-wine-gecko
Recommends:     mingw32-wine-gecko
%endif
Recommends:     winetricks
Recommends:     (libnotify or kdialog)
%ifarch aarch64
# x86 -> ARM translation. fex-emu-wine provides libarm64ecfex.dll and
# libwow64fex.dll (primary path); fex-emu provides FEXInterpreter
# (fallback path, needs an x86-64 RootFS with Wine).
Requires:       fex-emu
Recommends:     fex-emu-wine
%endif

%description
Lucent Bridge lets people double-click a Windows .exe or .msi and have it
run. On x86-64 the program's code runs directly on the CPU with Wine
translating Windows API calls. On AArch64 its x86 instructions are
translated to ARM by FEX-Emu inside Wine's ARM64EC/WoW64 layer. No virtual
machines, no telemetry, no AI. Installers get clean launcher entries with
the program's own icon.

%prep
%autosetup -n %{name}-%{version}
tar -xJf %{SOURCE1}
%ifarch x86_64
tar -xzf %{SOURCE2}
%endif
mkdir -p .cargo
cat > .cargo/config.toml <<'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
EOF

%build
cargo build --release --locked --offline
# Starts programs with the filtered (non-elevated) token; see the source.
# An x86-64 PE on both architectures (Fedora has no AArch64 MinGW).
x86_64-w64-mingw32-gcc -O2 -s -Wall -municode -mwindows \
    -o lucent-start.exe windows/lucent-start.c -ladvapi32

%install
install -Dpm 0755 target/release/%{name} %{buildroot}%{_bindir}/%{name}
install -Dpm 0644 lucent-start.exe %{buildroot}%{_datadir}/%{name}/windows/lucent-start.exe
%ifarch x86_64
for a in x64 x32; do
    install -d %{buildroot}%{_datadir}/%{name}/dxvk/$a
    install -pm 0644 dxvk-%{dxvk_version}/$a/*.dll %{buildroot}%{_datadir}/%{name}/dxvk/$a/
done
cp -p packaging/dxvk/LICENSE DXVK-LICENSE
%endif
install -d %{buildroot}%{_libexecdir}/%{name}
ln -s ../../bin/%{name} %{buildroot}%{_libexecdir}/%{name}/%{name}-binfmt
install -Dpm 0644 packaging/binfmt.d/%{name}.conf %{buildroot}%{_binfmtdir}/%{name}.conf
install -Dpm 0644 packaging/mimeapps.list %{buildroot}%{_datadir}/%{name}/mimeapps.list
install -d %{buildroot}%{_datadir}/%{name}/compat.d
desktop-file-install --dir=%{buildroot}%{_datadir}/applications packaging/%{name}.desktop

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/%{name}.desktop
cargo test --release --locked --offline

%post
%binfmt_apply %{name}.conf

%postun
if [ $1 -eq 0 ] && [ -e /proc/sys/fs/binfmt_misc/LucentBridge ]; then
    echo -1 > /proc/sys/fs/binfmt_misc/LucentBridge || :
fi

%files
%license LICENSE
%ifarch x86_64
%license DXVK-LICENSE
%endif
%doc README.md
%{_bindir}/%{name}
%{_libexecdir}/%{name}/
%{_binfmtdir}/%{name}.conf
%{_datadir}/applications/%{name}.desktop
%{_datadir}/%{name}/

%changelog
* Mon Nov 09 2026 Lucent OS developers - 0.2.1-1
- A UI font without some scripts (Lucent's Figtree: no Cyrillic) falls back
  to Inter in Windows dialogs, as on the desktop, before Tahoma
* Mon Nov 09 2026 Lucent OS developers - 0.2.0-1
- Programs start with the filtered (non-elevated) token, as on Windows
  for an administrator with UAC (no "[Administrator]" in Notepad++);
  installers keep full rights
- Windows programs follow the desktop: light/dark colours (Pebble Drift),
  UI font, DPI from the Plasma scale, smooth text
- x86_64: bundled upstream DXVK 3.1.1 (Fedora's 2.7.1 crashes on 0 Hz)
- run --progress-fd and the launch sheet (lucent-bridge-sheet): progress,
  Cancel, kind explanations (anti-cheat, hibernated Windows disk, ...)
* Mon Nov 09 2026 Lucent OS developers - 0.1.2-1
- A prefix copied from the template is relabelled for SELinux
  (it carried the template's var_lib_t into the user's home)
* Mon Nov 09 2026 Lucent OS developers - 0.1.1-1
- build-template works without HOME (from a system service)
- Launcher entries are added as soon as an installer exits, not only
  after the program it started (e.g. "Run Notepad++") is closed
* Sun Nov 08 2026 Lucent OS developers - 0.1.0-1
- First release: vendored offline build, prefix template support
  (lucent-bridge build-template), run --silent
