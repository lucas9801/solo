# Solo

**让 agent 写代码，你来读、判断和指挥。**

Solo 是给 AI 编程 agent 用的编辑器，基于 [Zed](https://github.com/zed-industries/zed)：终端占满窗口，Claude Code、Codex 这些 agent 在真终端里跑；需要看代码时，完整的代码编辑器从右边打开，随时看 agent 改了什么。

[下载 Windows 版](https://github.com/lucas9801/solo/releases/latest) · [用户指南](docs/guide/zh-CN.md) · [和其他工具比较](docs/site/zh/compare/index.md) · [English](docs/site/en/index.md)

<!-- 演示视频：把 GitHub 上传后得到的 https://github.com/user-attachments/assets/… 链接单独放在这一行（做法见 docs/media/README.md 和 script/demo-video/README.md） -->
> **演示视频（待补，约 60 秒）**：分屏和工作区标签 → agent 工作时标签显示状态 → 点终端里的链接打开代码和 Mermaid 图 → 一轮结束后审阅、撤回一块改动 → 关掉 Solo 再打开，agent 还在跑。

## 功能

| 功能 | 一句话 |
|---|---|
| [终端和编辑器分屏](#终端和编辑器分屏) | 终端占满窗口，打开文件时编辑器从右边出现 |
| [工作区标签](#工作区标签按项目分开) | 每个项目一个标签，终端、agent、编辑器、文件树各归各的 |
| [agent 状态](#agent-状态一眼看清) | 标签上看 agent 在工作、等你回复、做完还是出错，以及上下文用量 |
| [按轮审阅和回滚](#按轮审阅和回滚) | 每一轮改了什么、新增几个错误，逐块留下或撤回 |
| [完整的代码编辑器](#完整的代码编辑器) | Zed 的语言服务、跳转、搜索、Git 都在 |
| [终端里的链接](#终端里的链接一点就开) | 输出里的文件路径一点就开：代码、Markdown、图片 |
| [Mermaid 图](#mermaid-图) | agent 输出和文档里的 Mermaid 直接看图 |
| [文档审阅](#文档审阅) | 在 agent 写的文档上批注，一次发回去 |
| [常驻终端](#关掉-soloagent-照样在跑) | 关掉、更新甚至崩溃，agent 照样在跑 |
| [好用的终端](#好用的终端) | 命令块、输入编辑器、边输边补全、用一句话要命令 |
| [并行任务](#好几个-agent-同时干活) | 一个任务一个 worktree，几个 agent 互不干扰 |

### 终端和编辑器分屏

终端占满整个窗口；打开一个文件，编辑器就从右边出现，关掉最后一个文件又收回去。中间的分隔线可以拖动，比例会记住。左边一排工具按钮打开文件树、Git、大纲等侧栏。

<!-- ![终端和编辑器分屏](docs/media/split.png) -->
> **截图（待补）** `split.png`：左边 agent 终端在工作，右边编辑器打开着它刚改的文件。

### 工作区标签：按项目分开

每个项目文件夹是一个工作区，标题栏上一个标签，里面有它自己的终端、agent、编辑器和文件树；切换标签，这些一起换。标签上统计这个项目里有几个 agent 在工作、在等你、做完待看、出了错，后台项目有 agent 在等你也看得到；标签菜单列出它的终端，点一下直接跳过去。终端跑到了别的项目的目录里，标签上会标出来，并提示移过去。`Ctrl+Alt+1…9` 切换，重启后所有标签都回来。

<!-- ![工作区标签](docs/media/workspaces.png) -->
> **截图（待补）** `workspaces.png`：标题栏三四个工作区标签带状态计数，其中一个标签的菜单展开着。

### agent 状态一眼看清

运行 agent 的终端，标签上挂着红绿灯一样的状态点：蓝色在工作，黄色等你回复，绿色做完一轮待你看，红色出错；后面是上下文用了多少。悬停能看到状态、模型、上下文、目录和分支，以及这一轮和整个会话的 token。Solo 在后台时，agent 等你回复或做完一段较长的活，会发桌面通知。Claude Code、Codex、Copilot CLI 通过 Solo 为这次会话加的 hooks 准确上报（不改它们的配置），其他 agent 按各自的屏幕规则识别。

<!-- ![agent 状态](docs/media/status.png) -->
> **截图（待补）** `status.png`：几个终端标签分别处于工作中、等你回复、完成待看，带上下文百分比；一个标签的悬停卡片展开着。

### 按轮审阅和回滚

agent 每做完一轮，终端标签上出现"N 个文件已修改"。点开就是这一轮的全部改动：逐块留下或撤回，在代码行上写意见，一次发回给 agent。Solo 还会告诉你这一轮**新增**了几个错误和警告、修掉了几个。可以回滚到这一轮之前，回滚本身也能撤销；快照存在 Solo 自己的目录里，不碰你的分支、暂存区和 stash。[详细 →](docs/site/zh/features/review.md)

<!-- ![按轮审阅](docs/media/review.png) -->
> **截图（待补）** `review.png`：这一轮 2–3 个文件的改动，其中一块已撤回，顶部显示错误增减。

### 完整的代码编辑器

编辑器就是 Zed：语言服务、跳到定义、查找引用、全项目搜索、多光标、Git 面板和 diff 都能用。审阅 agent 的改动时也一样能跳转和搜索。大仓库里语言服务默认更省资源，状态栏显示它们占了多少内存，还可以按项目暂停后台分析或关掉。

<!-- ![代码编辑器](docs/media/editor.png) -->
> **截图（待补）** `editor.png`：编辑器里一个函数的"查找引用"结果，旁边是 Git 面板。

### 终端里的链接一点就开

终端输出里的文件路径，点一下就打开：代码在编辑器里，Markdown 在渲染好的预览里（Mermaid 图和图片都显示），图片在图片查看器里。agent 说"改了 src/app.ts:42"，点一下就跳到那一行；输出里只有行号（比如 `(:120)`）时，Solo 会到前面提到的文件里找有这一行的那个打开。

<!-- ![终端里的链接](docs/media/links.gif) -->
> **截图（待补）** `links.gif`：鼠标点 agent 输出里的一个路径，右边编辑器打开对应文件。

### Mermaid 图

agent 输出里的 `mermaid` 代码块，点 `mermaid` 那一行就能看图；屏幕上只显示了一部分时，Solo 从 agent 的会话记录里取完整内容。Markdown 预览里的 Mermaid 图也直接渲染。

<!-- ![Mermaid 图](docs/media/mermaid.png) -->
> **截图（待补）** `mermaid.png`：左边终端里 agent 输出的 mermaid 代码，右边渲染出来的图。

### 文档审阅

agent 写的计划和设计文档在预览里打开，选中文字写批注，一次发回给 agent；文档再被改时，Solo 告诉你改了哪几块。[详细 →](docs/site/zh/features/documents.md)

<!-- ![文档审阅](docs/media/documents.png) -->
> **截图（待补）** `documents.png`：一份计划文档的预览，一段文字上挂着批注。

### 关掉 Solo，agent 照样在跑

每个终端的 shell 跑在一个小的后台进程里。退出 Solo、更新重启，甚至 Solo 崩溃，正在运行的 agent 和编译都会继续；Solo 再打开时自动连回去，期间的输出也都在。空闲的 shell 随 Solo 一起结束。没能保留下来的 agent，重启后会提示接着上次的会话。

### 好用的终端

- **命令块**：每条命令和输出是一块，失败的标红，可以复制、重跑，或者一键把出错的部分发给 agent。
- **输入编辑器**：命令在底部的真编辑器里输入，多光标、撤销、换行都能用。
- **补全和历史**：边输边弹的补全（约 500 个命令的子命令、选项和参数值，另有 PowerShell 自己的补全），历史建议和模糊搜索。
- **`# 想做的事`**：让你自己的 Claude Code 或 Codex 给出命令（生成时关掉了它们的工具），你确认后再运行。

[详细 →](docs/site/zh/features/terminal.md)

<!-- ![好用的终端](docs/media/terminal.png) -->
> **截图（待补）** `terminal.png`：几个命令块（其中一个失败标红）和正在弹出的补全。

### 好几个 agent 同时干活

一个任务一个 git worktree 和分支，互不干扰，做完合并、开 PR 或丢弃；工作区里有几个仓库时，一个任务可以横跨它们。终端里的 agent 也能用 `solo-cli agent` 查看、等待别的 agent，给它们发消息，或者新开任务。[详细 →](docs/site/zh/features/parallel.md)

## 支持的 agent

| agent | 状态从哪里来 | 按轮审阅 | 重启后接着用 |
|---|---|---|---|
| Claude Code、Codex、Copilot CLI | Solo 加的 hooks，准确上报 | 支持 | 接最近一次会话 |
| OpenCode | Solo 加的插件，准确上报 | 支持 | 接回原来的会话 |
| Gemini CLI、Cursor CLI | 读屏幕 | 支持 | 接最近一次会话 |
| Amp、Qwen Code、Kimi、Cline 等 16 种 | 读屏幕 | 支持 | 不支持 |
| Aider 和用 `agent_commands` 加的程序 | 终端输出 | 不支持 | Aider 恢复聊天记录，其他不支持 |
| 其他程序 | [状态协议](docs/agent-status-protocol.md) | 支持 | 不支持 |

完整列表见[功能参考](docs/reference.md#supported-agents)。

## 安装

Solo 目前只有 Windows x64 版。两种方式都只为当前用户安装，不需要管理员权限，默认装到 `%LOCALAPPDATA%\Programs\Solo`。

**方式一：安装包（推荐）**

1. 从 [Releases](https://github.com/lucas9801/solo/releases/latest) 下载 `Solo-Setup-<版本>.exe`。
2. 运行安装包（有中文和英文界面）。它会创建开始菜单项，桌面图标可选（默认不勾），在资源管理器右键菜单里加上"通过 Solo 打开"（默认勾选，Windows 11 上在"显示更多选项"里），并在"设置 → 应用"里加上卸载入口。
3. 安装包还没有代码签名（见下面的[代码签名政策](#code-signing-policy)），第一次运行时 Windows 可能提示"已保护你的电脑"，点"更多信息 → 仍要运行"。

**方式二：便携 zip**

1. 下载 `Solo-<版本>-windows-x64.zip` 并解压。
2. 直接运行里面的 `solo.exe`；或者在解压出的文件夹里运行下面的命令，把它装到 `%LOCALAPPDATA%\Programs\Solo`，创建开始菜单和桌面快捷方式，并在右键菜单里加上"通过 Solo 打开"（不要右键菜单就加 `-NoContextMenu`）：

   ```powershell
   powershell -ExecutionPolicy Bypass -File install.ps1
   ```

**开始用**：打开 Solo，打开一个项目文件夹，在终端里运行 `claude`、`codex` 或你常用的 agent（agent 要你自己先装好）。

**更新**：Solo 启动 30 秒后检查一次 GitHub Releases，之后每 6 小时一次。有新版本时弹出通知，你同意了才会更新；有 agent 在跑时可以选"退出时安装"。不想自动检查，在设置里写 `"auto_update": false`。

**卸载**：安装包版在"设置 → 应用"里卸载；zip 版运行 `install.ps1 -Uninstall`（命令见[用户指南](docs/guide/zh-CN.md#4-卸载与彻底清除)）。卸载不会删除设置和数据，彻底清除的方法也在那一节。

## 从源码编译

**需要准备**（Windows x64）：

- Git。
- Rust：用 [rustup](https://rustup.rs) 安装，也可以运行 `script\install-rustup.ps1`。仓库的 `rust-toolchain.toml` 固定了编译器版本（1.98.1），第一次构建时 rustup 会自动装上。
- Visual Studio 2022 或更新版本（或只装 Build Tools 加"使用 C++ 的桌面开发"工作负载），勾上这些组件：**MSVC x64/x86 生成工具**、**MSVC x64/x86 Spectre 缓解库**、**Windows SDK（至少 10.0.20348.0）**、**CMake**（"用于 Windows 的 C++ CMake 工具"）。只装 Build Tools 时，要在开始菜单里的"Developer PowerShell"里编译。组件 ID 和常见问题见 [Zed 的 Windows 构建说明](https://github.com/zed-industries/zed/blob/main/docs/src/development/windows.md)。
- 要打包安装程序，还需要 [Inno Setup 6](https://jrsoftware.org/isinfo.php)（`winget install JRSoftware.InnoSetup`）。
- 磁盘空间：至少留 40 GB。实测一次发行构建占 14 GB，Rust 工具链和 cargo 下载的源码约 6 GB；调试构建的 `target\debug` 会随着反复编译越积越大。测法见[开发与发布](docs/development.md#notes-for-local-builds)。

**开发构建**：

```powershell
git clone https://github.com/lucas9801/solo
cd solo
cargo run -p zed        # 调试版，生成并运行 target\debug\solo.exe
```

这样只编译 `solo.exe`。agent hooks 用的 `solo-agent-hook.exe`、让终端在 Solo 关掉后继续跑的 `solo-terminal-host.exe` 和 `solo-cli.exe` 要另外编译，见[开发与发布](docs/development.md#notes-for-local-builds)；没有它们 Solo 也能用，只是这几项退回到慢一些或不保留的做法。

**发行构建和打包**：

```powershell
script\package-solo.ps1   # 优化构建，输出 dist\Solo-<版本>-windows-x64.zip
```

装了 Inno Setup 6（或者用 `-Iscc <ISCC.exe 的路径>` 指定）时，还会生成安装包 `dist\Solo-Setup-<版本>.exe`。脚本会先生成依赖的许可清单（需要时自动安装 cargo-about）。可选参数：`-TargetDir`（cargo 输出目录）、`-OutDir`（输出目录，默认 `dist`）、`-SignCommand`（签名命令）。

**注意**：

- 第一次构建要下载依赖、编译一千多个 crate，时间较长（几十分钟到几个小时，看机器）。编译时会同时跑很多个 rustc，每个可能占到 1 GB 以上；内存紧张时用 `cargo run -j 4 -p zed` 或环境变量 `CARGO_BUILD_JOBS` 限制并发。
- 构建时会自动从 GitHub 下载预编译的 WebRTC（约 93 MB），需要能访问 GitHub；不想每次全新构建都下载，可以先下好再设 `LK_CUSTOM_WEBRTC`，见[开发与发布](docs/development.md#notes-for-local-builds)。
- 正式版本由 GitHub Actions 构建和打包，本地构建只用来开发和测试。发版流程（维护者用）见[开发与发布](docs/development.md)。

## 现在的状态

- **只有 Windows 版。** 安装包还没有代码签名，第一次运行时 Windows 可能提示"已保护你的电脑"，点"更多信息 → 仍要运行"。
- **不用注册账号，不发送遥测。** 默认只会定期访问 GitHub 检查更新（可以关掉），其余网络访问都由你用到的功能触发，详见[隐私说明](docs/guide/zh-CN.md#9-隐私与网络)。
- **开源**，GPL-3.0。

## Code signing policy

Free code signing provided by [SignPath.io](https://about.signpath.io),
certificate by [SignPath Foundation](https://signpath.org) — once Solo's
application is approved; until then releases are unsigned. What is signed, who
approves it, and what Solo sends over the network:
[Code signing policy / 代码签名政策](docs/code-signing-policy.md). Every file
in the packages and its license: [package contents](docs/package-contents.md).

免费的代码签名由 SignPath.io 提供，证书来自 SignPath Foundation——要等 Solo 的申请通过，在那之前发布的版本没有签名。签什么、谁批准、Solo 会访问哪些网络，见上面的代码签名政策。

## 更多文档

- [用户指南](docs/guide/zh-CN.md)（[English](docs/guide/en.md)）：所有功能和设置的说明
- [功能参考（英文）](docs/reference.md)：完整功能清单、设置项、agent 支持表
- [agent 状态协议](docs/agent-status-protocol.md)、[给 agent 用的命令行](docs/agent-cli.md)
- [开发与发布](docs/development.md)：构建细节和发版流程

## 致谢与许可

Solo 基于 Zed Industries 的 [Zed](https://github.com/zed-industries/zed)。和 Zed 一样采用 GPL-3.0-or-later 许可（见 `LICENSE-GPL`）；部分 Zed crate 是 Apache-2.0（见 `LICENSE-APACHE` 和各 crate 的许可文件）。

内置的第三方代码，本地改动都标有 `SOLO:`（见 [`vendor/README.md`](vendor/README.md)）：

- [alacritty_terminal](https://github.com/alacritty/alacritty)（Apache-2.0），来自 Zed 的分支
- [vte](https://github.com/alacritty/vte)（Apache-2.0 / MIT）
- [Warp 的 command signatures](https://github.com/warpdotdev/command-signatures)（MIT，很多规格改编自 Fig，同为 MIT）
- [Herdr 的 agent 屏幕识别规则](https://github.com/herdrdev/herdr)（Apache-2.0，未修改）

Solo 与 Zed Industries、Warp 没有关联。没有使用 Warp 终端应用的代码（AGPL），相关功能是重新实现的。
