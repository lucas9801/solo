# SignPath Foundation application / SignPath Foundation 申请材料

Material for applying to [SignPath Foundation](https://signpath.org) for free
code signing, and what to set up afterwards. Requirements as read on
2026-11-11 from [signpath.org/terms](https://signpath.org/terms) and
[docs.signpath.io](https://docs.signpath.io); check the current versions
before applying.

申请 SignPath Foundation 免费代码签名用的材料，以及批准后要做的设置。条款于 2026-11-11 查阅自 signpath.org/terms 和 docs.signpath.io，申请前以官网最新为准。

## Project / 项目

| | |
|---|---|
| Name / 名称 | Solo |
| Description (English) | Solo is a terminal-first code editor for Windows: a fork of the Zed editor that puts a Warp-style terminal at the center (command blocks, completions, restored sessions), for working with AI coding agents such as Claude Code in the terminal while viewing and editing code beside it. |
| 简介（中文） | Solo 是 Windows 上以终端为中心的代码编辑器：基于 Zed 编辑器的分支，把类似 Warp 的终端放在中心（命令块、补全、会话恢复），方便在终端里使用 Claude Code 等 AI 编程智能体，同时在旁边查看和编辑代码。 |
| License / 许可证 | GPL-3.0-or-later (some crates inherited from Zed are Apache-2.0); no commercial dual licensing / 无商业双重许可 |
| Repository / 仓库 | https://github.com/lucas9801/solo |
| Releases / 下载与发布页 | https://github.com/lucas9801/solo/releases |
| Code signing policy / 签名政策 | https://github.com/lucas9801/solo/blob/main/docs/code-signing-policy.md (linked from the README and every release) |
| Package contents / 安装包内容 | https://github.com/lucas9801/solo/blob/main/docs/package-contents.md |
| Upstream / 上游 | https://github.com/zed-industries/zed (publishes signed Windows builds / 上游发布签名的 Windows 版) |
| Platform / 平台 | Windows x64 |
| Maintainer / 维护者 | lucas9801 (https://github.com/lucas9801) — sole committer, reviewer and approver / 唯一的提交者、审阅者和批准者 |

## How it's built / 构建方式

- Pushing a version tag `v<version>` (done by `script/release-solo.ps1`
  after it commits the version and the release notes on `main`) starts
  [`.github/workflows/solo-package.yml`](../.github/workflows/solo-package.yml)
  on GitHub-hosted `windows-latest` runners.
- `build` job: `cargo build --profile solo-dist` of the five Solo binaries
  from source (Rust toolchain pinned in `rust-toolchain.toml`, dependencies
  pinned in `Cargo.lock`), no cached build outputs; checks that `solo.exe`'s
  product version ends with the tagged commit.
- `package` job: SignPath signing request 1 (Solo's exes + Inno Setup
  uninstaller), then the zip and the Inno Setup installer from the signed
  files, then signing request 2 (the installer). Both wait for manual
  approval.
- `release` job: creates a **draft** GitHub release with the zip, the
  installer and the release notes; the maintainer publishes it by hand.
  Solo's built-in updater only sees published releases.
- Signing is behind the repository variable `SOLO_SIGNPATH_ENABLED`; until
  it's `true`, the same workflow produces unsigned packages.

中文：推送版本标签后，GitHub 托管的 Windows 运行器从源码构建（工具链和依赖版本固定，不用缓存的构建产物），核对 solo.exe 的版本号里带着该提交；然后提交第 1 轮签名（Solo 自己的 exe 和 Inno 卸载程序），用签好的文件打 zip 和安装程序，再提交第 2 轮签名（安装程序），两轮都要人工批准；最后创建草稿 release，由维护者手动发布。签名由仓库变量 `SOLO_SIGNPATH_ENABLED` 开关，关闭时同一流程产出未签名的包。

## Things to state in the application / 申请时要说明的

1. **Fork of Zed.** SignPath's rule for modified upstream builds: upstream
   publishes signed builds (Zed does), the project *visibly* uses a fork,
   release branches are based on upstream branches that are usually signed,
   and all upstream changes get code review. The repository isn't a GitHub
   "fork" (GitHub reports `fork: false`) but the README says on its first
   lines that it's a fork of Zed; ask whether that counts (or whether to
   re-create it as a GitHub fork). Upstream merges must be reviewed by the
   maintainer before they're merged into `main`.
   ——Solo 是 Zed 的分支：GitHub 上不是"fork"关系，但 README 开头写明了；要问 SignPath 是否算"明显的分支"。上游合并需要审阅。
2. **Non-OSI but open licenses** in the packages: CDLA-Permissive-2.0
   (certificate data in `webpki-roots`) and the Inno Setup License (the
   installer). See [package-contents.md](package-contents.md#components-to-watch--需要注意的组件).
3. **Prebuilt libwebrtc** (BSD-3-Clause), statically linked, downloaded with
   a pinned SHA-256 rather than built from source.
4. **Single maintainer**: one person is committer, reviewer and approver.
5. **Network access** of the program: see the privacy section of the code
   signing policy (update checks to GitHub, language server downloads, all
   switchable off; no telemetry).

## Before applying / 申请前要做的

- [ ] Turn on two-factor authentication on GitHub (and on SignPath once the
      account exists). / GitHub 开启两步验证（SignPath 账号开通后也要）。
- [ ] Merge this branch so the code signing policy is on `main` and linked
      from the README. / 合并本分支，让签名政策页出现在 main 上并能从 README 找到。
- [ ] Release at least once through the CI workflow (unsigned), so the
      project "is released in the form to be signed". / 先用 CI 流程（未签名）发布至少一次。
- [ ] Give the helper exes version resources with product name "Solo" and
      Solo's version (see package-contents.md, item 6). / 给辅助程序加版本信息。
- [ ] Optional: add a SHA-256 check to the ConPTY download in
      `crates/zed/build.rs`. / 可选：给 ConPTY 下载加 SHA-256 校验。

Apply at https://signpath.org/apply (the form asks for the information in
the table above). / 在 signpath.org 申请（表格里要填上面的信息）。

## After approval: setting up signing / 批准后：开启签名

In SignPath (organization provided by SignPath Foundation):

1. Project with slug `solo`, repository URL `https://github.com/lucas9801/solo`.
2. Trusted build system: GitHub.com, linked to the project; install the
   SignPath GitHub App on the repository if asked.
   ([Trusted build systems: GitHub](https://docs.signpath.io/trusted-build-systems/github))
3. Artifact configurations: `exes` from
   [`script/signpath/exes.xml`](../script/signpath/exes.xml) and `installer`
   from [`script/signpath/installer.xml`](../script/signpath/installer.xml)
   (check them against SignPath's schema in its editor;
   [artifact configuration](https://docs.signpath.io/artifact-configuration)).
4. Signing policy `release-signing` with the Foundation's certificate, origin
   verification on, manual approval by the maintainer, and the allowed
   branch/ref set to `main` and the `v*` tags (how tag refs are matched isn't
   documented on the [origin verification page](https://docs.signpath.io/origin-verification);
   test with the first tag). A pipeline policy with
   `github-build-policies: runners: require_github_hosted: true`
   ([pipeline policies](https://docs.signpath.io/pipeline-policies)).
5. A CI user with submitter rights; its API token goes into the repository
   secret `SIGNPATH_API_TOKEN`.

In GitHub (Settings → Secrets and variables → Actions):

- Secret `SIGNPATH_API_TOKEN`.
- Variables `SIGNPATH_ORGANIZATION_ID`, and `SIGNPATH_PROJECT_SLUG` /
  `SIGNPATH_SIGNING_POLICY_SLUG` only if they differ from `solo` /
  `release-signing`.
- Variable `SOLO_SIGNPATH_ENABLED` = `true` — the switch. Remove it (or set
  anything else) to go back to unsigned packages.

Then push the next version tag; approve the two signing requests in SignPath
when the workflow waits for them (up to an hour each), check the draft
release (both files should show "SignPath Foundation" under Properties →
Digital Signatures), and publish it.
