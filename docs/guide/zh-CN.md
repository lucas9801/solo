# Solo 用户指南

[English](en.md)

## 1. Solo 是什么

Solo 是一个以终端为中心的编辑器，基于 [Zed](https://github.com/zed-industries/zed) 修改而来。终端占据整个窗口，代码编辑器在需要时从右侧打开。它是为在终端里使用 AI agent（Claude Code、Codex 等）设计的：你能看到 agent 在做什么，按轮审阅它改了什么，也可以让多个 agent 在各自的 git worktree 里并行工作。

## 2. 安装

有两种装法，装一种就够了。

**GitHub 上的安装程序。** 从 [GitHub Releases](https://github.com/lucas9801/solo/releases/latest) 下载 `Solo-Setup-<版本>.exe`（同一页上的 `solo-remote-server-linux-*.gz` 是远程开发用的 Linux 服务端，不是安装程序）。

- 安装程序**没有签名**，所以运行时 Windows 的 SmartScreen 会提示“Windows 已保护你的电脑”：点“更多信息”，再点“仍要运行”。
- 可以**选择安装目录**（默认 `%LOCALAPPDATA%\Programs\Solo`，只为当前用户安装，不需要管理员权限）；选一个你有写权限的文件夹，比如 `D:\Apps\Solo`，不要选 `C:\Program Files`。
- 有开始菜单项、可选的桌面图标、资源管理器右键菜单里的“通过 Solo 打开”（在 Windows 11 的“显示更多选项”里，可以在安装时取消），以及 `solo://` 链接。
- **Solo 自己更新**：从 GitHub 下载新版本的安装程序，静默装到同一个目录（见第 3 节）。
- 用 2.4.2 及更早的安装程序装的 Solo 就是这一种，直接用新安装程序覆盖安装即可，设置和数据不变。以前的便携 zip 不再提供。

**Microsoft Store 的 Solo Code。** [下载 Solo Code 的安装程序](https://get.microsoft.com/installer/download/9NBW452JVWN6)，运行后直接为当前用户装好，不打开商店应用；链接下载不了时，打开 [Solo Code 的商店页面](https://apps.microsoft.com/detail/9NBW452JVWN6)安装。商店版由微软签名，通过商店更新，版本可能比 GitHub 晚一个。装好后有开始菜单项、可以在 Windows 命令行里用的 `solo` 命令，以及资源管理器右键菜单里的“通过 Solo Code 打开”（在 Windows 11 自己的菜单里，不只在“显示更多选项”里）。

想把 Solo Code 装到别的盘：在“设置 → 系统 → 存储 → 高级存储设置 → 保存新内容的地方”里把“新的应用将保存到”改成那个盘再安装，或者装好后在“设置 → 应用 → 已安装的应用”里点 Solo Code 的“…”选“移动”。（这些菜单名称是按微软的说明写的，没有实测。）

**右键菜单“通过 Solo Code 打开”**（GitHub 安装版叫“通过 Solo 打开”）：在文件、文件夹、文件夹空白处和驱动器上都有。打开文件夹时，Solo 没在运行就启动并打开它；已在运行就在现有窗口里加一个工作区标签并切过去，这个文件夹已经有标签时直接切到那个标签。打开文件时，放进包含它的那个标签，没有就放进当前标签，不会新开窗口。卸载时右键菜单会一起删掉。

两种安装都有 `solo.exe`、`solo-cli.exe`（agent 用的命令行）、`solo-agent-hook.exe`（Claude Code 用它上报状态）、`solo-update-helper.exe`、`solo-terminal-host.exe`（运行终端的 shell，让它能在 Solo 更新重启后继续跑）、`OpenConsole.exe` 和 `conpty.dll`（终端运行时），以及几份许可证文件。

## 3. 更新

- Solo 启动 30 秒后检查一次有没有新版本，之后每小时检查一次（只有稳定版会检查）：Solo Code 问 Microsoft Store；其他版本（GitHub 安装程序装的、从源码构建的）查 [GitHub Releases](https://github.com/lucas9801/solo/releases)。
- 有新版本时，标题栏最右边（窗口按钮左侧）出现“↑ 更新到 <版本>”按钮，不弹通知。点开是一句更新概要和“更新并重启”“退出时安装”“查看更新内容”“跳过此版本”；有 agent 在运行时会提示现在更新会结束哪些，并把“退出时安装”放在前面。下载进度、“退出时更新到 <版本>”和“更新失败”（菜单里写原因，可以“重试”）也显示在这个按钮上。跳过的版本不再提示，有更新的版本时按钮再出现。Solo 不会在你没同意的情况下安装任何东西。
- 从 Microsoft Store 装的 Solo Code 也在 Solo 里更新，用同一个按钮，不打开商店：商店允许时（商店里开着“自动更新应用”、网络不按流量计费）先在后台静默下载；“更新并重启”交给商店安装，装完 Windows 会重新打开 Solo。商店不允许静默安装时，会弹出商店的小确认框。
- **关于 Solo** 窗口显示版本、发布日期和提交号（点击可复制），是否已是最新，以及“检查更新”按钮；还有用户指南、本版本发行说明、GitHub 仓库和许可的链接。“复制”按钮复制版本、提交号和系统信息，方便报告问题。
- GitHub 安装版下载新版本的 `Solo-Setup-<版本>.exe`（按 GitHub 给出的 SHA-256 校验），等 Solo 退出后静默安装到原来的目录；“更新并重启”装完会重新打开 Solo。发布里没有安装程序时，按钮的菜单里只有“从 Microsoft Store 获取”（打开 Solo Code 的商店页面）和“跳过此版本”。
- 要关闭自动检查，在设置里写 `"auto_update": false`。命令面板里的 **Check for Updates（检查更新）** 不受这个设置影响，总会检查。

## 4. 卸载与彻底清除

- Solo Code：在“设置 → 应用”里卸载。
- GitHub 安装程序装的 Solo：在“设置 → 应用”里卸载 Solo。
- 用以前版本的 zip 装的 Solo：

  ```powershell
  powershell -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\Programs\Solo\install.ps1" -Uninstall
  ```


**卸载只删程序本身，不删你的设置和数据**（Windows 放在 Solo Code 包目录里的文件除外，见第 5 节）。要彻底清除，先关掉 Solo，再运行（第一行结束还在后台运行的终端）：

```powershell
Get-Process solo-terminal-host -ErrorAction SilentlyContinue | Stop-Process
Remove-Item -Recurse -Force "$env:APPDATA\Solo", "$env:LOCALAPPDATA\Solo", `
  "$env:TEMP\solo-shell-integration", "$env:TEMP\solo-update" -ErrorAction SilentlyContinue
```

另外还有几样东西不在上面这些目录里，需要的话手动处理：

- 项目里的 `.solo` 文件夹（项目设置、任务、调试配置）；
- agent 任务留下的 git worktree 和分支（默认在项目旁边的 `../worktrees/` 里）。最好先在 Solo 工作区标签菜单的“管理工作树…”里删除；也可以用 `git worktree remove` 加 `git branch -D` 删除。

Solo 不会修改你的 Claude Code 配置（见第 8 节），所以那边没有需要清理的。

## 5. 数据和设置放在哪里

| 位置 | 内容 |
| --- | --- |
| `%APPDATA%\Solo` | 设置目录：`settings.json`、`keymap.json`、`tasks.json`、`debug.json`、`AGENTS.md`、`workflows.json`（保存的命令） |
| `%LOCALAPPDATA%\Solo` | 数据目录：`logs\`（日志）、`db\`（工作区和会话状态）、`extensions\`、`languages\`（自动下载的语言服务器）、`node\`、`external_agents\`、`terminal_sessions\`（终端内容恢复，30 天后清理）、`agent_turns\`（agent 轮次快照）、`terminal-host\`（终端宿主进程的副本）、`hang_traces\` |
| `%LOCALAPPDATA%\Solo\browser` | 内置浏览器的 Cookie、网站数据和元素截图（`picks\`，保留一周） |
| `%LOCALAPPDATA%\Solo\solo-shell-integration` | shell 集成脚本，以及 agent 状态 hooks 用到的文件（Claude Code 的设置、Copilot 的插件、OpenCode 和 Pi 的插件等）。Solo Code 放在自己的包目录里。Solo 2.5.2 及更早版本用的是 `%TEMP%\solo-shell-integration`；这个文件夹还在时，Solo 会继续往里面写一份，供旧版 Solo 开的终端使用 |
| `%TEMP%\solo-update` | GitHub 安装版下载的更新安装程序和安装日志 |
| 项目里的 `.solo\` | 项目设置、任务和调试配置（也会读 Zed 的 `.zed\`） |

Solo 的目录和 Zed 的完全分开，同时装了两者也不会互相影响。

从商店装的 Solo Code：Windows 可能把 `%APPDATA%\Solo` 和 `%LOCALAPPDATA%\Solo` 里新建的文件改放到包自己的目录（`%LOCALAPPDATA%\Packages\lucas9801.SoloCode_*\LocalCache`），卸载 Solo Code 时这个目录会被删除。

## 6. 第一次使用：在 Solo 终端里跑 agent

打开一个项目文件夹，在终端里运行 `claude`（或者 `codex`、`gemini` 等），Solo 会自动加上以下功能。

### 标签页状态
终端标签会显示agent的图标，用它本来的颜色（Claude Code 是橙色，黑白标志用正文颜色；没有图标的显示名字首字母），右下角挂着红绿灯一样的状态点：红色带叉是**失败**，黄色带呼吸外圈是**等你回复**，绿色是**完成待看**（你没看着时做完了一轮），蓝色转动圆环是**工作中**，灰色空心圆是**空闲**，灰色短横是**已退出**。标题只显示会话名（没有会话名时显示agent名），后面是上下文圆环和百分比：灰色 < 80%，黄色 80–95%，红色 ≥ 95%。鼠标悬停时会显示一张卡片：状态、agent、模型和思考强度、上下文进度条、目录/分支/命令、本轮和本会话的 token 和用时，以及响应速度：最近一次请求的首字时间和输出速度，和它们的本会话平均。Solo 在后台时，agent需要你输入会发桌面通知。agent干完一段较长的活停下时的通知默认关闭，可以在 设置 > 终端 > 通知 里打开（`agent_stopped_notifications`）。

#### 首字、速度、本轮用时和思考强度
各个 agent 能显示什么；算不准的不显示：

| agent | 首字和速度 | 思考强度 | 来源 |
|---|---|---|---|
| Claude Code | 大多数请求（以普通文字开头的请求算不出） | 有 | 会话记录 |
| Codex | 有 | 有 | 它的遥测：Solo 启动的会话让它发给 Solo；思考强度来自会话文件 |
| GitHub Copilot CLI | 流式请求 | 无 | 它的遥测：Solo 让它写到这个终端的文件里 |
| OpenCode | 有（还没有用真实的 OpenCode 核对） | 消息里有 variant 时显示 | Solo 的插件 |
| 其他（Gemini CLI、aider 等） | 无 | 无 | |

本轮用时所有 agent 都有：从开始工作到停下，不算等你回复的时间。本会话用时是 Solo 看到 agent 启动以后各轮之和；接着以前的会话继续时不显示。你自己配置了遥测时，Solo 不改 Codex 和 Copilot 的遥测设置。Codex 的遥测只发到本机（Solo 只在内存里保留这些数字和它的补丁改了哪些文件）；这些终端里 `OTEL_BLRP_SCHEDULE_DELAY` 默认是 200（除非你自己设了），所以 Codex 每 200 毫秒而不是每秒发一次。

### 待审改动
agent 每开始和结束一轮，Solo 都会给工作目录拍一个快照（支持哪些 agent 见第 7 节）。agent 改的东西在你认可之前都是待审的：Solo 给每个文件记下你上次认可时的样子，标签上显示 **“N 个文件待审”** 按钮（命令：`terminal: review agent turn`，中文界面显示为“终端: 审阅 agent 轮次”）。审阅页拿每个文件和你认可的样子比，几轮的改动一起显示，每块标出是哪一轮改的；来自不止一轮时可以只看某一轮的。在审阅界面里你可以：

- 对每个改动块（hunk）选择 **保留** 或 **拒绝**，或者用 **全部保留 / 全部拒绝**；后面的轮次又改了同一个文件也照样能拒绝。拒绝 agent 新建的文件时，文件会被移到回收站；之前就存在的文件（例如当时被 `.gitignore` 忽略）不会被删除。文件在编辑器里有没保存的改动时不会拒绝；
- 在代码行上写评论，连同拒绝了哪些合成一条消息发给 agent（粘贴进它的输入框，不会自动回车）；
- **轮次**：**查看**只读地显示某一轮自己改了什么，**回滚**把工作区恢复到某一轮之前；
- 看这一轮对语言服务器诊断的影响：在改过的文件里新增了哪些错误和警告、修掉了几个错误（点一下跳到那里）；
- 像在编辑器里一样读代码：跳到定义、查引用、搜索都能用，每个改动块上方写着它所在的函数或类型。

正在进行的这一轮动过的文件先锁住，这一轮结束后再审。没处理的改动到下一轮结束时自动算保留（审阅页开着时先不算）。你自己改的、和 agent 的改动不冲突的部分，以及提交进 git 的内容，不算待审。待审改动在重启后还在；Solo 关着时 agent 在照样运行的终端里改的东西也算待审，标成“Solo 关闭期间”。关掉 agent 的终端，待审改动随之去掉。

快照存在 Solo 自己的目录里，不会改动你的分支、HEAD、暂存区、stash，也不会往你的 git 对象库里写东西。被 `.gitignore` 忽略的文件不在快照里。网络驱动器上的文件夹不拍快照。回滚时不动链接文件夹（目录联接、符号链接）里的文件，因为它们可能在项目之外；确认页显示之后如果又有文件改动，回滚会停下来让你重新确认。回滚（或撤销回滚）中途失败时（例如文件被别的程序打开着），页面会说明，并保留 **撤销回滚**：关掉那个程序后点它，所有文件会恢复到回滚之前的样子。

agent 报告自己改了哪些文件时（Claude Code 会，通过 Solo 给它的 Edit、Write 等工具加的 hook；Codex 用 `apply_patch` 做的修改来自 Solo 让 Codex 发给它的遥测数据，只对这次会话有效；如果你自己的 Codex 配置里设置了 `[otel]`，会替换掉 Solo 的，这时可以设置 `"terminal": {"codex_edit_hooks": true}`，改用 hook 报告，在 Windows 上每次补丁大约多花一秒），一轮只算它报告的文件：这期间你自己或同一仓库里别的 agent 改的文件不算进来，**全部拒绝**也不会动它们。页面会列出这一轮期间改动过的其他文件，包括 agent 自己运行的命令改的。不报告改动的 agent，一轮算文件夹里所有改动过的文件，页面会说明其中可能有不是它改的。

拍不了快照时（例如不是 git 仓库、文件又超过 20000 个），Solo 会记下这一轮里项目中被改动的文件（只算 agent 工作目录下、没被忽略的文件），标签上照样显示 **“N 个文件已修改”**；点开是一个文件列表，点文件在编辑器里打开，列表上方写明为什么这一轮没有差异和回滚。这种情况下只能看到项目里打开的文件夹里的改动，别的程序同时改的文件也会算进去。

Solo 不会在 agent 工作时自动打开它改的文件。

### 时间线（Claude Code、Codex）
命令 `terminal: show agent timeline`（“显示 agent 时间线”）会在终端旁边打开这次会话的时间线：每一轮的提示词、读过和改过的文件、跑过的命令和最后的回答。内容读自 agent 自己的会话记录（Claude Code 的 `~/.claude/projects/...`，Codex 的 `~/.codex/sessions/...`），Solo 只读不写。如果 Solo 判断不出是哪个会话，用 `terminal: choose agent session` 手动选。

### agent 任务（git worktree）
命令面板里运行 **新建 agent 任务**（`agent task: new agent task`）并给任务起个名字。Solo 会新建一个分支和对应的 git worktree（位置由 `git.worktree_directory` 决定，默认是 `../worktrees`），然后在里面打开一个终端，运行 `terminal.agent_task_command`（默认是 `claude`）。在工作区标签菜单的 **管理工作树…**（命令 `agent task: manage agent tasks`）里可以打开任务的终端、查看 diff、合并回原分支、开 PR 或删除任务。任务信息记在仓库的 git 配置里，重启后还在。只支持本地项目。

终端栏 **+** 菜单（以及工作区标签菜单）里的 **在新工作树中新建终端…** 打开同一个弹窗：填名称（下面显示分支名 `agent/<名称>`），再选在终端里运行什么：不运行（只开 shell）、Claude Code 或 Codex（只列 `PATH` 里找得到的；默认按 `terminal.agent_task_command`）。同一个菜单里的 **运行工作流…**（Alt+Shift+T）列出你的工作流和项目任务：选工作流会新开一个终端，把命令填进输入框并选中第一个 `{{参数}}`，不自动运行；选项目任务和"启动任务"一样运行。

- **跨仓库：** 工作区里有来自多个 git 仓库的文件夹时，新建任务时可以勾选同时在其他仓库里建。每个仓库都在同名的新分支上建一个工作树，放在以任务命名的同一个文件夹里，agent 在这个文件夹里启动，能看到所有仓库。每个仓库的部分分别合并、开 PR 或删除，最后一个删除时文件夹也一起删掉。这类任务不运行 `.solo/worktrees.json` 里的 setup 命令。
- **信任：** 仓库 `.solo/worktrees.json` 里的 `setup`、`teardown` 命令和 `env` 变量，只有在你信任这个项目（Zed 的受限模式）之后才会使用；在此之前新建任务时不使用它们。
- **配置文件：** 这份配置以前叫 `.solo/tasks.json`，和 Zed 的项目任务文件同名。旧文件如果是 JSON 对象（这份配置），Solo 第一次读到时会把它改名为 `.solo/worktrees.json`；是 JSON 数组（项目任务）就不动。
- **提交：** 合并和开 PR 用的是任务的提交。任务有未提交的改动时，**让 agent 提交** 会把提交请求粘贴给任务的 agent，让它根据 diff 写提交信息再提交；在它的终端里按回车发送。

### agent 看板
左侧 dock 里的看板（`agent board: toggle focus`）列出所有跑过 agent 的终端和所有 agent 任务，显示每个的状态、分支、上一轮的改动、运行时长和最后一句话。需要你输入的排在最前面，点一下就跳到对应终端。

有 PR 的任务多一行 PR 状态：号码加上 *检查中 3/7*、*检查失败 1 项*、*2 条新评论*（两样都有时写在一起）、*可以合并*、*已合并* 或 *已关闭* 之一。Solo 运行期间用 `gh`（GitHub 命令行，要装好并登录；没装或没登录就不查，也不显示；分支还没推送过的任务也不查）向 GitHub 查：有检查在跑时每 1 分钟一次，其余每 5 分钟一次，PR 合并或关闭后不再查。*可以合并* 的意思是检查都过了（或没有检查）、没有待处理的修改要求、GitHub 报告可合并。点这一行弹出菜单：把 PR 反馈发给 agent、在浏览器中打开 PR、复制链接。发给 agent 的只有新的：还没发过的失败检查（按检查名和提交）和评论，无论上次是用这个菜单、任务列表里的 **把 PR 反馈发给 agent** 还是自动发的；内容粘贴进任务的 agent，不按回车；有新的反馈时，这一行右边有 **发给 agent** 按钮。发过什么重启后还记得。

PR 出现失败的检查、新的评论或者可以合并时，各发一次桌面通知（你正看着这个任务的终端时不发），点通知切到任务的终端。设置 > 终端 > PR 里有两项：`check_pull_requests`（默认 `true`；关掉后后台不再运行 `gh`）和 `auto_send_pr_feedback`（默认 `false`；打开后 agent 空闲时自动把新的失败检查和评论粘贴给它）。

### 标题栏工作区标签
每个项目文件夹是一个工作区，在标题栏占一个标签，各有自己的终端、agent、编辑器和文件树。标签上会按红、黄、绿、蓝的顺序显示这个工作区里 agent 出现的状态（失败、等你回复、完成待看、工作中），每种状态只显示一个图标，不标数量，后台有 agent 在等你也能看到；标签菜单里列出各个终端。

- `+` 或打开文件夹：新增工作区；
- `ctrl-alt-1` … `ctrl-alt-9`：切换到第 N 个；`ctrl-alt-pagedown` / `ctrl-alt-pageup`：下一个 / 上一个；
- 标签菜单（箭头或右键）列出这个工作区的终端，可以直接跳过去；
- 重启后只加载当前工作区，其他的在第一次切换过去时才加载；
- 终端的目录（agent 上报的目录优先）在另一个标签的项目里（它的文件夹，或同一个 git 仓库），或者不在本标签的任何文件夹里时，切到这个终端会在顶部看到提示条：“移到 <标签名>”，或者“在新标签中打开 <名字> 并移过去”（目录在 git 仓库里时用仓库的名字）。移动不会打断正在运行的程序和 agent，同一个标签里去往同一处的终端会一起移过去。选“留在这里”后不再为那里提示（Solo 重启后仍记得）；× 只关这一次。不在本标签文件夹里的终端，标签上还有一个虚线小标记写着它实际所在的文件夹，悬停卡片的“目录”下面也会注明。不会自动移动；
- 终端标签的右键菜单里有“移到 <名字> 的新标签”（或“移到 <标签名> 的标签”）和“移到工作区 ›”，可以随时把终端移到这个窗口的任何工作区标签，或者先选文件夹再移到新工作区；
- 一个工作区里有互不相关的项目时，Solo 会弹出“拆分工作区”对话框：列出留下和拆出去的文件夹；不在任何文件夹里的终端逐个选去向（默认新建它自己的标签）。点“拆分”才生效，“保持现状”之后不再询问；
- 这个版本的 Solo 第一次启动时，有标签之前留下的终端会按 git 仓库移到对应的标签，没有的话在后台打开一个。agent 任务的工作树算作它所属的仓库。开着 `follow_working_directory` 时这两项都不生效。

### 把东西发给 agent
- 编辑器里按 `ctrl-alt-k` 发送选中内容或光标位置；`ctrl-alt-shift-k` 可以先选目标终端；`ctrl-alt-shift-m` 发送当前文件的诊断信息。
- 终端里选中一个命令块（`ctrl-up/down` 或点左侧边栏），按 `alt-enter` 把命令和输出发给另一个终端里的 agent。
- 命令失败时，退出码旁边（以及右键菜单里）有 **发给 agent**：只挑出失败的部分，比如编译错误和位置、失败的测试和断言（支持 cargo、tsc、gcc/clang、MSVC、pytest、go、jest），其他情况取输出的最后一段，粘贴给在同一目录里工作的 agent。
- 在 Markdown 预览里选中文字，右键选 **批注…**。批注列在文档下方；点 **发给 agent**，所有批注连同行号和原文一起粘贴给最近改过这份文档的 agent（没有的话给最近用过的）。你读过之后文档又被改了，上方会提示新增和删除了多少行；**显示改动** 会标出改过的段落，**标为已读** 表示看完了。
- agent 可以用 `solo-cli agent` 驱动其他终端（列出终端、发消息、等它完成、读它的回答、新开任务），见 [agent-cli.md](../agent-cli.md)。设置项 `agent_control` 控制它能做到什么程度。从微软商店装的 Solo Code 里，`solo-cli` 只在 Solo 的终端里能用。
- 在 Claude Code 终端的菜单里选“开启手机控制”，Solo 会往 Claude Code 里输入 `/remote-control`，之后可以用手机上的 Claude App 继续这个会话。

### 内置浏览器
网页在编辑区的标签页里打开，放在终端旁边，用的是 Windows 11 自带的 Microsoft Edge WebView2 运行时（电脑上没有时，标签页里会提示并给出下载按钮）。

- **打开网页**：在终端、Markdown 预览或时间线里 `ctrl`+点击链接。打开到哪里由 设置 › 浏览器 › **链接打开方式** 决定：默认本机地址（localhost、127.0.0.1、[::1]、*.localhost）用 Solo 浏览器，其他用系统浏览器；也可以都用 Solo 浏览器，或都用系统浏览器。`ctrl+shift`+点击换另一种方式打开。已经打开的同一网址会切过去，不会再开一个。命令面板里的 **浏览器: 打开浏览器** 打开一个空标签页。
- **工具栏**：后退、前进、刷新（右键：不用缓存刷新）、地址栏（`5173` 和 `localhost:5173` 都等于 `http://localhost:5173/`；只打开 http、https 和本机的 HTML、PDF 文件）、视口（桌面、平板 768×1024、手机 390×844）、**选取**，以及 `⋯` 菜单：开发者工具（`f12`）、在页面中查找（`ctrl+f`）、在系统浏览器中打开、复制网址。
- 网页里打开新窗口的链接在 Solo 里开新标签页。重启 Solo 后恢复打开的网页（只保存网址）。
- 网页有焦点时，Solo 带 `ctrl`、`alt` 的快捷键和功能键照样有效，但网页编辑文字要用的留给网页：`ctrl+a/c/v/x/z/y`、按词移动和删除、`ctrl+b/i/u/k`、`ctrl+enter`。`esc` 和 `tab` 也留给网页。
- Cookie 和网站数据存在 `%LOCALAPPDATA%\Solo\browser`。

**给网页元素写批注。** 点 **选取** 后，鼠标下的元素会加框并显示选择器和尺寸；点击元素（不会触发网页自己的点击）弹出批注框。加过的元素在页面上显示编号，点编号可以改或删除。`esc` 退出选取。**发送给 agent · n** 把批注粘贴到最近用过的 agent 的输入里（不自动发送），旁边的箭头可以选别的 agent。每条带上元素的选择器、文字、HTML（超过约 1 KB 截断）、几项计算后的样式和元素截图的路径；截图存在 `%LOCALAPPDATA%\Solo\browser\picks`，保留一周。没有 agent 在运行时，批注复制到剪贴板。刷新或热更新后，按选择器重新找到元素。

**让 agent 操作浏览器。** 打开 设置 › 浏览器 › **允许 agent 操作浏览器**（默认关闭，重启 Solo 后生效）后，Solo 浏览器会在 127.0.0.1 上开一个随机的调试端口，之后在 Solo 终端里新启动的 agent 可以在里面打开网页、读取、点击、填表、截图，你能看着它操作。**这包括你已登录的网页，而且本机任何找到这个端口的程序都能这样做**，所以只在需要时打开。

- Claude Code 会多一个名叫 `solo-browser` 的 MCP 服务器（Playwright MCP，用 `npx` 运行，需要 Node.js）：`claude` 包装函数在 `--settings` 前加上 `--mcp-config`，叠加在你自己的 MCP 服务器上，不改动它们。和包装函数一样，需要 `"claude_code_hooks"` 开着。
- Codex 通过每次会话的 `-c mcp_servers.solo-browser=...` 得到同一个服务器（需要 `"codex_hooks"`）。
- 每个终端都有 `SOLO_BROWSER_CDP=http://127.0.0.1:<端口>`。其他 agent（Copilot CLI、OpenCode）可以在自己的 MCP 配置里加一个运行 `npx -y @playwright/mcp@0.0.85 --cdp-endpoint $SOLO_BROWSER_CDP` 的服务器。
- 网页里打开的新窗口显示为编辑区里的标签页，不抢焦点。agent 自己新建的标签页（Playwright 的“新标签页”）会在 Solo 外面单独开一个窗口，所以让 agent 用已经打开的标签页。至少要开着一个浏览器标签页，agent 才能找到浏览器。

### 更新重启、甚至崩溃，都不打断 agent
每个终端的 shell 跑在一个小的后台进程 `solo-terminal-host.exe` 里。Solo 更新重启（“更新并重启”，或商店版的更新），甚至崩溃时，正在运行东西（agent，或者编译之类的命令）的终端会在那里继续跑；Solo 再打开那个工作区时会自动连回去，期间的输出也都在，agent 也照样认得出来（状态点、标签上的名字）。空闲的 shell 还是随 Solo 一起结束；关掉终端标签会结束里面运行的东西。

- 更新时不因为终端里的 agent 而询问。自己退出 Solo（菜单退出、关最后一个窗口、Alt+F4）时，和关标签、关窗口一样先问：“退出并结束 N 个程序”会结束它们，“取消”什么都不关。重启后还没点开的工作区标签里留着在跑的终端，也会按标签列在里面。
- 设了 `"terminal": {"confirm_close": "never"}` 时，退出不问，直接结束。
- 想在退出时也让它们继续跑（和更新重启一样）：`"terminal": {"keep_running_on_quit": true}`。
- 留着在跑、但所在工作区标签还没点开的终端，会等那个标签：点开时它就在里面。
- 每个后台进程会把 Solo 发给它的指令和它结束的原因记在 Solo 日志目录的 `terminal-host-<日期>.log` 里（保留一周）。
- 如果某个后台进程自己出了问题，它的终端就结束了；重启后会按下面的方式提示继续会话。

### 命令输入框
shell 在提示符处等待输入时（需要 shell 集成），命令在终端底部的编辑器里输入。

- **边输边弹补全**：停顿一小会儿就出现。约 500 个常用命令有子命令、选项和参数值（git 分支、npm 脚本、docker 容器等），另外还有 PowerShell 自己的补全、PATH 里的命令和路径。空行、引号中间、`#` 请求里不弹。自动弹出的菜单里 **Tab 接受**、**Enter 直接运行**输入的命令；按 Tab（或 `ctrl-r`）打开的菜单里 Enter 接受。**Esc** 关闭。慢的来源（PowerShell、分支列表）随后补上，不影响打字。只想按 Tab 才出现：`"terminal": {"completions_as_you_type": false}`。
- **灰色提示**：光标后面的灰字先来自跑过的命令（这个目录里跑过的优先），没有的话来自命令规格（输入 `git ch` 提示 `eckout`）；按 `→` 或 Tab 接受。
- **`# 想做的事`** 加 Enter：让 AI 生成命令，填进输入框给你确认，再按一次 Enter 才运行。用的是你自己的 Claude Code（`claude -p`，默认模型 `haiku`）或 Codex（`codex exec`），装了哪个用哪个（都装了优先 Claude Code；用 `terminal.ai_command_agent` 指定）。生成时关掉了它们的工具：Claude Code 不给任何工具，Codex 用只读沙箱且没有 shell 工具，所以生成过程不会执行命令或改文件。它们在 Solo 的一个空目录里运行，不会加载项目的 hooks、MCP 服务器和 CLAUDE.md/AGENTS.md。生成中显示已用时间，**Esc** 取消。
- **会发送什么**：请求本身、shell、系统和当前目录，再加上最近一条失败的命令和它输出的最后一部分（最多 4000 字符；`terminal.ai_command_include_last_failure`），所以 `# 修好刚才的错误` 能用。选中一个命令块按 `ctrl-i`（或右键菜单“带上这个命令块让 AI 生成命令”）也可以把它带上。要发送的命令块以小标签显示在输入框上方，点 × 可以去掉。这些内容会以你的账号发给你自己的 Claude Code 或 Codex，和你直接在里面输入一样。
- Solo 不自动判断输入的是命令还是自然语言（Warp 的 Agent Mode 会这样做），请求要以 `#` 开头：Solo 的终端主要用来跑 agent，把命令误当成自然语言发给 AI 的代价比多打一个 `#` 大。

### 其他
- 关闭正在运行 agent 的终端或窗口前会先确认（`terminal.confirm_close`，默认 `"agents"`）。
- 重启后，跑着 agent、但没能保留下来的终端会提示是否继续上次的会话（例如 `claude --continue`）。
- 关掉最后一个终端后终端栏不收起，显示"新建终端"（Ctrl+Shift+`）、"新建 Claude Code / Codex 终端"（只列 `PATH` 里找得到的）和"查找文件"按钮。终端栏被收起（Ctrl+`）又没有打开文件时，编辑区显示同样的快捷入口。
- 终端栏标签条最右边的按钮隐藏终端栏（Ctrl+J），里面的终端和 agent 照常运行。点左边活动栏最上面的终端图标（或 Ctrl+J）显示回来；隐藏期间这个图标右下角显示需要你看一眼的 agent 数：红色失败、黄色等你回复、绿色完成待看。

### 大仓库里的语言服务
语言服务（rust-analyzer、clangd 等）会在后台分析整个项目，大仓库里可能占好几 GB 内存。
- **默认更省。** rust-analyzer 的 `cargo check` 同时最多跑逻辑核数四分之一个 `rustc`（至少 2 个）；clangd 用 2 个线程、低优先级建索引，预编译头放在磁盘上。想改回去：在设置里写 `"lsp": {"rust-analyzer": {"initialization_options": {"cargo": {"extraEnv": {"CARGO_BUILD_JOBS": "default"}}}}}`，并给 `lsp.clangd.binary.arguments` 写上你自己的参数（不要参数就写 `[]`）。
- **占了多少。** 状态栏 ⚡ 按钮旁边的数字是所有语言服务的内存，包括它们启动的进程（rust-analyzer 下面的 `cargo`、`rustc`）。鼠标停在按钮上，每个语言服务一行，例如"rust-analyzer：检查中 · 5 个编译进程 · 3.1 GB"；语言服务的子菜单里也有。
- **设置。** 设置 → 语言服务（⚡ 菜单最下面的"语言服务设置…"也能打开）：
  - "启用语言服务"：全部关掉或打开（`enable_language_server`）；关掉后什么都不启动，不管项目里怎么选的。
  - "默认分析级别"（`global_lsp_settings.default_analysis_mode`）：完整、仅打开的文件（rust-analyzer 保存时不跑 `cargo check`、不在后台建索引；clangd 不建后台索引；其他语言服务照常完整分析）或关闭。没在项目里单独选过的项目用它；项目里选过的优先。默认"完整"。
  - "大项目启动前询问"（`global_lsp_settings.ask_before_large_projects`）：默认级别是"完整"时，在超过 10 万个文件的项目里启动重的语言服务（rust-analyzer、clangd、roslyn、jdtls、gopls 等）时，Solo 会问一次要分析多少。
  - "内存超过 4 GB 时提醒"（`global_lsp_settings.memory_alert` 的 `enabled` 和 `threshold_gb`）：本项目的语言服务（包括它们启动的进程）一共持续超过阈值约 30 秒时，Solo 列出各自占了多少，同一项目在 Solo 重启前最多提醒一次；按钮可以只分析打开的文件、关闭本项目语言服务，或者不再提醒。它自己不会改任何东西。
- **本项目。** ⚡ 菜单顶部是本项目语言服务的内存和"完整 / 仅打开的文件 / 关闭"三档，一下作用于本项目全部语言服务（包括以后才启动的）；各服务不一致时三档都不高亮，并显示"各不相同"。下面每个语言服务一行，写着级别和内存；它的子菜单"在本项目"下可以单独选（完整分析；rust-analyzer 和 clangd 还有"暂停后台分析（只分析打开的文件）"；或者在本项目关闭）。选择按项目记住（存在 Solo 的数据库里，不写进仓库），重启后保持。关掉的语言服务列在下面，可以再开启。"停止所有语言服务"只停到 Solo 下次启动。
- **按轮审阅和暂停。** 一轮结束时，即使暂停了后台分析，Solo 仍会让 rust-analyzer 对改过的文件跑一次 `cargo check`，这一轮的诊断仍然完整；关掉以后就不比较 Rust 的诊断了。
- **语言服务起不来时**，如果 Solo 认得原因（例如 roslyn 缺 .NET 运行时、下载失败），状态栏会直接写出原因；点一下可以看到解决办法和下载链接。

### 给 agent 用的 IDE 工具
在 Solo 终端里启动的 agent 可以直接问 Solo 的语言服务，不用靠搜文本。范围是这个终端所属的工作区，一共四个只读工具：`workspace_symbols`（按名字找函数、类型）、`go_to_definition`（跳到定义）、`find_references`（找调用和引用，不会把同名的文本也算进来）、`diagnostics`（某个文件的错误和警告，或者整个工作区各文件的数量）。它们用的是 Solo 已经在跑的语言服务，agent 不会再起一份 rust-analyzer；问到某个文件时，如果它的语言服务还没启动，会顺便启动。
- **Claude Code** 通过 `claude` 包装函数（`--mcp-config`）拿到这些工具，**Codex** 通过 `codex` 包装函数（`-c mcp_servers.solo-ide...`），服务名都叫 `solo-ide`，只对这次会话有效，不会改你自己的配置。前提是终端开着 shell 集成；Claude Code 还需要开着 `claude_code_hooks`。
- **其他支持 MCP 的 agent** 可以自己配置：终端里 `SOLO_IDE_MCP_URL` 是服务地址（streamable HTTP），`SOLO_IDE_TOKEN` 是要放进 `Authorization: Bearer` 的 token。
- **代理。** Solo 会把 `127.0.0.1` 和 `localhost` 加进终端的 `NO_PROXY`：代理访问不到本机上的这个服务；而 Codex 即使访问 `127.0.0.1` 也会走 Windows 的系统代理（Internet 选项），不管系统代理的例外列表，只认 `NO_PROXY`。
- **只限本工作区。** 服务只监听 127.0.0.1。每个工作区有自己的随机 token，只有这个工作区的终端拿得到；工具只接受、只显示这个工作区里的文件（工作区外的文件、`.env` 这类私密文件都不行），也不会改任何东西。
- **局限。** agent 在磁盘上改了文件后，诊断可能还没更新（比如 rust-analyzer 的 `cargo check` 要等 Solo 保存文件时才跑），所以最后还是要让 agent 编译或跑测试确认。Solo 重启后保留下来的终端拿的还是旧地址，新开一个终端就能再用。
- 关闭方法：`"terminal": {"ide_tools": false}`（新开的终端不再带这些工具，已经开着的终端发来的请求也会被拒绝）。

## 7. 支持哪些 agent、支持到什么程度

| 功能 | Claude Code | Codex、Copilot CLI、OpenCode | gemini、qwen、amp、cursor-agent 等 | aider、`agent_commands` 里加的程序 |
| --- | --- | --- | --- | --- |
| 被识别为 agent（状态、关闭确认、发送命令块） | 是 | 是 | 是 | 是 |
| 状态来源 | hooks 上报，准确 | hooks 上报（OpenCode 是插件），准确（Codex 需要先信任一次） | 按各自的规则读屏幕，可能会慢一点 | 从终端输出推断 |
| 按轮审阅、回滚 | 是 | 是 | 是，按屏幕状态划分轮次 | 否 |
| 时间线、会话标题、token 用量 | 是 | Codex 是（标题取第一条提示词）；Copilot CLI、OpenCode 否 | 否 | 否 |
| 重启后继续会话 | 是（`--continue`） | 是（`codex resume --last`、`copilot --continue`、`opencode --session <id>`） | 是（各自的 resume 参数；qwen 和 amp 不支持） | 否 |
| agent 任务 | 默认命令就是 `claude` | 把 `agent_task_command` 改成对应程序即可 | 同左 | 同左 |

没有 hooks 的 agent，Solo 是这样判断状态的：输出里有转圈动画就是在工作，安静 3 秒左右算停止，响铃或发通知（OSC 9/777）就当作需要你输入。

要让 Solo 识别别的程序，在设置里加：

```json
{ "terminal": { "agent_commands": ["my-agent"] } }
```

## 8. Solo 对你的系统做了什么

- **Claude Code hooks 只对单次会话生效。** 在接入了 shell 集成的终端里，Solo 会定义一个 `claude` 包装函数，启动时给 claude 加上 `--settings %LOCALAPPDATA%\Solo\solo-shell-integration\claude-settings-<hash>.json`。Claude Code 会把这份设置叠加在你自己的设置上，**你的 `~/.claude/settings.json` 和你自己的 hooks 都不会被修改**。如果你已经有自己的 `claude` 函数或别名，Solo 不会覆盖它。运行 `claude mcp`、`claude update` 这类子命令，或者你自己传了 `--settings` 时，命令会原样执行。关闭方法：`"terminal": {"claude_code_hooks": false}`，对新开的终端生效。
- **这些文件丢了也没关系**（手动删除或被清理软件删掉）：agent 启动时，Solo 会先把它们重新写出来，包装函数再启动 agent。如果写不出来，包装函数会打印一行，说明这个 agent 在这个终端里没有状态上报，**为什么是这个 agent 状态？**里也会说明；新开一个终端即可恢复。
- **Codex hooks 也只对单次会话生效。** Solo 的 `codex` 包装函数会加上 `-c hooks.<事件>=...` 参数，Codex 把它们叠加在你自己的配置上，只对这次会话有效；**Solo 不会修改你的 `~/.codex/config.toml` 和 `hooks.json`**。Codex 只运行你信任过的 hooks，所以第一次启动时它会问你：选择信任后 Codex 会记住（由 Codex 写进它自己的配置）。这些 hooks 用完整路径运行 Solo 文件夹里的 `solo-agent-hook.exe`（Codex 在 Windows 上用 PowerShell 运行 hooks），所以 Solo 换了文件夹、或新版 Solo 改了 hooks 时，Codex 会再问一次。`codex login`、`codex mcp` 这类不启动会话的子命令原样执行；只有 `codex.cmd`、旁边没有 `codex.ps1` 或 `codex.exe` 时也原样执行。关闭方法：`"terminal": {"codex_hooks": false}`，对新开的终端生效。
- **Copilot CLI hooks 也只对单次会话生效。** Solo 会把带 hooks 的一个小插件写到 `%LOCALAPPDATA%\Solo\solo-shell-integration\copilot-plugin-<hash>`，`copilot` 包装函数用 `--plugin-dir` 加载它；**不会往 `~/.copilot` 里安装任何东西**。Copilot 用 PowerShell 运行这些 hooks（hooks 运行的是 `solo-agent-hook.exe`）。`copilot login`、`copilot plugin` 这类不启动会话的子命令原样执行。关闭方法：`"terminal": {"copilot_hooks": false}`，对新开的终端生效。
- **OpenCode 的状态插件也只对单次会话生效。** OpenCode 没有命令式 hooks，所以 Solo 会把一个小的 JavaScript 插件写到 `%LOCALAPPDATA%\Solo\solo-shell-integration\opencode-<hash>\opencode-plugin`（OpenCode 1 用的是 `opencode-config\plugins`），`opencode` 包装函数通过环境变量为这次会话加载它：OpenCode 2 用 `OPENCODE_CLI_CONFIG_CONTENT`（内容是你 `cli.json` 里的插件加上 Solo 的；Solo 只读取你的 `cli.json`，不修改它），OpenCode 1 用 `OPENCODE_CONFIG_DIR`。**不会往 `~/.config/opencode` 写任何东西**。如果你自己设置了其中某个变量，Solo 不会覆盖它（对应版本的 OpenCode 状态就改为读屏幕）。插件在一轮开始、请求权限或提问、一轮结束时运行 `solo-agent-hook.exe report`，并且只上报 OpenCode 窗口里当前显示的那个会话。重启后点“继续会话”会输入 `opencode --session <id>`。OpenCode 1 第一次运行时会往 `opencode-config` 里装它的插件依赖包。关闭方法：`"terminal": {"opencode_hooks": false}`，对新开的终端生效。
- **Pi、oh-my-pi、Kilo Code CLI、Gemini CLI、Qwen Code 也只对单次会话生效**（`more_agent_hooks`）。Pi 和 oh-my-pi：`pi`、`omp` 包装函数加上 `-e <Solo 的扩展>`（`pi install` 之类的命令照原样运行）。Kilo：Solo 的终端把 `KILO_CONFIG_DIR` 设为 Solo 的一个插件目录，Kilo 会把它加到你自己的插件之外（你自己设了 `KILO_CONFIG_DIR` 时 Solo 不动它）。Gemini CLI 和 Qwen Code：Solo 的终端把 `GEMINI_CLI_SYSTEM_DEFAULTS_PATH` / `QWEN_CODE_SYSTEM_DEFAULTS_PATH` 设为 Solo 的一个带状态 hooks 的设置文件，它们会把这些 hooks 加到你自己的 hooks 之外；管理员的系统默认设置文件（或你自己让变量指向的文件）会被复制进去，照样生效。不会往这些 agent 的目录写任何东西。Gemini CLI 和 Qwen Code 通过各自的 shell 运行 Solo 的 hook 程序，这一点还没有用真实的 agent 验证过。
- **Kimi Code CLI 和 MastraCode 只从你自己的配置读 hooks**，所以只给你列出的 agent 加：`"terminal": {"global_agent_hooks": ["kimi", "mastracode"]}`。第一次修改前 Solo 会把文件复制成 `<文件>.solo-backup`（`~/.kimi-code/config.toml` 或 `$KIMI_CODE_HOME`；`~/.mastracode/hooks.json`），并给自己加的条目做标记；把 agent 从列表里去掉，就只删除 Solo 加的条目。这些 hooks 在 Solo 的终端以外什么都不做。卸载 Solo 之前请先把它们从列表里去掉，否则 hooks 会一直指向 Solo 的 hook 程序。
- **为什么是这个状态？** 在运行 agent 的终端里右键，选“为什么是这个 agent 状态？”，或者运行 `solo-cli agent why <id>`（当前终端用 `self`）：会说明状态来自 agent 的 hook、屏幕规则（匹配了哪条规则，来自 Solo 内置的规则还是你放在 Solo 配置目录 `agent-detection` 下的文件）还是终端输出，以及最后一次 hook 上报是什么时候。
- **shell 集成**：PowerShell、bash、zsh、fish 在没有自定义启动参数时会加载 Solo 的集成脚本，这些脚本写在 `%LOCALAPPDATA%\Solo\solo-shell-integration`。PowerShell 用 `-NoExit -Command` 读取并执行这个脚本文件，不用 `-ExecutionPolicy Bypass`，也不用 `-EncodedCommand`。脚本先加载你自己的 profile，再加载集成。关闭方法：`"terminal": {"shell_integration": false}`。
- **hooks 跑的程序**是安装目录里的 `solo-agent-hook.exe`。它从 stdin 读事件，往终端写一个转义序列，然后退出；它不联网，也不写文件。
- **轮次快照**存在 `%LOCALAPPDATA%\Solo\agent_turns`，不碰你仓库里的东西。
- **agent 任务**会在你的仓库里新建分支和工作树，并往仓库的 git 配置里写 `branch.<分支>.solotask` / `solobase`（跨仓库任务还有 `sologroup`）。这些只在你新建任务时才会发生。
- **更新**：只有你点了更新才会安装：Solo Code 由 Microsoft Store 安装，GitHub 安装版由 `solo-update-helper.exe` 在 Solo 退出后静默运行新版本的安装程序。
- **`solo://` 链接**：Solo Code 的安装包（GitHub 安装版是安装程序，写在 `HKCU\Software\Classes\solo`）为当前用户注册 `solo://` 链接，`solo://settings/terminal.font_size`（跳到这一项设置）或分享的技能链接因此会在 Solo 里打开，卸载时删除。Solo 不注册 `zed://`，那是 Zed 的，但传给 Solo 的 `zed://` 链接照样能打开。任何网页都能打开这种链接，所以打开文件、连接 SSH 主机、克隆或打开仓库的链接（`solo://file/...`、`solo://ssh/...`、`solo://git/...`）会先问你，写明是哪个文件、哪台主机、哪个仓库。
- **终端宿主进程**：每个终端的 shell 跑在各自的 `solo-terminal-host.exe` 里，它从 `%LOCALAPPDATA%\Solo\terminal-host` 里的副本启动（这样更新时可以替换 Solo 的文件）。它和 Solo 之间用只有你这个用户能打开的命名管道通信；shell 结束、关掉标签，或者 Solo 结束而终端里没在运行东西时，它就结束（见第 6 节）。
- Solo 会读 Claude Code 的对话记录（`~/.claude/projects`）和 Codex 的会话记录（`~/.codex/sessions`），用来显示时间线和会话信息，只读不写。

## 9. 隐私与网络

**Solo 不发送遥测，也不上传崩溃报告。** 遥测的 `diagnostics` 和 `metrics` 默认都是关的；Solo 的构建没有配置崩溃上报地址，所以不会生成 minidump，也不会上传。Solo 启动时不会登录 Zed 账号；Zed 托管的模型、编辑预测、协作功能和 Zed 的自动更新都已关闭或隐藏。

**默认会访问的地址（不需要你做任何操作）：**

| 地址 | 用途 | 怎么关 |
| --- | --- | --- |
| Microsoft Store（Solo Code）或 `api.github.com`（其他版本） | 检查 Solo 更新（启动 30 秒后，之后每小时） | `"auto_update": false` |

**只在你用到某个功能时才访问：**

- 下载更新：Microsoft Store（Solo Code），或 GitHub（`github.com` 和它的下载服务器，GitHub 安装版）；
- Solo 自己下载的组件，先问过你（见下面的“下载前先询问”）：受信任项目的语言服务器（GitHub Releases、npm 源、`proxy.golang.org`、PyPI），系统里没有合适的 Node.js 时访问 `nodejs.org`，文件第一次格式化时装 Prettier，第一次调试时下载调试适配器，GitHub Copilot 的服务端；
- 你自己配置的 AI 服务（Anthropic、OpenAI、Ollama 等）；
- git blame 和提交详情里的头像：`avatars.githubusercontent.com`、GitHub API；
- 浏览、安装扩展，以及检查已装扩展的更新（没装扩展时不检查）：`api.zed.dev`，也就是 Zed 的扩展服务；
- Solo 不会访问 Zed 的其他服务：帮助链接打开的是这份指南（Solo 自带一份，写到它的数据目录里，没网也能打开）；连接需要 OAuth 登录的 MCP 服务器时，Solo 只用动态注册，不借用 Zed 的客户端身份（只支持客户端元数据文档的服务器因此无法登录）；Solo 去掉了 agent 面板（Zed 自己的 agent 界面），它的下载（比如 WSL 沙箱要用的 Linux 版 Zed）也就不会发生。
- 打开 ACP agent 注册表，或使用从注册表配置的 agent：`cdn.agentclientprotocol.com` 和注册表里列出的图标地址；
- 在终端里运行的 agent（Claude Code、Codex 等）自己的网络访问，不经过 Solo。

**下载前先询问。** Solo 自己要下载某个组件（语言服务器、Node.js、Prettier、调试适配器、GitHub Copilot、扩展更新）之前，会按组件问一次，并写明从哪里下载：

- **允许（记住）**：下载，以后它的更新不再询问；
- **暂不**：这次不下载，直到 Solo 重启；
- **使用本地路径…**：打开设置文件，改用你自己装好的（`"node": { "path": ... }`、`"lsp": { "<服务器>": { "binary": { "path": ... } } }`）。

同时来的几个请求合成一条通知，可以 **全部允许并记住**，也可以 **逐个选择…**。新项目的信任对话框会列出信任后 Solo 要下载的语言服务器，“允许这些下载”默认勾上，点一次就行。回答记在设置文件的 `downloads.allowed` 里，随时可以改。已经下载过的组件算作允许。Prettier 在文件第一次格式化时才安装，不在打开文件时安装；对未信任的项目，即使是 `"auto"` 模式也会先问。npm 安装 Solo 的包时加 `--ignore-scripts`（ESLint 服务端和扩展的包需要安装脚本，保留不加）。

```json
"downloads": {
  "mode": "ask",         // "ask"、"auto"（不问直接下载）或 "never"
  "allowed": { "rust-analyzer": true, "prettier": false },
  "check_updates": true  // 允许的组件是否检查并下载更新
}
```

**离线模式。** 点状态栏右侧的地球图标（或运行 `network: toggle offline mode`，或设置 `"network": { "offline": true }`）。之后 Solo 只连接本机和局域网（`localhost`、私有和链路本地地址、不带点的主机名、`.local`、`.lan`），所以本地模型（Ollama、LM Studio）、局域网里的服务和 SSH 照常可用。离线时什么都不下载，也不查版本：已经下载的语言服务器、Node.js、Prettier、调试适配器照常使用，缺的会提示“离线：未下载 X”。Copilot、云端 AI 服务、ACP agent 注册表、扩展商店和扩展更新、Solo 的自动检查更新和头像都会关闭。远程开发能用已经下载的远程服务端，但不会下载新的。终端里运行的 agent 不受影响。

**远程开发暂时关闭：** SSH、WSL、Docker（开发容器）远程开发要在远端运行一个服务端。Solo 的 release 现在带有自己构建的 Linux 版（x86_64 和 aarch64），但还在测试中，所以这些入口仍然隐藏，触发时会提示暂不可用。在终端里用 `ssh`、`wsl` 照常可以。想试用的话，设置环境变量 `SOLO_REMOTE_DEVELOPMENT=1` 后重新启动 Solo：连接时会从 Solo 的 GitHub release 下载与当前版本对应的远程服务端（`github.com`，按 GitHub 提供的 SHA-256 校验；和其他下载一样遵守离线模式和 `downloads.mode`，默认会先问，除非你在 `downloads.allowed` 里允许了 `remote-server`），上传到远端，从 `~/.solo_server` 运行，数据放在 `~/.local/share/solo`，与 Zed 的分开。不会访问 Zed 的服务器。暂不支持 macOS 和 Windows 远端。Solo 自己的功能是按本地项目写的：在远程项目里，终端预计没有命令块、agent 状态 hooks 和内置浏览器集成，终端不会在关窗口后保留，agent 任务会提示只支持本地项目。在远程项目里打开 localhost 链接时会提示打开的是本机的端口、不是远端的（WSL 不提示，因为它的端口会转发到本机）。这些还没有在真实主机上测过。

## 10. 故障排查

### 被安全软件拦截
**症状**：终端打不开、一打开就关、提示符一直不出现；Claude Code 的状态一直不更新。

**原因**：Solo 会用 PowerShell 执行它数据文件夹里的脚本，Claude Code 每个事件都会运行 `solo-agent-hook.exe`。有些安全软件会把这些行为当成可疑操作。

**处理**：

1. 把以下位置加入安全软件的信任或白名单：安装目录（Solo Code 在 `C:\Program Files\WindowsApps` 下，GitHub 安装版默认在 `%LOCALAPPDATA%\Programs\Solo`；里面有 solo.exe、solo-agent-hook.exe、solo-terminal-host.exe、OpenConsole.exe、conpty.dll），`%LOCALAPPDATA%\Solo\terminal-host`（终端宿主进程的副本），以及 `%LOCALAPPDATA%\Solo\solo-shell-integration`。
2. 想确认是不是被拦了：在设置里写 `"terminal": {"shell_integration": false}`，再新开一个终端。如果这样能用，就说明拦的是集成脚本。
3. 向厂商报告误报：例如 360 可以通过开放平台 open.soft.360.cn 提交（待核实）；其他厂商一般也有误报申诉入口。提交时附上版本号。

### 终端打不开
- 先看日志（见下文），搜索 `shell integration` 或 `failed to`。
- 如果你给 shell 设置了自定义启动参数，Solo 不会接入集成；这时命令块、输入编辑器和 Claude hooks 都不可用，但终端本身应该能打开。
- 安装目录缺了 `conpty.dll` 或 `OpenConsole.exe` 时，Solo 会退回使用系统自带的 ConPTY。

### agent 状态不对
- 确认终端用的是 PowerShell、bash、zsh 或 fish，并且没有自定义启动参数；System32 下的 `bash.exe`（WSL）不支持。
- 如果你有自己的 `claude` 函数或别名，Solo 的包装函数不会生效，这时状态只能靠输出推断。
- 改了 `claude_code_hooks`、`codex_hooks`、`copilot_hooks` 或 `opencode_hooks` 以后要新开终端才生效。
- 在 Codex 启动时的 hooks 确认里信任 Solo 的 hooks 之前，Codex 的状态仍然是读屏幕得来的。
- `solo-agent-hook.exe` 被拦截或丢失时，Solo 会改用 PowerShell 脚本，每次上报都会变慢。
- 会话认错了：运行 `terminal: choose agent session` 手动选。
- 其他 agent 的状态本来就是推断出来的，可能会慢一两秒。

### 日志和崩溃信息
- 日志：`%LOCALAPPDATA%\Solo\logs\Solo.log`，更早的内容在 `Solo.log.old`（日志变大时移过去）。命令面板里的 `solo: open log` 打开日志，`solo: reveal log in file manager` 在资源管理器里显示。
- 卡顿 trace：`%LOCALAPPDATA%\Solo\hang_traces\`。
- 崩溃：崩溃后先看 `%LOCALAPPDATA%\Solo\logs\Solo.log`（日志变大后旧内容会移到 `Solo.log.old`）。Solo 自身出错（panic）导致的崩溃会记在里面：搜 `panicked at`，后面是出错的位置、消息和调用栈，报告问题时请附上这一段。Solo 默认不生成 minidump；需要 minidump 时，设置环境变量 `SOLO_GENERATE_MINIDUMPS=1` 后再启动 Solo，文件会写到 logs 目录（具体文件名待核实）。
- 提交问题：[github.com/lucas9801/solo/issues](https://github.com/lucas9801/solo/issues)，附上日志。

## 11. 常用设置速查

Solo 自己加的设置写在 `"terminal"` 下面：

| 设置 | 默认值 | 作用 |
| --- | --- | --- |
| `shell_integration` | `true` | 加载 shell 集成（命令块、输入编辑器、hooks 都依赖它） |
| `claude_code_hooks` | `true` | 通过 `--settings` 给 Claude Code 加状态 hooks |
| `codex_hooks` | `true` | 通过 `-c hooks...` 给 Codex 加状态 hooks |
| `codex_edit_hooks` | `false` | 让 Codex 的 hooks 也报告每次 `apply_patch` 改了哪些文件；只在你自己的 Codex 配置设置了 `[otel]` 时需要（在 Windows 上每次补丁大约多花一秒） |
| `copilot_hooks` | `true` | 通过 `--plugin-dir` 给 Copilot CLI 加状态 hooks |
| `opencode_hooks` | `true` | 每次会话给 OpenCode 加载一个状态插件 |
| `ide_tools` | `true` | 以 MCP 服务的形式给 Claude Code 和 Codex 提供 Solo 的只读 IDE 工具（符号、定义、引用、诊断） |
| `more_agent_hooks` | `true` | 每次会话给 Pi、oh-my-pi、Kilo Code CLI、Gemini CLI、Qwen Code 加上状态上报 |
| `global_agent_hooks` | `[]` | 允许 Solo 往自己配置里加状态 hooks 的 agent（`"kimi"`、`"mastracode"`） |
| `keep_running_on_quit` | `false` | 自己退出 Solo 时，正在运行东西的终端也继续运行（更新重启和崩溃时总是继续）；false 时先问，确认后结束 |
| `agent_usage_display` | `"always"` | 运行中的 agent 的上下文、模型和思考强度、首字和速度、本轮用时和 token 显示在终端底部的细栏里（窗口窄时整段省略：先省掉本会话，再省掉本轮，再省掉首字和速度，再省掉模型，最后只留上下文进度条和百分比），标签上显示上下文圆环和百分比；`"tooltip"` 只在标签提示里显示 |
| `agent_commands` | `[]` | 把其他程序也当作agent |
| `agent_task_command` | `"claude"` | 新建agent任务时运行的命令 |
| `confirm_close` | `"agents"` | 关闭前确认：`agents` / `commands` / `never` |
| `follow_working_directory` | `false` | 文件树和搜索跟随当前终端所在的仓库 |
| `restore_contents` | `false` | 重启后恢复终端内容 |
| `input_editor` | `true` | 在底部用编辑器输入命令 |
| `completions_as_you_type` | `true` | 输入框边输边弹补全；`false` 只在按 Tab 时出现 |
| `ai_command_agent` | `"auto"` | 用哪个 agent 把 `#` 请求变成命令：`auto` / `claude_code` / `codex` |
| `ai_command_claude_model` | `"haiku"` | Claude Code 生成命令用的模型 |
| `ai_command_codex_model` | `""` | Codex 生成命令用的模型（空：用 Codex 的默认） |
| `ai_command_include_last_failure` | `true` | `#` 请求带上最近一条失败的命令和它输出的最后一部分 |
| `notify_long_commands_after_seconds` | `10` | 长命令跑完后通知；设为 0 关闭 |
| `agent_input_notifications` | `true` | agent 需要你输入或响铃时通知 |
| `agent_stopped_notifications` | `false` | agent 工作一段时间后停下时通知 |
| `program_notifications` | `true` | 允许程序发桌面通知（OSC 9/777） |

这些通知只在 Solo 处于后台时出现。四项也都可以在 设置 > 终端 > 通知 里改。

agent 任务的 PR（见上面的 agent 看板），在 设置 > 终端 > PR 里：

| 键 | 默认 | 作用 |
| --- | --- | --- |
| `check_pull_requests` | `true` | 用 `gh` 查 agent 任务的 PR，显示在 agent 看板上；检查失败、有新评论、可以合并时通知（你正看着这个任务的终端时不通知，不管 Solo 在不在后台） |
| `auto_send_pr_feedback` | `false` | agent 空闲时，把任务 PR 上新的失败检查和评论粘贴给它 |

不在 `"terminal"` 下的：`"ui_language"`（`"system"` / `"en"` / `"zh"`，也可以用环境变量 `SOLO_LANGUAGE` 覆盖）、`"auto_update"`、`"disable_ai"`、`"network": { "offline": false }`（离线模式）、`"downloads"`（`"mode": "ask"` / `"auto"` / `"never"`，`"allowed": {}`，`"check_updates": true`；见第 9 节）。`"disable_ai"`（以及 `"agent": { "enabled": false }`）只关闭 Zed 自己的 AI 功能（内联助手、编辑预测、提交信息生成），不影响工作区标签、终端和其中运行的 agent、按轮审阅。
