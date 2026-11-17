# Code signing policy / 代码签名政策

[English](#english) · [中文](#中文)

## English

Solo is installed and updated only from the Microsoft Store, where it's
listed as Solo Code. Microsoft signs the Store package when it's published;
the files in it aren't signed separately. Releases on GitHub have no Windows
download any more (no installer, no zip), and nothing is signed through
SignPath.

### Version information

Each of Solo's own programs (`solo.exe`, `solo-cli.exe`,
`solo-agent-hook.exe`, `solo-update-helper.exe`, `solo-terminal-host.exe`)
carries the same version information (`crates/windows_resources`), which
[`solo-package.yml`](../.github/workflows/solo-package.yml) checks after
building:

| Field | Value |
|---|---|
| Product name | `Solo` |
| Product version, file version | Solo's version, e.g. `2.3.0` |
| Company | `lucas9801` (the repository owner) |
| Copyright | `Copyright lucas9801 and Solo contributors. Based on Zed, Copyright 2022 - 2025 Zed Industries, Inc. GPL-3.0-or-later` |
| Comments | the build, `<version>+stable.<run>.<commit>` |

Every file in the package, with its source and license, is listed in
[package-contents.md](package-contents.md).

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
| Update check | Solo Code from the Microsoft Store asks the Store (Windows' `StoreContext`); other builds (from source, or earlier releases' installer or zip) ask `api.github.com` (this repository's releases) | 30 s after start, then every 6 hours | `"auto_update": false` |
| Language servers and Node.js | GitHub releases, the npm registry, `nodejs.org` | When a file of a language is opened whose language server isn't installed yet (and checks for newer versions) | `"enable_language_server": false`, per language `"language_servers"`, or a language server's `binary.path` in `"lsp"` |
| Extension updates | Zed's extension API (`api.zed.dev`) | Only when extensions are installed | Don't install extensions, or `"auto_update_extensions": { "<id>": false }` |
| Git avatars | `avatars.githubusercontent.com` / GitHub API | When commit details of a GitHub repository are shown | `"git": { "blame": { "show_avatar": false } }` |

Programs the user runs in Solo's terminals (shells, AI agents such as Claude
Code) are not part of Solo and follow their own policies.

### Where this policy is linked

From the README's "更多文档" list. Release notes linked it until releases
stopped being signed.

## 中文

Solo 只从 Microsoft Store 安装和更新，在 Store 上名为 Solo Code。Store 安装包在上架时由微软签名，里面的文件不单独签名。GitHub 上的发布不再有 Windows 下载（没有安装程序，也没有 zip），也不再通过 SignPath 签名。

### 版本信息

Solo 自己的每个程序（`solo.exe`、`solo-cli.exe`、`solo-agent-hook.exe`、`solo-update-helper.exe`、`solo-terminal-host.exe`）都带同样的版本信息（`crates/windows_resources`），[`solo-package.yml`](../.github/workflows/solo-package.yml) 构建后会核对：产品名 `Solo`；产品版本和文件版本都是 Solo 的版本（如 `2.3.0`）；公司 `lucas9801`（仓库所有者）；版权 `Copyright lucas9801 and Solo contributors. Based on Zed, Copyright 2022 - 2025 Zed Industries, Inc. GPL-3.0-or-later`；备注（Comments）是构建号 `<版本>+stable.<运行号>.<提交>`。

安装包里每个文件的来源和许可证见 [package-contents.md](package-contents.md)。

### 隐私

Solo 是代码编辑器，它的联网大多是用户要求的：用户自己配置的 AI 服务和智能体、MCP 服务器、从扩展市场安装的扩展、SSH 远程项目、用户点开的链接。Solo 不发送遥测和崩溃报告：两者默认关闭（`telemetry.metrics`、`telemetry.diagnostics`；如果用户打开，会发到 Zed 的服务器），Solo 也没有内置崩溃报告服务器。它不会自己登录 Zed 账号，也不使用 Zed 托管的 AI 模型。

不需要用户每次要求、Solo 会自己联网的地方（根据 2.2.0 版源代码阅读整理）：

| 做什么 | 访问哪里 | 什么时候 | 怎么关 |
|---|---|---|---|
| 检查更新 | 从 Microsoft Store 装的 Solo Code 问 Store（Windows 的 `StoreContext`）；其他版本（从源码构建，或以前的安装程序、zip 装的）问 `api.github.com`（本仓库的 releases） | 启动 30 秒后，之后每 6 小时 | `"auto_update": false` |
| 语言服务器和 Node.js | GitHub releases、npm 仓库、`nodejs.org` | 打开某种语言的文件而它的语言服务器还没装时（也会检查新版本） | `"enable_language_server": false`、按语言设置 `"language_servers"`，或在 `"lsp"` 里给语言服务器指定 `binary.path` |
| 扩展更新 | Zed 的扩展 API（`api.zed.dev`） | 只在装了扩展时 | 不装扩展，或设置 `"auto_update_extensions": { "<扩展 id>": false }` |
| Git 头像 | `avatars.githubusercontent.com` / GitHub API | 显示 GitHub 仓库的提交详情时 | `"git": { "blame": { "show_avatar": false } }` |

用户在 Solo 终端里运行的程序（shell、Claude Code 等 AI 智能体）不属于 Solo，遵循它们自己的政策。

### 本页在哪里链接

README 的"更多文档"。发布不再签名之后，release 说明里不再链接本页。
