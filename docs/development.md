# Building Solo from source

[← README](../README.md)

Building Solo from source, moved here from the README.

## Building

See [Building Zed for Windows](https://github.com/zed-industries/zed/blob/main/docs/src/development/windows.md)
for the toolchain (the README's "Build from source" section lists what Solo needs), then:

```powershell
cargo run -p solo              # debug build, runs target\debug\solo.exe
script\package-solo.ps1       # optimized build, staged in dist\Solo
script\package-solo.ps1 -Step Installer   # dist\Solo-Setup-<version>.exe from dist\Solo (Inno Setup 6)
```

`dist\Solo` is what the Microsoft Store package and the installer are made
from (see below); `solo.exe` also runs straight from there. The installer
(`script\solo.iss`) needs Inno Setup 6 (`ISCC.exe` on `PATH`, in Inno Setup's
usual folders, or given with `-Iscc`); it's unsigned. Don't run a locally
built installer on a machine with Solo installed from GitHub: it has the same
AppId and updates that Solo in its folder. Releases have no zip.

### Notes for local builds

- **Toolchain.** `rust-toolchain.toml` pins Rust 1.98.1; rustup installs it on
  the first build. Visual Studio (or the Build Tools) needs the MSVC x64/x86
  build tools, the MSVC Spectre-mitigated libs, a Windows SDK of at least
  10.0.20348.0, and CMake (wasmtime's C API builds with it); see Zed's
  Windows page above for the component IDs. If `cmake` isn't on `PATH`, put
  Visual Studio's on it or set `CMAKE` to its path (for example
  `...\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe`).
- **Helper programs.** `cargo run -p solo` builds only `solo.exe`. Solo's other
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

Solo checks for a newer version 30 seconds after it starts and every 6 hours
after that (turn this off with `"auto_update": false`; **Check for Updates**
in the command palette always checks). The Store package asks the Store,
which installs the update. Other builds (installed with the GitHub
installer, or built from source) ask
[Solo's GitHub releases](https://github.com/lucas9801/solo/releases), download
the release's `Solo-Setup-<version>.exe` and have `solo-update-helper.exe` run
it silently into Solo's folder once Solo quits
(`crates/auto_update/src/solo_update.rs`). A release without an installer is
offered with "Get It from the Microsoft Store", which opens Solo Code's Store
page.

Releases are built by GitHub Actions, not locally (local builds are for
testing). A version tag starts the
[Solo package workflow](../.github/workflows/solo-package.yml), which builds
Solo, checks the version information of its exes (product name Solo, Solo's
version, built from the tagged commit), makes the Store package and the
installer, builds the Linux remote servers, and creates a draft GitHub
release with the installer, the remote servers and the notes in
`docs/release-notes/<version>.md`. Running copies of Solo only update to
published releases.

The notes start with the two summary lines the update button shows (one
sentence in Chinese, then English; `script/check-solo-release-notes.ps1`
checks them), then the link line: both downloads, the GitHub installer first
and then the Store, neither marked recommended (the Store can be a version
behind), the user guide, and what the `.gz` files are, for example:

```markdown
下载 / Download: [GitHub 安装程序 / installer `Solo-Setup-<version>.exe`](https://github.com/lucas9801/solo/releases/download/v<version>/Solo-Setup-<version>.exe)（未签名，运行时点"更多信息"→"仍要运行" / unsigned: click "More info", then "Run anyway"） · [Microsoft Store](https://get.microsoft.com/installer/download/9NBW452JVWN6)（[商店页面 / Store page](https://apps.microsoft.com/detail/9NBW452JVWN6)；由微软签名，可能晚一个版本 / signed by Microsoft, may be a version behind） · [用户指南](https://github.com/lucas9801/solo/blob/main/docs/guide/zh-CN.md) · [User guide](https://github.com/lucas9801/solo/blob/main/docs/guide/en.md) · `solo-remote-server-linux-*.gz` 是远程开发用的 Linux 服务端，不是安装程序 / are the Linux server for remote development, not installers.
```

Nothing is signed by the workflow: the Store signs its package, and the
installer is unsigned.
The repository settings used for SignPath signing are no longer read and can
be deleted: the variables `SOLO_SIGNPATH_ENABLED`, `SIGNPATH_ORGANIZATION_ID`,
`SIGNPATH_PROJECT_SLUG` and `SIGNPATH_SIGNING_POLICY_SLUG`, and the secret
`SIGNPATH_API_TOKEN`.

## The Microsoft Store package

Solo is packaged as an MSIX for the Microsoft Store, where it's listed as
Solo Code; that's how it's installed. `script\package-solo-msix.ps1` makes
the package from a release build staged by `package-solo.ps1`; it refuses a
debug build (one reads its assets and shaders from the source tree, so it
crashes once installed) and, for the Store, a build that isn't of the stable
channel:

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
"Open with" for text and code files, and the context menu. It doesn't have
the restricted capability `unvirtualizedResources` (the Store refused it), so
Windows redirects Solo's new files in `%APPDATA%` and `%LOCALAPPDATA%` (its
data folder among them) to the package's own folder; the script refuses a
manifest with it. The Solo package workflow also builds the Store package and keeps it as the
`solo-windows-x64-msix` artifact; it isn't part of the GitHub release.

Without `-Store` the script makes a test package to sideload, signed with a
certificate given by `-CertificateThumbprint`; the comments at the top of the
script show how to keep it apart from an installed Solo.

A packaged Solo:

- doesn't use its own updater: the Store updates it. **Check for Updates**
  offers to open its page in the Store.
- starts the programs in its terminals outside the package
  (`PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY`, set by `solo-terminal-host`),
  so the user's tools and agents write to the real `%APPDATA%` and
  `%LOCALAPPDATA%`.
- puts what it hands to them in the package's `LocalCache\Solo` folder
  (`util::packaged::package_shared_dir`), which they see at the same path:
  the shell integration scripts, and the copies of `solo-cli.exe` and
  `solo-agent-hook.exe` in `bin` (made when they changed). It gives its
  terminals (`SOLO_CLI`, `SOLO_AGENT_HOOK`) and agents' hooks those paths,
  which stay the same across updates, rather than the versioned folder it
  runs from. `bin`
  goes first on `PATH` in Solo's terminals, so `solo-cli` works there;
  outside Solo there's no `solo-cli` command. The package has no other
  application than Solo: the Store rejects one without a Start menu entry
  (`AppListEntry="none"`), and the script refuses to make such a package.
- asks Windows to start it again (`RegisterApplicationRestart`) when an
  update closes it.

### Submitting to the Store

Publishing a GitHub release starts the
[Solo Store submission workflow](../.github/workflows/solo-store.yml). It
finds the tag's successful Solo package run (it builds nothing), checks that
the `.msixupload` in its `solo-windows-x64-msix` artifact is version
`x.y.z.0` for the tag `vx.y.z`, and with the
[Microsoft Store Developer CLI](https://learn.microsoft.com/windows/apps/publish/msstore-dev-cli/overview)
(`msstore`) uploads it to a new submission, writes "What's new in this
version" in each Store listing, and submits it for certification. It doesn't
wait for certification (up to three business days): the log ends with the
submission's status and a link to it in Partner Center. Prereleases aren't
submitted.

"What's new in this version" comes from `docs/release-notes/<version>.md`
(`script\msix\store-release-notes.ps1`, tested by
`script\msix\test-store-release-notes.ps1` in Solo CI): the summary line and
the titles of the items, in Chinese for the `zh-*` listings and in English
for the others; if that's over the Store's 1500 characters, just the summary.

**The switch.** A published release is submitted only when the repository
variable `SOLO_STORE_SUBMIT_ENABLED` is `true`. While it isn't, the workflow
skips the submission and says so in its log, and the package is submitted by
hand: download the `solo-windows-x64-msix` artifact of the tag's Solo package
run, unzip it, upload the `.msixupload` to a new submission in Partner
Center, fill in "What's new in this version" (`script\msix\store-release-notes.ps1
-Path docs\release-notes\<version>.md -Languages zh-cn,en-us` prints it) and
submit it. Keep the switch off until the setup below is done.

**Setting it up (once).**

1. Solo Code's first submission is made by hand in Partner Center; `msstore`
   can't make it, and it only updates free apps (Solo Code is free).
2. Associate the Partner Center account with a Microsoft Entra tenant
   (Partner Center: Account settings > Tenants), if it isn't already.
3. In the Microsoft Entra admin center, register an application (App
   registrations > New registration) and create a client secret for it
   (Certificates & secrets). Copy the secret's value right away; it isn't
   shown again.
4. In Partner Center, Account settings > User management > Microsoft Entra
   applications, add that application with the **Manager** role.
5. In the GitHub repository, Settings > Environments, create the environment
   `microsoft-store` (only the workflow's submit job uses it; its deployment
   branches and tags can be limited to `main` and `v*` tags). Give it:
   - the secret `SOLO_STORE_CLIENT_SECRET`: the client secret;
   - the variables `SOLO_STORE_TENANT_ID` (the Entra tenant ID),
     `SOLO_STORE_CLIENT_ID` (the application's client ID),
     `SOLO_STORE_SELLER_ID` (Partner Center's seller ID, in Account settings)
     and `SOLO_STORE_ID` (Solo Code's Store ID, in Partner Center under
     Product identity).
   The job fails, naming them, if any is missing.
6. Try it: Actions > Solo Store submission > Run workflow, with the latest
   release's tag and **Only upload** ticked. Partner Center then has a draft
   submission with the package and the notes, which isn't submitted; check
   it, then submit or delete it there.
7. Set the repository variable `SOLO_STORE_SUBMIT_ENABLED` to `true`. From the
   next release on, publishing the GitHub release submits the Store package.

Client secrets expire (at most two years after they're made, as chosen in
Entra). Before it does, create a new one for the same application and replace
`SOLO_STORE_CLIENT_SECRET`; with an expired one the job fails at signing in.

**Retrying.** Run the workflow by hand with the tag and **Only upload**
unticked; a run by hand doesn't look at the switch. A draft submission
already in Partner Center (from an "only upload" run, or made there) is
deleted and replaced, and whatever was changed in it is lost. If a submission
is being certified or published, the job fails and says so: wait for it to
finish, or cancel it in Partner Center, and run the workflow again.
