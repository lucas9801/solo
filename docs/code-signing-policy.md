# Code signing policy / 代码签名政策

[English](#english) · [中文](#中文)

## English

Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org).

> Status: Solo has applied (or is about to apply) to SignPath Foundation.
> Until signing is turned on, releases are built and packaged the same way
> but are **not signed**; this page describes how they are signed once it is.

### What is signed

Only files built from this repository ([github.com/lucas9801/solo](https://github.com/lucas9801/solo))
by its GitHub Actions workflow
[`solo-package.yml`](../.github/workflows/solo-package.yml), on GitHub-hosted
runners, from a version tag (`v*`) on the `main` branch. Nothing built on a
developer's computer is signed.

Signed in two rounds, each a separate SignPath signing request that has to be
approved by hand:

1. Solo's own programs: `solo.exe`, `solo-cli.exe`, `solo-agent-hook.exe`,
   `solo-update-helper.exe`, `solo-terminal-host.exe`, and the Inno Setup
   uninstaller that the installer puts next to them (`unins000.exe`). The
   portable zip `Solo-<version>-windows-x64.zip` is made from these signed
   files.
2. The installer `Solo-Setup-<version>.exe`, built with Inno Setup from the
   files signed in round 1.

`conpty.dll` and `OpenConsole.exe` (Microsoft's console host, from
[microsoft/terminal](https://github.com/microsoft/terminal), MIT) are shipped
as Microsoft signs them and are not re-signed. Every file in the packages,
with its source and license, is listed in
[package-contents.md](package-contents.md).

### Team roles

| Role | Members |
|---|---|
| Committers and reviewers | [lucas9801](https://github.com/lucas9801) (repository owner) |
| Approvers (approve each signing request) | [lucas9801](https://github.com/lucas9801) (repository owner) |

Changes from anyone else (pull requests) are reviewed by a committer before
they are merged. Changes merged from upstream Zed are reviewed the same way.
All members use multi-factor authentication on GitHub and on SignPath.

### Privacy

Solo is a code editor, so most of what it does on the network is something the
user asks for: AI providers and agents the user sets up, MCP servers, extensions
installed from the extension gallery, remote projects over SSH, links the user
opens. Solo sends no telemetry and no crash reports: both are off by default
(`telemetry.metrics`, `telemetry.diagnostics`; turned on, they would go to
Zed's servers), and no crash report server is built into Solo. It doesn't sign in to a Zed account on its own and doesn't use
Zed's hosted AI models.

What Solo contacts without being asked each time (from reading Solo's source
code at version 2.2.0):

| What | Where | When | How to turn it off |
|---|---|---|---|
| Update check | `api.github.com` (this repository's releases); the installer is downloaded from GitHub only after the user chooses to update | 30 s after start, then every 6 hours | `"auto_update": false` |
| Language servers and Node.js | GitHub releases, the npm registry, `nodejs.org` | When a file of a language is opened whose language server isn't installed yet (and checks for newer versions) | `"enable_language_server": false`, per language `"language_servers"`, or a language server's `binary.path` in `"lsp"` |
| Extension updates | Zed's extension API (`api.zed.dev`) | Only when extensions are installed | Don't install extensions, or `"auto_update_extensions": { "<id>": false }` |
| Git avatars | `avatars.githubusercontent.com` / GitHub API | When commit details of a GitHub repository are shown | `"git": { "blame": { "show_avatar": false } }` |

Programs the user runs in Solo's terminals (shells, AI agents such as Claude
Code) are not part of Solo and follow their own policies.

### Where this policy is linked

From the [README](../README.md#code-signing-policy) and from the notes of every
release ([releases](https://github.com/lucas9801/solo/releases)).

## 中文

Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org).
（免费代码签名由 SignPath.io 提供，证书来自 SignPath Foundation。）

> 状态：Solo 正在（或即将）向 SignPath Foundation 申请。签名开启之前，发布版照常构建和打包，但**没有签名**；本页说明开启之后怎么签。

### 签什么

只签由本仓库（[github.com/lucas9801/solo](https://github.com/lucas9801/solo)）的 GitHub Actions 工作流 [`solo-package.yml`](../.github/workflows/solo-package.yml) 在 GitHub 托管的运行器上、从 `main` 分支上的版本标签（`v*`）构建出的文件。开发者本机构建的东西一律不签。

分两轮签，每轮都是一次单独的 SignPath 签名请求，要人工批准：

1. Solo 自己的程序：`solo.exe`、`solo-cli.exe`、`solo-agent-hook.exe`、`solo-update-helper.exe`、`solo-terminal-host.exe`，以及安装程序放在它们旁边的 Inno Setup 卸载程序（`unins000.exe`）。便携版 zip（`Solo-<版本>-windows-x64.zip`）用这些签好的文件打包。
2. 安装程序 `Solo-Setup-<版本>.exe`，用第 1 轮签好的文件由 Inno Setup 生成。

`conpty.dll` 和 `OpenConsole.exe`（微软的控制台宿主，来自 [microsoft/terminal](https://github.com/microsoft/terminal)，MIT 许可）保持微软自己的签名，不重新签。安装包里每个文件的来源和许可证见 [package-contents.md](package-contents.md)。

### 成员与角色

| 角色 | 成员 |
|---|---|
| 提交者和审阅者 | [lucas9801](https://github.com/lucas9801)（仓库所有者） |
| 批准者（批准每一次签名请求） | [lucas9801](https://github.com/lucas9801)（仓库所有者） |

其他人的改动（pull request）由提交者审阅后才合并；从上游 Zed 合并的改动同样审阅。所有成员在 GitHub 和 SignPath 上都开启两步验证。

### 隐私

Solo 是代码编辑器，它的联网大多是用户要求的：用户自己配置的 AI 服务和智能体、MCP 服务器、从扩展市场安装的扩展、SSH 远程项目、用户点开的链接。Solo 不发送遥测和崩溃报告：两者默认关闭（`telemetry.metrics`、`telemetry.diagnostics`；如果用户打开，会发到 Zed 的服务器），Solo 也没有内置崩溃报告服务器。它不会自己登录 Zed 账号，也不使用 Zed 托管的 AI 模型。

不需要用户每次要求、Solo 会自己联网的地方（根据 2.2.0 版源代码阅读整理）：

| 做什么 | 访问哪里 | 什么时候 | 怎么关 |
|---|---|---|---|
| 检查更新 | `api.github.com`（本仓库的 releases）；用户选择更新后才从 GitHub 下载安装程序 | 启动 30 秒后，之后每 6 小时 | `"auto_update": false` |
| 语言服务器和 Node.js | GitHub releases、npm 仓库、`nodejs.org` | 打开某种语言的文件而它的语言服务器还没装时（也会检查新版本） | `"enable_language_server": false`、按语言设置 `"language_servers"`，或在 `"lsp"` 里给语言服务器指定 `binary.path` |
| 扩展更新 | Zed 的扩展 API（`api.zed.dev`） | 只在装了扩展时 | 不装扩展，或设置 `"auto_update_extensions": { "<扩展 id>": false }` |
| Git 头像 | `avatars.githubusercontent.com` / GitHub API | 显示 GitHub 仓库的提交详情时 | `"git": { "blame": { "show_avatar": false } }` |

用户在 Solo 终端里运行的程序（shell、Claude Code 等 AI 智能体）不属于 Solo，遵循它们自己的政策。

### 本页在哪里链接

[README](../README.md#code-signing-policy) 和每个 release 的说明（[releases](https://github.com/lucas9801/solo/releases)）。
