# Lucent Bridge

Double-click a Windows `.exe` or `.msi` on **Lucent OS** (Fedora Atomic + KDE Plasma) and it runs. No virtual machine, no telemetry, no AI, no ads.

> **Status: first working prototype.** The logic is complete and tested (see [Testing](#testing)). It has **not** yet been run against a real Wine/FEX installation or real Windows installers on Lucent OS hardware. Treat everything under "Status" as the honest current state.

## Кратко

Lucent Bridge запускает программы Windows на Lucent OS двойным щелчком, без виртуальных машин:

- **На x86-64** программа выполняется процессором напрямую, а Wine переводит вызовы Windows API в вызовы Linux. Эмуляции нет.
- **На ARM (AArch64)** x86-инструкции программы переводятся в ARM с помощью **FEX-Emu**. Основной путь: Wine для ARM64 с поддержкой ARM64EC/WoW64 и DLL-эмуляторами FEX (`libarm64ecfex.dll`, `libwow64fex.dll`). Транслируется только код самой программы, а Wine, драйверы и графика работают нативно. Запасной путь: x86-64 Wine целиком под `FEXInterpreter`.
- У каждой программы свой префикс Wine (`~/.local/share/lucent-bridge/prefixes/<app-id>`). Когда установщик заканчивает работу, программа появляется в меню Plasma со своей иконкой.
- **Что не работает:** античиты уровня ядра (Riot Vanguard и подобные), драйверы Windows (принтеры, донглы, VPN-клиенты), всё, что требует загрузки кода в ядро. Ядра с 16K-страницами (например, Asahi на Mac) не поддерживаются: обходной путь там — виртуальная машина, а её мы сознательно не используем.
- Статус: прототип. Логика покрыта тестами, но с настоящими Wine и FEX на железе ещё не проверялась.

## How it works

```
                     ┌───────────────────────────────────────────────┐
  setup.exe ──────▶  │ lucent-bridge                                  │
  (double-click,     │  1. inspect: PE machine, GUI/console, version, │
   binfmt, or CLI)   │     icon, installer type (Inno/NSIS/MSI/...)   │
                     │  2. compat DB lookup (env, DLL overrides, ...) │
                     │  3. pick a runner for host × program × tools   │
                     │  4. per-app prefix, winetricks verbs           │
                     │  5. run; after installers: launcher entries    │
                     └───────────────────────────────────────────────┘
```

| Host    | Program                  | Runner            | What executes the x86 code                    |
|---------|--------------------------|-------------------|-----------------------------------------------|
| x86-64  | x86 / x86-64 / MSI       | `wine`            | the CPU, directly. Wine translates API calls only |
| x86-64  | ARM64 / ARM64EC / ARM64X | unsupported       | (no ARM→x86 translation is shipped)           |
| AArch64 | x86 / x86-64 / MSI       | `wine-fex` (primary) | FEX JIT inside the process, via Wine's ARM64EC (x86-64) and WoW64 (x86) emulator interface |
| AArch64 | x86 / x86-64 / MSI       | `fex-interpreter` (fallback) | FEX JIT translating an x86-64 Linux Wine and everything under it |
| AArch64 | ARM64 / ARM64X           | `wine-arm64`      | native, nothing translated                     |
| AArch64 | ARM64EC                  | `wine-arm64`      | native, with FEX for any x86-64 parts          |

The selector (`src/runner.rs`) is a pure function and is tested over **every** combination of host, program architecture, Wine capabilities, FEX components, page size and preference (85,680 cases).

### The ARM mechanism: why ARM64EC/WoW64 + FEX DLLs is primary

As of late 2026, FEX-Emu (releases 2605–2611) puts most of its Windows work into the *Wine-integrated* path: FEX is built as a Windows DLL (`libarm64ecfex.dll` for x86-64, `libwow64fex.dll` for 32-bit x86). An ARM64 Wine built with `--enable-archs=arm64ec,aarch64,i386` loads it, the same way Windows 11 on ARM loads its own emulator. Wine picks the DLLs from the `HODLL64` and `HODLL` environment variables (the registry equivalents are `HKLM\Software\Microsoft\Wow64\amd64` and `\x86`). This is the upstreamed form of the Hangover approach, and Valve's ARM Proton work uses it too.

We chose it as the primary path because:

- **Only the program's own code is translated.** Wine itself (ntdll, win32u, the graphics/audio/input drivers) and the host's Mesa/Vulkan drivers run as native ARM64. ARM64EC is ABI-compatible with x64, so calls from translated code into system DLLs drop straight into native code.
- **No x86 Linux userland.** No x86-64 RootFS, no x86 Mesa, no library thunks to keep in sync with the host. The host GPU driver is used directly.
- **32-bit programs work without any 32-bit libraries.** WoW64 handles them.
- **It is where upstream is going.** Recent FEX releases fixed ARM64EC suspend handling, moved to a unixlib split, adopted CHPEv2 for WoW64 exceptions and added a JIT code cache that works under Wine.

What it costs: Lucent OS has to ship an ARM64EC-enabled Wine. We could not confirm one in the main Fedora repositories at the time of writing; the `lacamar/wine-arm64ec` COPR provides both Wine and `fex-emu-wine` for Fedora. FEX also recommends some Wine patches that are not upstream yet.

**Fallback: x86-64 Wine under `FEXInterpreter`.** This path works with any stock x86-64 Wine inside the FEX RootFS and is FEX's most mature mode. The price is that all of Wine and its Linux dependencies are translated too, and it needs a multi-gigabyte x86-64 RootFS. It is selected automatically when the primary path is incomplete (for example, Wine without ARM64EC), or on request (`--runner fex-interpreter`, or `runner = "fex-interpreter"` in a compat entry).

**16 KiB / 64 KiB page kernels** (Asahi Linux on Apple silicon, some server kernels): Wine and FEX need 4 KiB pages. The known workaround is a micro-VM with a 4K kernel, and that violates the no-VM rule. Lucent Bridge says so plainly and refuses to run.

## Usage

```sh
lucent-bridge inspect setup.exe            # what is this file? (--json, --extract-icon out.png)
lucent-bridge plan setup.exe               # dry run: the exact commands `run` would execute
lucent-bridge plan --assume-tools --host aarch64 setup.exe   # ...as on a stock ARM Lucent OS
lucent-bridge run setup.exe [-- args...]   # run it; installers get launcher entries afterwards
lucent-bridge list                         # installed apps (one prefix each)
lucent-bridge remove <app-id> [--yes]      # delete prefix, launcher entries, icons
lucent-bridge integrate <app-id>           # rescan a prefix for shortcuts
```

`plan` works on any OS (it is how development on macOS works). `run` needs Linux.

Example dry run of a 32-bit Inno Setup installer on ARM:

```
Lucent Bridge plan for IrfanView [irfanview-e9d5c050]
  Runner:    wine-fex — Wine ARM64EC/WoW64 + FEX (x86 → ARM translation)
  Prefix:    ~/.local/share/lucent-bridge/prefixes/irfanview-e9d5c050 (will be created)
  Compat:    IrfanView (matched by product name, status: works, from compat/apps.toml)
Steps:
  1. Create the Windows environment (first launch only)
     $ env HODLL=libwow64fex.dll HODLL64=libarm64ecfex.dll WINEDEBUG=-all WINEDLLOVERRIDES=winemenubuilder.exe=d WINEPREFIX=... /usr/bin/wine wineboot --init
  2. Install components: corefonts
     $ env ... WINE=/usr/bin/wine ... /usr/bin/winetricks -q corefonts
  3. Start IrfanView
     $ env ... /usr/bin/wine /home/me/Downloads/iview_setup.exe
  4. Wait for the installer to finish completely
     $ env ... /usr/bin/wineserver -w
  5. Add the installed program to the application launcher
```

### Double-click and `./setup.exe`

- `packaging/lucent-bridge.desktop` registers a hidden MIME handler for `application/x-ms-dos-executable`, `application/x-msdownload`, `application/x-msi` (and the newer `application/vnd.microsoft.portable-executable`). `packaging/mimeapps.list` makes it the default.
- `packaging/binfmt.d/lucent-bridge.conf` registers the `MZ` magic, so an executable `.exe` can be started from a shell. The interpreter is a symlink named `lucent-bridge-binfmt`. Invoked under that name, the binary acts as `lucent-bridge run -- <file>`.
- Progress and errors appear through `notify-send` or `kdialog` when one of them and a graphical session exist. Otherwise they are silently skipped. Errors use a `kdialog --error` box, because after a double-click nothing else would be visible.

### Per-app prefixes and launcher entries

Each program gets its own Wine prefix at `$XDG_DATA_HOME/lucent-bridge/prefixes/<app-id>`. The app ID is a readable slug of the product name plus 8 hex digits of a SHA-256 over product and company, e.g. `notepadplusplus-1f0c9a2e`. It stays the same across versions, and two vendors' "Editor" programs get different prefixes. A program that lives inside a prefix (the one an installer just put there) always runs in that prefix.

Wine's own menu builder is disabled (`winemenubuilder.exe=d`), so Wine doesn't litter the menu with `wine-*` entries. When an installer finishes (`wineserver -w`), Lucent Bridge:

1. reads the `.lnk` shortcuts it created (Start menu, Public desktop; symlinked desktops are never followed), plus any Wine-generated `.desktop` files that belong to this prefix;
2. maps their targets into the prefix (case-insensitively, as Windows does), skips uninstallers, documents and missing targets, and de-duplicates;
3. extracts each program's icon (largest image of the first icon group, PNG or BMP) to PNG;
4. writes spec-quoted `lucent-bridge-<app-id>-<name>.desktop` files with `StartupWMClass` set so the Plasma task manager groups windows correctly, and removes the Wine-generated entries it replaced.

### Compatibility database

TOML, built-in entries in `compat/`, plus `/usr/share/lucent-bridge/compat.d`, `/etc/lucent-bridge/compat.d` and `~/.config/lucent-bridge/compat.d` (later files win). Entries match by SHA-256, then product name, then file name:

```toml
[[app]]
name = "Adobe Photoshop CS6"
match.product = ["Adobe Photoshop CS6"]
match.exe = ["Photoshop.exe"]
status = "partial"                      # works | partial | broken | unknown
verbs = ["vcrun2010", "msxml6", "atmlib", "gdiplus"]   # winetricks, once per prefix
dll_overrides = { msxml6 = "native,builtin" }
env = { SOME_VAR = "1" }
runner = "fex-interpreter"              # optional AArch64 preference
native_alternative = "flatpak: ..."     # suggested instead, if it exists
notes = "Shown to the user."
```

Every entry is validated: environment variable names, override syntax, verb names (no shell injection) and hash format. `status = "broken"` blocks the launch with the entry's explanation, for example for kernel anti-cheat; `run --force` tries anyway. The shipped entries are **examples** whose statuses still need confirming on real hardware.

## Status: what works and what doesn't

**Implemented and tested**

- PE inspection: i386 / x86-64 / ARM64, plus ARM64EC and ARM64X (detected through the load-config CHPE pointer); GUI vs console; .NET; DLLs. Version resources (language-aware), group icon → PNG, Inno Setup / NSIS / WiX Burn / InstallShield / MSI / name heuristics. The reader is bounds-checked everywhere and fuzzed by truncation in the tests.
- Runner selection, plans and execution, per-app prefixes, compat DB, `.lnk` parsing, desktop integration, `list` / `remove` / `integrate`.
- `run` is verified end to end on Linux CI (x86-64 and AArch64) against a *fake* Wine that behaves like an installer. The tests check that the right commands run, with the right environment and in the right order, and that the launcher entry appears.

**Not yet verified / known gaps**

- **Not yet run with real Wine, FEX or real installers on Lucent OS hardware.** That is the next milestone.
- MSI packages are named after their file name; the MSI SummaryInformation stream isn't parsed yet.
- winetricks verbs are not applied on the `fex-interpreter` runner (it would need winetricks itself running under FEX).
- For image-based FEX RootFS (SquashFS/EroFS), the x86-64 Wine inside can't be auto-detected. Set `LUCENT_BRIDGE_FEX_GUEST_WINE=/usr/bin/wine`. Other overrides: `LUCENT_BRIDGE_WINE`, `LUCENT_BRIDGE_FEX_DLL_DIR`, `LUCENT_BRIDGE_FEX_INTERPRETER`.
- Icons are written as absolute-path PNGs, not into the hicolor theme.
- `remove` deletes the prefix; it doesn't run the program's own uninstaller.
- `wine-systemd` registers a competing `MZ` binfmt entry; only one should be active (the Lucent OS image should not install `wine-systemd`).

**Won't work, by design or by nature**

- **Kernel-mode anti-cheat and DRM** (Riot Vanguard; Easy Anti-Cheat / BattlEye where the developer hasn't enabled Linux support). Wine can't load Windows kernel drivers, and nothing short of running Windows can.
- **Windows drivers of any kind**: vendor printer/scanner drivers, USB dongles, VPN clients, hardware tools, file-system filters.
- Explorer shell extensions, Microsoft Store/UWP apps.
- **ARM programs on x86-64 PCs.** There is no ARM→x86 translation.
- **x86 vs ARM differences:** on AArch64, x86 code runs through a JIT. Expect lower CPU performance than native, a slower first start while code is translated (FEX's code cache shortens later starts) and occasional emulation-specific bugs. ARM64 Windows builds of a program, when they exist, run natively and are preferable. Kernels with 16 KiB pages are refused (see above).

## Building and testing

```sh
cargo build --release
cargo test                 # unit + CLI tests; the fake-Wine `run` tests are Linux-only
cargo clippy --all-targets -- -D warnings
```

Every test fixture (PE files with resources and icons, `.lnk` shortcuts) is generated in-process by `src/testpe.rs`. No third-party binaries are in the repository.

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`, `clippy -D warnings`, `cargo test` and `desktop-file-validate` on `ubuntu-24.04` (x86-64) and `ubuntu-24.04-arm` (AArch64).

Packaging: `packaging/lucent-bridge.spec` is an RPM stub that the Lucent OS compose can layer with rpm-ostree. On AArch64 it requires `fex-emu` and recommends `fex-emu-wine`. It still builds with network access to crates.io; switch to Fedora Rust packaging macros or vendored crates before Koji/COPR.

## Principles

- **No virtual machines.** Programs run on the real CPU: directly on x86-64, through FEX's x86→ARM translation on AArch64.
- **No telemetry, no network access, no ads, no AI.** Lucent Bridge makes no network connections of its own (winetricks downloads only the components listed in a compat entry).
- **Honest failures.** If something can't work, the user is told why in plain language, and what would fix it.

## License

Apache-2.0. See [LICENSE](LICENSE).
