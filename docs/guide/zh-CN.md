# Solo 用户指南

[English](en.md)

## 1. Solo 是什么

Solo 是一个以终端为中心的编辑器，基于 [Zed](https://github.com/zed-industries/zed) 修改而来。终端占据整个窗口，代码编辑器在需要时从右侧打开。它是为在终端里使用 AI 智能体（Claude Code、Codex 等）设计的：你能看到智能体在做什么，按轮审阅它改了什么，也可以让多个智能体在各自的 git worktree 里并行工作。

## 2. 安装

两种方式都只为当前用户安装，不需要管理员权限，默认装到 `%LOCALAPPDATA%\Programs\Solo`。

- **安装包** `Solo-Setup-<版本>.exe`：安装界面有中文和英文，会创建开始菜单项，桌面图标可选（默认不勾选），并在“设置 → 应用”里加上卸载入口。
- **便携 zip** `Solo-<版本>-windows-x64.zip`：解压后可以直接运行 `solo.exe`；也可以运行

  ```powershell
  powershell -ExecutionPolicy Bypass -File install.ps1
  ```

  这会把文件复制到 `%LOCALAPPDATA%\Programs\Solo`，并创建开始菜单和桌面快捷方式。

安装目录里有 `solo.exe`、`solo-agent-hook.exe`（Claude Code 用它上报状态）、`solo-update-helper.exe`（安装更新用）、`OpenConsole.exe` 和 `conpty.dll`（终端运行时），以及几份许可证文件。

## 3. 更新

- Solo 启动 30 秒后检查一次 [GitHub Releases](https://github.com/lucas9801/solo/releases)，之后每 6 小时检查一次。只有稳定版会检查。
- 有新版本时会弹出通知，可以选“更新并重启”、“跳过此版本”或“查看发布说明”。Solo 不会在你没同意的情况下安装任何东西。
- 选了更新以后：安装包会下载到 `%TEMP%\solo-update\`，Solo 像正常退出一样关闭窗口（未保存的文件会询问你），然后由 `solo-update-helper.exe` 等 Solo 退出，在原来的目录静默运行安装包，装完再启动 Solo。安装日志在 `%TEMP%\solo-update\install.log`，helper 的错误记录在同一个目录的 `update-helper.log`。
- 要关闭自动检查，在设置里写 `"auto_update": false`。命令面板里的 **Check for Updates（检查更新）** 不受这个设置影响，总会检查。

## 4. 卸载与彻底清除

- 安装包版：在“设置 → 应用”里卸载 Solo。
- zip 版：

  ```powershell
  powershell -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\Programs\Solo\install.ps1" -Uninstall
  ```

**两种方式都只删程序本身，不删你的设置和数据。** 要彻底清除，先关掉 Solo，再运行：

```powershell
Remove-Item -Recurse -Force "$env:APPDATA\Solo", "$env:LOCALAPPDATA\Solo", `
  "$env:TEMP\solo-shell-integration", "$env:TEMP\solo-update" -ErrorAction SilentlyContinue
```

另外还有几样东西不在上面这些目录里，需要的话手动处理：

- 项目里的 `.solo` 文件夹（项目设置、任务、调试配置）；
- 智能体任务留下的 git worktree 和分支（默认在项目旁边的 `../worktrees/` 里）。最好先在 Solo 的“Manage Agent Tasks”里删除；也可以用 `git worktree remove` 加 `git branch -D` 删除。

Solo 不会修改你的 Claude Code 配置（见第 8 节），所以那边没有需要清理的。

## 5. 数据和设置放在哪里

| 位置 | 内容 |
| --- | --- |
| `%APPDATA%\Solo` | 设置目录：`settings.json`、`keymap.json`、`tasks.json`、`debug.json`、`AGENTS.md`、`workflows.json`（保存的命令） |
| `%LOCALAPPDATA%\Solo` | 数据目录：`logs\`（日志）、`db\`（工作区和会话状态）、`extensions\`、`languages\`（自动下载的语言服务器）、`node\`、`external_agents\`、`terminal_sessions\`（终端内容恢复，30 天后清理）、`agent_turns\`（智能体轮次快照）、`hang_traces\` |
| `%TEMP%\solo-shell-integration` | shell 集成脚本和 Claude Code 的 hook 设置文件 |
| `%TEMP%\solo-update` | 下载的更新安装包和安装日志 |
| 项目里的 `.solo\` | 项目设置、任务和调试配置（也会读 Zed 的 `.zed\`） |

Solo 的目录和 Zed 的完全分开，同时装了两者也不会互相影响。

## 6. 第一次使用：在 Solo 终端里跑智能体

打开一个项目文件夹，在终端里运行 `claude`（或者 `codex`、`gemini` 等），Solo 会自动加上以下功能。

### 标签页状态
终端标签会显示智能体的状态：**工作中**（转圈）、**等你输入**、**空闲**、**完成**、**失败**。标签标题会显示智能体名和会话标题；鼠标悬停时会显示 token 用量和上下文占用。Solo 在后台时，智能体等你输入或干完一段较长的活，会发桌面通知。

### 按轮审阅（仅 Claude Code）
Claude Code 每开始和结束一轮，Solo 都会给工作目录拍一个快照。一轮结束后，标签上会出现 **“N files changed”** 按钮（命令：`terminal: review agent turn`，中文界面显示为“终端: 审阅智能体轮次”）。在审阅界面里你可以：

- 对每个改动块（hunk）选择 **保留** 或 **拒绝**，或者用 **全部保留 / 全部拒绝**；
- 在代码行上写评论，再一起发回给智能体；
- **回滚**，把工作区恢复到某一轮之前。

快照存在 Solo 自己的目录里，不会改动你的分支、HEAD、暂存区、stash，也不会往你的 git 对象库里写东西。被 `.gitignore` 忽略的文件不在快照里。

### 时间线（仅 Claude Code）
命令 `terminal: show agent timeline`（“显示智能体时间线”）会在终端旁边打开这次会话的时间线：每一轮的提示词、读过和改过的文件、跑过的命令和最后的回答。内容读自 Claude Code 自己的对话记录（`~/.claude/projects/...`），Solo 只读不写。如果 Solo 判断不出是哪个会话，用 `terminal: choose agent session` 手动选。

### 智能体任务（git worktree）
命令面板里运行 **New Agent Task（新建智能体任务）** 并给任务起个名字。Solo 会新建一个分支和对应的 git worktree（位置由 `git.worktree_directory` 决定，默认是 `../worktrees`），然后在里面打开一个终端，运行 `terminal.agent_task_command`（默认是 `claude`）。在 **Manage Agent Tasks（管理智能体任务）** 里可以打开任务的终端、查看 diff、合并回原分支或删除任务。任务信息记在仓库的 git 配置里，重启后还在。只支持本地项目。

### 智能体看板
左侧 dock 里的看板（`agent board: toggle focus`）列出所有跑过智能体的终端和所有智能体任务，显示每个的状态、分支、上一轮的改动、运行时长和最后一句话。需要你输入的排在最前面，点一下就跳到对应终端。

### 标题栏工作区标签
每个项目文件夹是一个工作区，在标题栏占一个标签，各有自己的终端、智能体、编辑器和文件树。标签上会统计这个工作区里正在工作、等你输入和失败的智能体数量，后台有智能体在等你也能看到。

- `+` 或打开文件夹：新增工作区；
- `ctrl-alt-1` … `ctrl-alt-9`：切换到第 N 个；`ctrl-alt-pagedown` / `ctrl-alt-pageup`：下一个 / 上一个；
- 标签菜单（箭头或右键）列出这个工作区的终端，可以直接跳过去；
- 重启后只加载当前工作区，其他的在第一次切换过去时才加载；
- 在终端里 `cd` 到另一个标签的项目（它的文件夹，或同一个 git 仓库）时，Solo 会提示把终端移到那个标签；选“留在这里”后，这个终端不再为那个标签提示；
- 这个版本的 Solo 第一次启动时，有标签之前留下的终端会按 git 仓库移到对应的标签，没有的话在后台打开一个。agent 任务的 worktree 算作它所属的仓库。开着 `follow_working_directory` 时这两项都不生效。

### 把东西发给智能体
- 编辑器里按 `ctrl-alt-k` 发送选中内容或光标位置；`ctrl-alt-shift-k` 可以先选目标终端；`ctrl-alt-shift-m` 发送当前文件的诊断信息。
- 终端里选中一个命令块（`ctrl-up/down` 或点左侧边栏），按 `alt-enter` 把命令和输出发给另一个终端里的智能体。
- 在 Claude Code 终端的菜单里选“启用远程控制”，Solo 会往 Claude Code 里输入 `/remote-control`，之后可以用手机上的 Claude App 继续这个会话。

### 其他
- 关闭正在运行智能体的终端、窗口或 Solo 前会先确认（`terminal.confirm_close`，默认 `"agents"`）。
- 重启后，之前跑着智能体的终端会提示是否继续上次的会话（例如 `claude --continue`）。

## 7. 支持哪些智能体、支持到什么程度

| 功能 | Claude Code | Codex、Copilot CLI | gemini、opencode、qwen、amp、cursor-agent 等 | aider、`agent_commands` 里加的程序 |
| --- | --- | --- | --- | --- |
| 被识别为智能体（状态、关闭确认、发送命令块） | 是 | 是 | 是 | 是 |
| 状态来源 | hooks 上报，准确 | hooks 上报，准确（Codex 需要先信任一次） | 按各自的规则读屏幕，可能会慢一点 | 从终端输出推断 |
| 按轮审阅、回滚 | 是 | 是 | 是，按屏幕状态划分轮次 | 否 |
| 时间线、会话标题、token 用量 | 是 | 否 | 否 | 否 |
| 重启后继续会话 | 是（`--continue`） | 是（`codex resume --last`、`copilot --continue`） | 是（各自的 resume 参数；qwen 和 amp 不支持） | 否 |
| 智能体任务 | 默认命令就是 `claude` | 把 `agent_task_command` 改成对应程序即可 | 同左 | 同左 |

没有 hooks 的智能体，Solo 是这样判断状态的：输出里有转圈动画就是在工作，安静 3 秒左右算停止，响铃或发通知（OSC 9/777）就当作需要你输入。

要让 Solo 识别别的程序，在设置里加：

```json
{ "terminal": { "agent_commands": ["my-agent"] } }
```

## 8. Solo 对你的系统做了什么

- **Claude Code hooks 只对单次会话生效。** 在接入了 shell 集成的终端里，Solo 会定义一个 `claude` 包装函数，启动时给 claude 加上 `--settings %TEMP%\solo-shell-integration\claude-settings.json`。Claude Code 会把这份设置叠加在你自己的设置上，**你的 `~/.claude/settings.json` 和你自己的 hooks 都不会被修改**。如果你已经有自己的 `claude` 函数或别名，Solo 不会覆盖它。运行 `claude mcp`、`claude update` 这类子命令，或者你自己传了 `--settings` 时，命令会原样执行。关闭方法：`"terminal": {"claude_code_hooks": false}`，对新开的终端生效。
- **Codex hooks 也只对单次会话生效。** Solo 的 `codex` 包装函数会加上 `-c hooks.<事件>=...` 参数，Codex 把它们叠加在你自己的配置上，只对这次会话有效；**Solo 不会修改你的 `~/.codex/config.toml` 和 `hooks.json`**。Codex 只运行你信任过的 hooks，所以第一次启动时它会问你：选择信任后 Codex 会记住（由 Codex 写进它自己的配置）。这些 hooks 运行的是 `solo-agent-hook.exe`。`codex login`、`codex mcp` 这类不启动会话的子命令原样执行；只有 `codex.cmd`、旁边没有 `codex.ps1` 或 `codex.exe` 时也原样执行。关闭方法：`"terminal": {"codex_hooks": false}`，对新开的终端生效。
- **Copilot CLI hooks 也只对单次会话生效。** Solo 会把带 hooks 的一个小插件写到 `%TEMP%\solo-shell-integration\copilot-plugin`，`copilot` 包装函数用 `--plugin-dir` 加载它；**不会往 `~/.copilot` 里安装任何东西**。Copilot 用 PowerShell 运行这些 hooks（hooks 运行的是 `solo-agent-hook.exe`）。`copilot login`、`copilot plugin` 这类不启动会话的子命令原样执行。关闭方法：`"terminal": {"copilot_hooks": false}`，对新开的终端生效。
- **shell 集成**：PowerShell、bash、zsh、fish 在没有自定义启动参数时会加载 Solo 的集成脚本，这些脚本写在 `%TEMP%\solo-shell-integration`。PowerShell 用 `-NoExit -Command` 读取并执行这个脚本文件，不用 `-ExecutionPolicy Bypass`，也不用 `-EncodedCommand`。脚本先加载你自己的 profile，再加载集成。关闭方法：`"terminal": {"shell_integration": false}`。
- **hooks 跑的程序**是安装目录里的 `solo-agent-hook.exe`。它从 stdin 读事件，往终端写一个转义序列，然后退出；它不联网，也不写文件。
- **轮次快照**存在 `%LOCALAPPDATA%\Solo\agent_turns`，不碰你仓库里的东西。
- **智能体任务**会在你的仓库里新建分支和 worktree，并往仓库的 git 配置里写 `branch.<分支>.solotask` / `solobase`。这些只在你新建任务时才会发生。
- **更新**：只有你点了更新才会安装，由 `solo-update-helper.exe` 在 Solo 退出后静默运行安装包。
- Solo 会读 Claude Code 的对话记录（`~/.claude/projects`），用来显示时间线和会话信息，只读不写。

## 9. 隐私与网络

**Solo 不发送遥测，也不上传崩溃报告。** 遥测的 `diagnostics` 和 `metrics` 默认都是关的；Solo 的构建没有配置崩溃上报地址，所以不会生成 minidump，也不会上传。Solo 启动时不会登录 Zed 账号；Zed 托管的模型、编辑预测、协作功能和 Zed 的自动更新都已关闭或隐藏。

**默认会访问的地址（不需要你做任何操作）：**

| 地址 | 用途 | 怎么关 |
| --- | --- | --- |
| `api.github.com` | 检查 Solo 更新（启动 30 秒后，之后每 6 小时） | `"auto_update": false` |

**只在你用到某个功能时才访问：**

- 下载更新安装包：`github.com` 和 GitHub 的下载 CDN；
- 打开某种语言的文件时自动下载语言服务器：GitHub Releases、npm 源；系统里没有合适的 Node.js 时还会访问 `nodejs.org`；
- 你自己配置的 AI 服务（Anthropic、OpenAI、Ollama 等）；
- git blame 和提交详情里的头像：`avatars.githubusercontent.com`、GitHub API；
- 浏览、安装扩展，以及检查已装扩展的更新（没装扩展时不检查）：`api.zed.dev`，也就是 Zed 的扩展服务；SSH 远程开发时下载远程服务端：`cloud.zed.dev`；
- Solo 不会访问 Zed 的其他服务：帮助链接打开的是这份指南（Solo 自带一份，写到它的数据目录里，没网也能打开）；连接需要 OAuth 登录的 MCP 服务器时，Solo 只用动态注册，不借用 Zed 的客户端身份（只支持客户端元数据文档的服务器因此无法登录）；Solo 自带的 agent 面板在 Windows 上不提供 WSL 沙箱，因为它需要从 zed.dev 下载 Linux 版的 Zed。
- 打开 ACP 智能体注册表，或使用从注册表配置的智能体：`cdn.agentclientprotocol.com` 和注册表里列出的图标地址；
- 在终端里运行的智能体（Claude Code、Codex 等）自己的网络访问，不经过 Solo。

## 10. 故障排查

### 被安全软件拦截
**症状**：终端打不开、一打开就关、提示符一直不出现；Claude Code 的状态一直不更新；更新装不上。

**原因**：Solo 会用 PowerShell 执行 `%TEMP%` 里的脚本，Claude Code 每个事件都会运行 `solo-agent-hook.exe`，更新时 helper 会静默运行安装包。有些安全软件会把这些行为当成可疑操作。

**处理**：

1. 把以下位置加入安全软件的信任或白名单：安装目录 `%LOCALAPPDATA%\Programs\Solo`（里面有 solo.exe、solo-agent-hook.exe、solo-update-helper.exe、OpenConsole.exe、conpty.dll），以及 `%TEMP%\solo-shell-integration` 和 `%TEMP%\solo-update`。
2. 想确认是不是被拦了：在设置里写 `"terminal": {"shell_integration": false}`，再新开一个终端。如果这样能用，就说明拦的是集成脚本。
3. 向厂商报告误报：例如 360 可以通过开放平台 open.soft.360.cn 提交（待核实）；其他厂商一般也有误报申诉入口。提交时附上安装包和版本号。

### 终端打不开
- 先看日志（见下文），搜索 `shell integration` 或 `failed to`。
- 如果你给 shell 设置了自定义启动参数，Solo 不会接入集成；这时命令块、输入编辑器和 Claude hooks 都不可用，但终端本身应该能打开。
- 安装目录缺了 `conpty.dll` 或 `OpenConsole.exe` 时，Solo 会退回使用系统自带的 ConPTY。

### 智能体状态不对
- 确认终端用的是 PowerShell、bash、zsh 或 fish，并且没有自定义启动参数；System32 下的 `bash.exe`（WSL）不支持。
- 如果你有自己的 `claude` 函数或别名，Solo 的包装函数不会生效，这时状态只能靠输出推断。
- 改了 `claude_code_hooks`、`codex_hooks` 或 `copilot_hooks` 以后要新开终端才生效。
- 在 Codex 启动时的 hooks 确认里信任 Solo 的 hooks 之前，Codex 的状态仍然是读屏幕得来的。
- `solo-agent-hook.exe` 被拦截或丢失时，Solo 会改用 PowerShell 脚本，每次上报都会变慢。
- 会话认错了：运行 `terminal: choose agent session` 手动选。
- 其他智能体的状态本来就是推断出来的，可能会慢一两秒。

### 日志和崩溃信息
- 日志：`%LOCALAPPDATA%\Solo\logs\Solo.log`，上一次运行的是 `Solo.log.old`。命令面板里的 `solo: open log` 打开日志，`solo: reveal log in file manager` 在资源管理器里显示。
- 卡顿 trace：`%LOCALAPPDATA%\Solo\hang_traces\`。
- 崩溃：Solo 默认不生成 minidump；崩溃前的信息一般在 `Solo.log.old` 里。需要 minidump 时，设置环境变量 `ZED_GENERATE_MINIDUMPS=1` 后再启动 Solo，文件会写到 logs 目录（具体文件名待核实）。
- 更新失败：看 `%TEMP%\solo-update\install.log` 和 `update-helper.log`。
- 提交问题：[github.com/lucas9801/solo/issues](https://github.com/lucas9801/solo/issues)，附上日志。

## 11. 常用设置速查

Solo 自己加的设置写在 `"terminal"` 下面：

| 设置 | 默认值 | 作用 |
| --- | --- | --- |
| `shell_integration` | `true` | 加载 shell 集成（命令块、输入编辑器、hooks 都依赖它） |
| `claude_code_hooks` | `true` | 通过 `--settings` 给 Claude Code 加状态 hooks |
| `codex_hooks` | `true` | 通过 `-c hooks...` 给 Codex 加状态 hooks |
| `copilot_hooks` | `true` | 通过 `--plugin-dir` 给 Copilot CLI 加状态 hooks |
| `agent_commands` | `[]` | 把其他程序也当作智能体 |
| `agent_task_command` | `"claude"` | 新建智能体任务时运行的命令 |
| `confirm_close` | `"agents"` | 关闭前确认：`agents` / `commands` / `never` |
| `follow_working_directory` | `false` | 文件树和搜索跟随当前终端所在的仓库 |
| `restore_contents` | `false` | 重启后恢复终端内容 |
| `open_changed_files` | `true` | 命令运行期间被修改的文件自动在编辑器里打开 |
| `input_editor` | `true` | 在底部用编辑器输入命令 |
| `notify_long_commands_after_seconds` | `10` | 长命令跑完后通知；设为 0 关闭 |
| `program_notifications` | `true` | 允许程序发桌面通知（OSC 9/777） |

不在 `"terminal"` 下的：`"ui_language"`（`"system"` / `"en"` / `"zh"`，也可以用环境变量 `SOLO_LANGUAGE` 覆盖）、`"auto_update"`、`"disable_ai"`。
