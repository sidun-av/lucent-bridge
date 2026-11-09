# Bundled DXVK (x86_64 RPM only)

The x86_64 RPM carries the upstream DXVK release named in `SHA256SUMS`
(`https://github.com/doitsujin/dxvk/releases`, zlib licence: `LICENSE` here,
copied from the release's source tree because the binary tarball has none).
It installs to `/usr/share/lucent-bridge/dxvk/{x64,x32}/` and Lucent Bridge
copies the DLLs into each prefix (`src/dxvk.rs`).

Why not Fedora's `wine-dxvk`: Fedora 45 has 2.7.1 only (checked on
2026-11-09: f45, f45-updates, f45-updates-testing and rawhide), and 2.7.1
divides by the display's refresh rate unguarded, so every Direct3D 10/11
program dies on a display that reports 0 Hz. DXVK 3.x guards it
(`computeRefreshPeriod` in `src/util/util_misc.h`).

To update: put the new release's file name and SHA-256 in `SHA256SUMS`
(compare with the digest on the GitHub release page) and the version in
`%global dxvk_version` in `lucent-bridge.spec`; `ci/rpm/sources.sh` checks
that both agree and that the download matches.
