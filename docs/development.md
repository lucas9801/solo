# Building Solo from source

[← README](../README.md)

Building Solo from source, moved here from the README. Signing:
[code signing policy](code-signing-policy.md).

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
testing). A version tag starts the
[Solo package workflow](../.github/workflows/solo-package.yml), which builds
Solo, checks the version information of its exes (product name Solo, Solo's
version, built from the tagged commit), packages the installer
and the zip, and creates a draft GitHub release with them and the notes in
`docs/release-notes/<version>.md`. Running copies of Solo only update to
published releases.

## The Microsoft Store package

Solo is also packaged as an MSIX for the Microsoft Store, where it's listed
as Solo Code. `script\package-solo-msix.ps1` makes the package from a release
build staged by `package-solo.ps1`; it refuses a debug build (one reads its
assets and shaders from the source tree, so it crashes once installed) and,
for the Store, a build that isn't of the stable channel:

```powershell
script\package-solo.ps1 -Step Stage     # release build in dist\Solo
cargo build --profile solo-dist -p explorer_command_injector --no-default-features --features solo
script\package-solo-msix.ps1 -Store     # dist\SoloCode-<version>.0-x64.msix and .msixupload
script\msix\test-msix-version.ps1       # tests of the version mapping
```

The second command builds the Explorer context menu ("Open with Solo Code",
a COM `IExplorerCommand` in `crates\explorer_command_injector`), which the
script finds in the cargo target folder. The Store package is unsigned (the
Store signs it), has Solo Code's identity from Partner Center, and the version
`x.y.z.0` for Solo `x.y.z`. Its manifest is `script\msix\AppxManifest.xml.in`:
one application, Solo, with the execution alias `solo`, `solo://` links,
"Open with" for text and code files, the context menu, and the restricted
capability `unvirtualizedResources`, so Solo and the unpackaged shells and
agents in its terminals share the real `%APPDATA%` and `%LOCALAPPDATA%`. The
Solo package workflow also builds the Store package and keeps it as the
`solo-windows-x64-msix` artifact; it isn't part of the GitHub release.

Without `-Store` the script makes a test package to sideload, signed with a
certificate given by `-CertificateThumbprint`; the comments at the top of the
script show how to keep it apart from an installed Solo.

A packaged Solo:

- doesn't use its own updater: the Store updates it. **Check for Updates**
  offers to open its page in the Store.
- copies `solo-cli.exe` and `solo-agent-hook.exe` to `bin` in its data
  folder (when they changed) and gives its terminals (`SOLO_CLI`,
  `SOLO_AGENT_HOOK`) and agents' hooks those paths, which stay the same
  across updates, rather than the versioned folder it runs from. That folder
  goes first on `PATH` in Solo's terminals, so `solo-cli` works there;
  outside Solo there's no `solo-cli` command. The package has no other
  application than Solo: the Store rejects one without a Start menu entry
  (`AppListEntry="none"`), and the script refuses to make such a package.
- asks Windows to start it again (`RegisterApplicationRestart`) when an
  update closes it.
