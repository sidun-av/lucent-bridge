# Packaging stub for Lucent OS (Fedora Atomic). Lets the image compose
# layer Lucent Bridge with rpm-ostree. Builds with network access to
# crates.io; switch to %%cargo_prep/%%cargo_generate_buildrequires (Fedora
# Rust packaging) or a vendored tarball before submitting to Koji/COPR.

Name:           lucent-bridge
Version:        0.1.0
Release:        1%{?dist}
Summary:        Run Windows programs without a virtual machine
License:        Apache-2.0
URL:            https://github.com/sidun-av/lucent-bridge
Source0:        %{url}/archive/v%{version}/%{name}-%{version}.tar.gz

ExclusiveArch:  x86_64 aarch64

BuildRequires:  cargo >= 1.88
BuildRequires:  rust >= 1.88
BuildRequires:  desktop-file-utils
BuildRequires:  systemd-rpm-macros

Requires:       wine
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

%build
cargo build --release --locked

%install
install -Dpm 0755 target/release/%{name} %{buildroot}%{_bindir}/%{name}
install -d %{buildroot}%{_libexecdir}/%{name}
ln -s %{_bindir}/%{name} %{buildroot}%{_libexecdir}/%{name}/%{name}-binfmt
install -Dpm 0644 packaging/binfmt.d/%{name}.conf %{buildroot}%{_binfmtdir}/%{name}.conf
install -Dpm 0644 packaging/mimeapps.list %{buildroot}%{_datadir}/%{name}/mimeapps.list
install -d %{buildroot}%{_datadir}/%{name}/compat.d
desktop-file-install --dir=%{buildroot}%{_datadir}/applications packaging/%{name}.desktop

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/%{name}.desktop
cargo test --release --locked

%post
%binfmt_apply %{name}.conf

%postun
if [ $1 -eq 0 ] && [ -e /proc/sys/fs/binfmt_misc/LucentBridge ]; then
    echo -1 > /proc/sys/fs/binfmt_misc/LucentBridge || :
fi

%files
%license LICENSE
%doc README.md
%{_bindir}/%{name}
%{_libexecdir}/%{name}/
%{_binfmtdir}/%{name}.conf
%{_datadir}/applications/%{name}.desktop
%{_datadir}/%{name}/

%changelog
* Sun Nov 08 2026 Lucent OS developers - 0.1.0-1
- First prototype
