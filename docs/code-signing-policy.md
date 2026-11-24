# Code signing policy / 代码签名政策

[English](#english) · [中文](#中文)

## English

Solo is installed from the Microsoft Store, where it's listed as Solo Code,
or with the installer attached to each GitHub release
(`Solo-Setup-<version>.exe`).

- **The Store package is signed by Microsoft** when it's published; the files
  in it aren't signed separately.
- **The GitHub installer is currently unsigned**, and so are the programs it
  installs (Solo's own exes; `conpty.dll` and `OpenConsole.exe` keep
  Microsoft's signature). Windows' SmartScreen warns about it when it's run
  after downloading it with a browser ("More info", then "Run anyway"). Solo
  installed with it updates itself with the next release's installer, which
  it checks against the SHA-256 GitHub reports for the file, not against a
  signature.

Nothing is signed through SignPath, and releases have no zip.

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

Every file in the Store package and the installer, with its source and
license, is listed in [package-contents.md](package-contents.md).

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
| Update check | Solo Code from the Microsoft Store asks the Store (Windows' `StoreContext`); other builds (installed with the GitHub installer, or from source) ask `api.github.com` (this repository's releases) | 30 s after start, then every hour | `"auto_update": false` |
| Language servers and Node.js | GitHub releases, the npm registry, `nodejs.org` | When a file of a language is opened whose language server isn't installed yet (and checks for newer versions) | `"enable_language_server": false`, per language `"language_servers"`, or a language server's `binary.path` in `"lsp"` |
| Extension updates | Zed's extension API (`api.zed.dev`) | Only when extensions are installed | Don't install extensions, or `"auto_update_extensions": { "<id>": false }` |
| Git avatars | `avatars.githubusercontent.com` / GitHub API | When commit details of a GitHub repository are shown | `"git": { "blame": { "show_avatar": false } }` |

Programs the user runs in Solo's terminals (shells, AI agents such as Claude
Code) are not part of Solo and follow their own policies.

### Where this policy is linked

From the README's "More docs" list (and "更多文档" in README.zh-CN.md). Release notes linked it until releases
stopped being signed.

## 中文

Solo 可以从 Microsoft Store 安装（在 Store 上名为 Solo Code），也可以用每个 GitHub 发布里附带的安装程序（`Solo-Setup-<版本>.exe`）安装。

- **Store 安装包由微软签名**（上架时），里面的文件不单独签名。
- **GitHub 安装程序目前没有签名**，它装的程序也没有（Solo 自己的 exe；`conpty.dll` 和 `OpenConsole.exe` 保留微软的签名）。用浏览器下载后运行时，Windows 的 SmartScreen 会提示（点“更多信息”，再点“仍要运行”）。用它装的 Solo 用下一个发布的安装程序更新自己，校验的是 GitHub 给出的文件 SHA-256，不是签名。

不通过 SignPath 签名，发布里也没有 zip。

### 版本信息

Solo 自己的每个程序（`solo.exe`、`solo-cli.exe`、`solo-agent-hook.exe`、`solo-update-helper.exe`、`solo-terminal-host.exe`）都带同样的版本信息（`crates/windows_resources`），[`solo-package.yml`](../.github/workflows/solo-package.yml) 构建后会核对：产品名 `Solo`；产品版本和文件版本都是 Solo 的版本（如 `2.3.0`）；公司 `lucas9801`（仓库所有者）；版权 `Copyright lucas9801 and Solo contributors. Based on Zed, Copyright 2022 - 2025 Zed Industries, Inc. GPL-3.0-or-later`；备注（Comments）是构建号 `<版本>+stable.<运行号>.<提交>`。

Store 安装包和安装程序里每个文件的来源和许可证见 [package-contents.md](package-contents.md)。

### 隐私

Solo 是代码编辑器，它的联网大多是用户要求的：用户自己配置的 AI 服务和智能体、MCP 服务器、从扩展市场安装的扩展、SSH 远程项目、用户点开的链接。Solo 不发送遥测和崩溃报告：两者默认关闭（`telemetry.metrics`、`telemetry.diagnostics`；如果用户打开，会发到 Zed 的服务器），Solo 也没有内置崩溃报告服务器。它不会自己登录 Zed 账号，也不使用 Zed 托管的 AI 模型。

不需要用户每次要求、Solo 会自己联网的地方（根据 2.2.0 版源代码阅读整理）：

| 做什么 | 访问哪里 | 什么时候 | 怎么关 |
|---|---|---|---|
| 检查更新 | 从 Microsoft Store 装的 Solo Code 问 Store（Windows 的 `StoreContext`）；其他版本（GitHub 安装程序装的，或从源码构建的）问 `api.github.com`（本仓库的 releases） | 启动 30 秒后，之后每小时 | `"auto_update": false` |
| 语言服务器和 Node.js | GitHub releases、npm 仓库、`nodejs.org` | 打开某种语言的文件而它的语言服务器还没装时（也会检查新版本） | `"enable_language_server": false`、按语言设置 `"language_servers"`，或在 `"lsp"` 里给语言服务器指定 `binary.path` |
| 扩展更新 | Zed 的扩展 API（`api.zed.dev`） | 只在装了扩展时 | 不装扩展，或设置 `"auto_update_extensions": { "<扩展 id>": false }` |
| Git 头像 | `avatars.githubusercontent.com` / GitHub API | 显示 GitHub 仓库的提交详情时 | `"git": { "blame": { "show_avatar": false } }` |

用户在 Solo 终端里运行的程序（shell、Claude Code 等 AI 智能体）不属于 Solo，遵循它们自己的政策。

### 本页在哪里链接

README.zh-CN.md 的"更多文档"（英文 README 的"More docs"）。发布不再签名之后，release 说明里不再链接本页。
