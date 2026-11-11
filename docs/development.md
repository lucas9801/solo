# Developing and releasing Solo

[← README](../README.md)

Building Solo from source and how releases are made, moved here from the
README. Signing: [code signing policy](code-signing-policy.md).

## Building

See [Building Zed for Windows](https://github.com/zed-industries/zed/blob/main/docs/src/development/windows.md)
for the toolchain (the README's "从源码编译" section lists what Solo needs), then:

```powershell
cargo run -p zed              # debug build, runs target\debug\solo.exe
script\package-solo.ps1       # optimized build, packaged as dist\Solo-<version>-windows-x64.zip
```

With [Inno Setup 6](https://jrsoftware.org/isinfo.php) installed (or given
with `-Iscc <path to ISCC.exe>`), it also builds `dist\Solo-Setup-<version>.exe`
from `script\solo.iss`: an English and Chinese installer for the current user,
with a Start menu entry, an optional desktop icon, and an uninstall entry in
Settings → Apps.

The zip, which needs no installer, contains `install.ps1`, which installs Solo
for the current user with Start menu and desktop shortcuts.

### Notes for local builds

- **Toolchain.** `rust-toolchain.toml` pins Rust 1.98.1; rustup installs it on
  the first build. Visual Studio (or the Build Tools) needs the MSVC x64/x86
  build tools, the MSVC Spectre-mitigated libs, a Windows SDK of at least
  10.0.20348.0, and CMake (wasmtime's C API builds with it); see Zed's
  Windows page above for the component IDs. If `cmake` isn't on `PATH`, put
  Visual Studio's on it or set `CMAKE` to its path (for example
  `...\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe`).
- **WebRTC.** livekit's `webrtc-sys` links a prebuilt libwebrtc. Without
  `LK_CUSTOM_WEBRTC` its build script downloads it (about 93 MB, from
  zed-industries/livekit-rust-sdks' GitHub releases) into its build folder on
  each fresh build of a profile, so nothing needs doing by hand, only access
  to GitHub. To download it once and reuse it (as the CI does):

  ```powershell
  script\solo-ci-webrtc.ps1 -Tag webrtc-0001d84-4 -Sha256 b16f68e1fef8bd8089b9c7fc46c3c9b43ad5257c84460794a3668ec5accc1f13 -Dir C:\deps\webrtc
  $env:LK_CUSTOM_WEBRTC = "C:\deps\webrtc\win-x64-release"
  ```

  The tag and hash are `WEBRTC_TAG` and `WEBRTC_SHA256` in
  `.github/workflows/solo-ci.yml`.
- **Helper programs.** `cargo run -p zed` builds only `solo.exe`. Solo's other
  programs are found next to it: `solo-agent-hook.exe` (Claude Code's hooks
  fall back to slower PowerShell scripts without it), `solo-terminal-host.exe`
  (without it terminals run inside Solo and end with it) and `solo-cli.exe`
  (`solo-cli agent`; cargo builds it as `cli.exe`). To have them in a debug
  build:

  ```powershell
  cargo build -p solo_agent_hook -p solo_terminal_host -p cli
  Copy-Item target\debug\cli.exe target\debug\solo-cli.exe
  ```

  `script\package-solo.ps1` builds and places all of them.
- **Disk.** Measured on the maintainer's machine (2026-11-12, sizes of the
  files under each folder): a `solo-dist` build (what `package-solo.ps1`
  builds) takes 14.3 GB in `target\solo-dist`; the Rust toolchain about
  3 GB in `%USERPROFILE%\.rustup`; cargo's downloaded sources 3.3 GB in
  `%USERPROFILE%\.cargo`. A debug build's own share of `target\debug` (one
  copy of each crate's library, plus `solo.exe` with its 1.8 GB `solo.pdb`)
  came to roughly 12 GB, but `target\debug` keeps every version of every
  crate built since, and incremental compilation adds more: the shared one
  there had grown to 118 GB. Plan for at least 40 GB free, more if you build
  often; `cargo clean` frees it.
- **Memory.** Cargo runs one `rustc` per core, and some take more than 1 GB;
  with little memory, limit them with `-j 4` or `CARGO_BUILD_JOBS`.

## Updates and releases

Solo checks [its GitHub releases](https://github.com/lucas9801/solo/releases)
30 seconds after it starts and every 6 hours after that (turn this off with
`"auto_update": false`; **Check for Updates** in the command palette always
checks). When there's a newer version it offers to update: it downloads the
installer, quits, installs it silently where Solo is, and starts again.

Releases are built by GitHub Actions, not locally (local builds are for
testing). To release a version:

```powershell
script\release-solo.ps1 -NotesFile notes.md [-Version 1.2.3] [-DryRun]
```

The notes start with a one-sentence summary in Chinese, then the same in
English, plain text; the update notification shows the one in Solo's language.
It raises the patch version unless given one, commits the version with the
notes (`docs/release-notes/<version>.md`), tags the commit and pushes both
(`-DryRun` only shows what it would do). The tag starts the
[Solo package workflow](../.github/workflows/solo-package.yml), which builds
Solo, checks solo.exe was built from the tagged commit, packages the installer
and the zip, and creates a draft GitHub release with them; publish the draft
once it looks right. Running copies of Solo only update to published releases.
