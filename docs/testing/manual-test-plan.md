# Solo 人工测试表 / Manual test plan

在**安装版**里逐项点（不是开发版）。只列 [feature-checklist.md](feature-checklist.md) 里不能自动化的部分：界面观感、外部链接、安装和更新、手机控制、多个 agent 的实际运行。每项在"通过 / 不通过"里打 x，问题写在备注里（附 `%LOCALAPPDATA%\Solo\logs\Solo.log` 里相关的几行）。

准备：一个 git 仓库（下面叫 `A`），一个不相关的文件夹或仓库 `B`；装好 Claude Code、Codex、Copilot CLI 和至少一个只靠屏幕识别的 agent（如 gemini 或 opencode）；手机上有 Claude App。标"pending G-xx"的项要等那个任务合并后再测。

版本：________　日期：________　系统：________　测试人：________

## 1. 安装、首次启动、卸载

| # | 步骤 | 预期 | 通过 | 不通过 | 备注 |
| --- | --- | --- | --- | --- | --- |
| 1.1 | 运行 `Solo-Setup-<版本>.exe`，选中文 | 安装界面是中文；装到 `%LOCALAPPDATA%\Programs\Solo`；开始菜单有 Solo；桌面图标默认不勾 | | | |
| 1.2 | 安装后第一次启动 | 欢迎页；终端占满窗口，有提示符；主题 Solo Dark | | | |
| 1.3 | 欢迎页"Agents in the Terminal" | 列出装了的 agent（Claude Code、Codex、Copilot…） | | | |
| 1.4 | 看安装目录 | 有 solo.exe、solo-agent-hook.exe、solo-update-helper.exe、solo-terminal-host.exe、OpenConsole.exe、conpty.dll、许可证 | | | |
| 1.5 | 便携 zip：解压后直接运行 `solo.exe` | 能启动，终端正常 | | | |
| 1.6 | （最后做）设置 → 应用 → 卸载 Solo | 程序删除；`%APPDATA%\Solo`、`%LOCALAPPDATA%\Solo` 还在；`solo://` 链接不再打开 Solo | | | |

## 2. 界面观感

| # | 步骤 | 预期 | 通过 | 不通过 | 备注 |
| --- | --- | --- | --- | --- | --- |
| 2.1 | 打开 `A`，在终端里 `ls`，再打开一个文件 | 编辑器出现在终端右边，分隔线可拖；关掉最后一个文件后编辑器区消失，焦点回终端 | | | |
| 2.2 | 左侧活动栏逐个点（项目、git、大纲、Agent Board） | 面板在左侧打开；右侧 dock 有关闭按钮；没有 Agent Panel 入口（G12/G13） | | | |
| 2.3 | 设置 `"ui_language": "zh"` 后重启 | 菜单、命令面板、通知、对话框、标签菜单都是中文，没有英文漏网 | | | |
| 2.4 | 关一个有未保存文件的标签 | Solo 自绘的确认框（不是系统对话框），中英文都正常（G15） | | | |
| 2.5 | 窗口缩到很窄 | 标题栏的工作区标签、用量条不重叠不溢出（pending G16） | | | |
| 2.6 | 悬停工作区标签和 agent 终端标签 | 卡片/提示内容正确、位置不遮挡（pending G23） | | | |
| 2.7 | 状态颜色：工作中 / 需要输入 / 完成 / 失败 | 标签和工作区标签上颜色可区分，深浅主题都看得清（pending G16） | | | |
| 2.8 | Help → About Solo | 关于窗口可拖动；版本、日期、提交正确；点提交复制；Copy 复制系统信息（G8） | | | |

## 3. 终端、命令块、输入编辑器

| # | 步骤 | 预期 | 通过 | 不通过 | 备注 |
| --- | --- | --- | --- | --- | --- |
| 3.1 | 在 PowerShell 终端里跑 `git status`、`cargo build`（故意失败） | 每条命令一个块，提示符上显示耗时和退出码；失败的有"Send to Agent" | | | |
| 3.2 | `ctrl-up` / `ctrl-down`，`ctrl-c`，`ctrl-alt-c`，`enter`，`escape` | 选块、复制（Markdown）、复制输出、重跑、取消 | | | |
| 3.3 | 选块后 `ctrl-s` 存为 Workflow，再在输入框 `ctrl-shift-r` | 出现刚存的 workflow；"Edit workflows…"打开 `workflows.json` | | | |
| 3.4 | 输入框里 `tab` 补全 `git ch`、`Get-Ch` | 补全菜单合理（pending G20） | | | |
| 3.5 | 输入框里 `up`/`down`/`ctrl-r`/`right` | 历史和建议正确 | | | |
| 3.6 | 输入 `# 列出最大的 5 个文件` 回车 | 生成的命令填进输入框，不自动执行（pending G21） | | | |
| 3.7 | 输出里点路径（`src/main.rs:12`、中文句子里的路径、只有行号的链接） | 打开文件到对应行（pending G26） | | | |
| 3.8 | `cat` 一个含 mermaid 代码块的文件，点 Preview | Mermaid 查看器，缩放拖动正常 | | | |
| 3.9 | 在 bash（Git Bash）、zsh / fish（若有）里重复 3.1 | 命令块和输入框都有 | | | |
| 3.10 | `Start-Sleep 12` 后切到别的程序 | 结束时桌面通知 | | | |

## 4. 各个 agent 实际运行

每个 agent 在 `A` 的终端里启动，提一个会改文件的问题（如"给 README 加一行"）。

| # | 步骤 | 预期 | 通过 | 不通过 | 备注 |
| --- | --- | --- | --- | --- | --- |
| 4.1 | Claude Code：`claude`，提问 | 标签转圈 → 完成 / 空闲；名字和会话标题；用量条和上下文百分比 | | | |
| 4.2 | Claude Code 问权限时切到别的程序 | "需要输入"状态 + 桌面通知 | | | |
| 4.3 | Claude Code 一轮后点"N files changed" | 审阅：Keep / Reject 每块、Keep All、行评论 → 发给 agent、Roll Back 再 Undo | | | |
| 4.4 | Claude Code 终端标签右键 → Agent Timeline | 不崩；时间线在旁边打开，内容是这次会话（G1） | | | |
| 4.5 | `terminal: choose agent session` | 列出会话，选另一个后时间线切换 | | | |
| 4.6 | Codex：`codex`，第一次信任 hook，提问 | 信任前状态靠屏幕，信任后靠 hook；一轮后有审阅和时间线、标题、token | | | |
| 4.7 | Copilot CLI：`copilot`，提问 | 状态正确，有审阅；无时间线（预期） | | | |
| 4.8 | gemini / opencode 等：启动、提问 | 状态从屏幕读（可能慢一两秒）；有审阅 | | | |
| 4.9 | 两个 agent 同时在两个终端里工作 | 各自状态正确，Agent Board 都列出、需要输入的排前面 | | | |
| 4.10 | 关掉跑着 agent 的终端标签 | 先问（`confirm_close`） | | | |
| 4.11 | agent 上下文用到 80% 以上 | "context is N% full" 提示一次 | | | |
| 4.12 | 检查 `~/.claude/settings.json`、`~/.codex/config.toml`、`~/.copilot` | Solo 没改它们 | | | |

## 5. 发送给 agent

| # | 步骤 | 预期 | 通过 | 不通过 | 备注 |
| --- | --- | --- | --- | --- | --- |
| 5.1 | 编辑器里选几行，`ctrl-alt-k` | 贴进 agent 终端（不回车） | | | |
| 5.2 | `ctrl-alt-shift-k` | 先选终端再贴 | | | |
| 5.3 | 有错误的文件里 `ctrl-alt-shift-m` | 错误和位置贴进去 | | | |
| 5.4 | 普通终端里选块，`alt-enter` | 切到 agent 终端并贴进去（G1） | | | |
| 5.5 | 失败的 `cargo build` 上点 Send to Agent | 贴错误位置，不是整段输出 | | | |
| 5.6 | Markdown 预览里选字 → Comment… → Send to Agent | 评论带行号贴进 agent | | | |
| 5.7 | 没有 agent 时 `alt-enter` | 复制到剪贴板并提示 | | | |

## 6. 工作区标签和 agent 任务

| # | 步骤 | 预期 | 通过 | 不通过 | 备注 |
| --- | --- | --- | --- | --- | --- |
| 6.1 | 打开 `B`（`+` 或打开文件夹） | 新标签；`ctrl-alt-1/2`、`ctrl-alt-pageup/pagedown` 切换 | | | |
| 6.2 | 在 `A` 的终端里 `cd` 到 `B` | 提示移到那个标签：Move to That Tab / Keep It Here | | | |
| 6.3 | 往一个工作区加不相关的文件夹 | 拆分提示：Split / Keep Together | | | |
| 6.4 | 后台标签里的 agent 需要输入 | 那个标签上计数变化 | | | |
| 6.5 | 标签菜单 | 终端列表可跳转；Reveal in File Manager；Close Workspace | | | |
| 6.6 | New Agent Task，起名 | 建 worktree 和分支，终端里跑 claude | | | |
| 6.7 | Manage Agent Tasks：View Diff、Ask the Agent to Commit、Merge、Delete | 都正常；Delete 不碰主 worktree | | | |
| 6.8 | Push and Open PR（有远程时） | 推送并打开 PR；Send PR Feedback to Agent 能贴反馈 | | | |
| 6.9 | 关闭 Solo 再打开 | 只加载活动工作区，其它切过去时加载 | | | |

## 7. 退出、重开、托管进程、更新

| # | 步骤 | 预期 | 通过 | 不通过 | 备注 |
| --- | --- | --- | --- | --- | --- |
| 7.1 | agent 工作时退出 Solo | 不问 agent（问未保存文件）；任务管理器里 solo-terminal-host 还在 | | | |
| 7.2 | 重开 Solo | 终端重连，显示期间输出，agent 状态正确（pending G4/G22） | | | |
| 7.3 | 空闲 shell 时退出 | 对应 terminal host 跟着退出 | | | |
| 7.4 | 更新通知（装旧版或等新版发布） | 一行摘要；Skip This Version / View Release Notes 正常 | | | |
| 7.5 | Update and Restart | 关窗口、静默安装、自动重启；跑着的 agent 不断（pending G5） | | | |
| 7.6 | Install When Solo Quits | 退出后安装，下次启动是新版 | | | |
| 7.7 | Help → View Release Notes Locally | Solo 的发行说明，不是空白页（G9） | | | |
| 7.8 | 断网时 Check for Updates | "Couldn't check for updates"，不卡 | | | |
| 7.9 | 重启后原来跑 agent、没能保留的终端 | Continue Session 能接上 | | | |

## 8. 外部链接

| # | 步骤 | 预期 | 通过 | 不通过 | 备注 |
| --- | --- | --- | --- | --- | --- |
| 8.1 | 设置窗口里某项复制链接（pending G24 的按钮），在浏览器或 `win-r` 打开 | Solo 打开设置并定位到该项 | | | |
| 8.2 | Help → Documentation | 本机指南，界面语言，离线也能开 | | | |
| 8.3 | Help → Solo Repository / File Bug Report / Request Feature | 打开 Solo 仓库对应页面 | | | |
| 8.4 | About 里的指南、发行说明、仓库、许可证链接 | 都打开 Solo 自己的页面 | | | |

## 9. 手机控制

| # | 步骤 | 预期 | 通过 | 不通过 | 备注 |
| --- | --- | --- | --- | --- | --- |
| 9.1 | Claude Code 终端标签右键 → Enable Remote Control | Claude Code 里出现 `/remote-control` 并启用 | | | |
| 9.2 | 手机 Claude App 里继续对话 | 能看到并接着这个会话；Solo 里状态跟着变 | | | |
