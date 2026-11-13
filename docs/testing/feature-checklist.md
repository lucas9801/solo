# Solo 功能清单 / Feature checklist

用户能碰到的 Solo 功能，从代码整理（Solo 2.2.0，提交 8b2afcc；标"pending G-xx"的是正在做的任务完成后的样子）。
每改一个功能就更新这里；`python script/solo-feature-audit.py` 会把代码里 Solo 新增的 action 和本文对比，列出漏掉的。

- 来源：Solo 新增的 action（相对 Zed v1.27.0，见 `script/solo-feature-audit/upstream-actions.txt`）、带 `SOLO:` 注释的改动、菜单（`crates/zed/src/zed/app_menus.rs`）、终端标签右键菜单（`tab_extra_context_menu_actions`）、工作区标签菜单、`assets/settings/default.json`、通知、外部入口。
- **Agent 列**：CC = Claude Code（hook）；Cx = Codex（hook，信任一次后）；Cp = Copilot CLI（hook 插件）；屏 = 只靠屏幕识别的 agent（gemini、opencode、qwen、amp、cursor-agent 等）；— = 与 agent 无关。
- **自动化列**：冒烟 = `crates/terminal_view/src/smoke_tests.rs` 在合适焦点下派发过（只保证不崩、不卡死）；单测 = 已有功能测试；人工 = 见 [manual-test-plan.md](manual-test-plan.md)；可加 = 能写 gpui 测试，还没写。
- 快捷键是 Windows 默认键位（`assets/keymaps/default-windows.json`）。

## 1. Solo 新增的 action

### 1.1 终端：命令块（shell integration）

| Action | 入口 | 操作步骤 | 预期结果 | Agent | 自动化 |
| --- | --- | --- | --- | --- | --- |
| `terminal::SelectPreviousCommandBlock` / `terminal::SelectNextCommandBlock` | `ctrl-up` / `ctrl-down`，或点命令块左侧的槽 | 在有几条命令输出的终端里按 | 选中上 / 下一个命令块，块高亮，粘性标题显示命令 | — | 冒烟、单测 |
| `terminal::ClearCommandBlockSelection` | 选中块时 `escape` | 选中一块后按 | 取消选中，焦点回到输入框 | — | 冒烟 |
| `terminal::CopyCommandBlock` | 选中块时 `ctrl-c`；块右键菜单"Copy Command and Output" | 选中块后复制 | 剪贴板是 Markdown：命令、输出、退出码 | — | 冒烟、单测 |
| `terminal::CopyCommandBlockCommand` | 块右键菜单"Copy Command" | | 只复制命令 | — | 冒烟 |
| `terminal::CopyCommandBlockOutput` | 选中块时 `ctrl-alt-c`；右键"Copy Output" | | 只复制输出 | — | 冒烟 |
| `terminal::RerunCommandBlock` | 选中块时 `enter` | | 在 shell 里重新执行这条命令 | — | 冒烟 |
| `terminal::SaveCommandBlockAsWorkflow` | 选中块时 `ctrl-s`；右键"Save Command as Workflow" | | `%APPDATA%\Solo\workflows.json` 多一条，提示"Saved as a workflow" | — | 人工（写真实配置目录，冒烟跳过） |
| `terminal::SendCommandBlockToAgent` | 选中块时 `alt-enter`；右键"Send to AI Agent" | 另一个终端在跑 agent 时 | 切到 agent 的终端，把命令和输出贴进去（不回车）；没有 agent 时复制到剪贴板并提示 | CC Cx Cp 屏 | 冒烟、单测（G1 崩溃的回归） |
| `terminal::SendFailuresToAgent` | 失败命令退出码旁"Send to Agent"；右键 | 跑一个失败的 `cargo build` / `tsc` / `pytest` 等 | 把错误位置和失败断言贴到同目录的 agent；不认识的输出贴结尾部分 | CC Cx Cp 屏 | 冒烟、单测（解析） |
| `terminal::PreviewMermaidDiagram` | 输出里有 mermaid 代码块时的"Preview"按钮；右键 | | 打开 Mermaid 查看器，可缩放（`=` `-` `0` `1`）、拖动 | — | 冒烟 |

### 1.2 终端：输入编辑器

| Action | 入口 | 操作步骤 | 预期结果 | Agent | 自动化 |
| --- | --- | --- | --- | --- | --- |
| `terminal::SubmitInput` | 输入框里 `enter` | 输入命令回车 | 命令送进 shell，输入框清空，命令块开始 | — | 冒烟、单测 |
| `terminal::CompleteInput` | `tab` | 输入 `git ch` 按 tab | 补全菜单（Warp 风格命令签名、git alias、PowerShell 补全器）（pending G20） | — | 冒烟、单测 |
| `terminal::AcceptInputSuggestion` | `right`（光标在行尾） | 有灰色建议时按 | 接受历史建议 | — | 冒烟、单测 |
| `terminal::InputHistoryPrevious` / `terminal::InputHistoryNext` | `up` / `down` | | 翻 shell 历史 | — | 冒烟、单测 |
| `terminal::SearchInputHistory` | `ctrl-r` | | 历史搜索选择器 | — | 冒烟 |
| `terminal::SearchWorkflows` | `ctrl-shift-r` | | Workflow 选择器（内置 + `workflows.json`），选中后填进输入框 | — | 冒烟 |
| `terminal::SelectWorkflowArgument` | 填入带参数的 workflow 后 `tab` | | 依次选中参数占位符 | — | 冒烟 |
| `terminal::OpenWorkflowsFile` | Workflow 选择器底部"Edit workflows…"；命令面板 | | 打开 `%APPDATA%\Solo\workflows.json`（不存在就创建示例） | — | 人工（写真实配置目录，冒烟跳过） |
| `terminal::CopyOrClearInput` | 输入框里 `ctrl-c` | 有选中文字 / 没有 | 有选中就复制，没有就清空输入（像 shell 的 ctrl-c） | — | 冒烟、单测 |
| `terminal::CopyInputOrTerminal` | `ctrl-shift-c` / `ctrl-insert` | | 复制输入框或终端里选中的文字 | — | 冒烟 |
| 输入 `#` 生成命令 | 输入框开头输入 `# 描述` 回车 | | 用配置的模型生成命令填进输入框（pending G21） | — | 人工 |

### 1.3 终端：agent

| Action | 入口 | 操作步骤 | 预期结果 | Agent | 自动化 |
| --- | --- | --- | --- | --- | --- |
| `terminal::ShowAgentTimeline` | 终端标签右键"Agent Timeline"（跑过 agent 才有）；命令面板 | 跑过一轮 agent 后选 | 终端旁边打开时间线：每轮的提问、读/改的文件、命令、回答；再选一次切到已打开的那个 | CC Cx（Cp、屏：无时间线） | 冒烟、单测（G1 崩溃的回归） |
| `terminal::ChooseAgentSession` | 命令面板；时间线里"Choose Session…" | | 列出这个目录下 agent 的会话，选一个后时间线/用量跟着换；没有 agent 时提示 | CC Cx | 冒烟 |
| `terminal::ReviewAgentTurn` | 标签上"N files changed"；命令面板 | agent 改完一轮后 | 打开本轮审阅：Keep / Reject（每块或全部）、行评论发给 agent、Roll Back / Undo Rollback、诊断变化、跳转定义；目录不能快照时列出改动文件 | CC Cx Cp 屏 | 冒烟、单测 |
| `terminal::EnableRemoteControl` | Claude Code 终端标签右键"Enable Remote Control" | | 在 Claude Code 里输入 `/remote-control`，可以在手机 Claude App 里接着用 | CC | 冒烟；手机端人工 |
| `terminal::SendSelectionToAgent` | 编辑器 `ctrl-alt-k`；`ctrl-alt-shift-k`（`choose_terminal`，选终端） | 在编辑器里选一段 | 选区（`@path:行`+内容）贴进上次用的 agent 终端 | CC Cx Cp 屏 | 冒烟 |
| `terminal::SendPositionToAgent` | 命令面板（`choose_terminal` 可选） | | 光标位置 `@path:line` 贴进 agent；未保存的文件提示 | CC Cx Cp 屏 | 冒烟 |
| `terminal::SendDiagnosticsToAgent` | 编辑器 `ctrl-alt-shift-m` | 文件有错误时 | 文件的错误/警告和位置贴进 agent | CC Cx Cp 屏 | 冒烟 |
| `terminal::SendDocumentCommentsToAgent` | Markdown 预览：选文字 → 右键"Comment…"→ 文末"Send to Agent" | | 所有评论（行号、原文、评论）贴进最后改这个文档的 agent | CC Cx Cp 屏 | 冒烟（直接构造 action） |

### 1.4 Agent 任务、看板、工作区标签

| Action | 入口 | 操作步骤 | 预期结果 | Agent | 自动化 |
| --- | --- | --- | --- | --- | --- |
| `agent_task::NewAgentTask` | 命令面板"New Agent Task"；工作区标签菜单"New Agent Task…" | 输入任务名 | 建分支和 worktree（`git.worktree_directory`，默认 `../worktrees`），开终端跑 `terminal.agent_task_command`；多仓库工作区可同时给其它仓库建；`.solo/tasks.json` 的 setup 命令先跑 | 默认 CC，可设成任意 | 冒烟、单测 |
| `agent_task::ManageAgentTasks` | 命令面板；工作区标签菜单"Manage Agent Tasks…" | | 任务列表：打开终端、View Diff、Merge、Push and Open PR、Send PR Feedback to Agent、Ask the Agent to Commit、Delete | CC Cx Cp 屏 | 冒烟、单测 |
| `agent_board::ToggleFocus` | 左侧活动栏"Agent Board"；命令面板 | | 看板列出跑过 agent 的终端和任务：状态、分支、改动、时长、最后一句；需要输入的排前面；点击跳到终端 | CC Cx Cp 屏 | 冒烟 |
| `solo::ActivateWorkspace` | `ctrl-alt-1`…`ctrl-alt-9` | 有几个工作区标签时按 | 切到第 N 个工作区；未加载的此时加载；没有第 N 个就不动 | — | 单测（title_bar `test_switch_workspaces_with_actions`） |
| `solo::NextWorkspace` / `solo::PreviousWorkspace` | `ctrl-alt-pagedown` / `ctrl-alt-pageup` | | 切到下 / 上一个工作区，循环 | — | 单测（同上） |
| `solo::OpenRepository` | 帮助菜单"Solo Repository" | | 浏览器打开 github.com/lucas9801/solo | — | 人工 |

## 2. 菜单

### 2.1 应用菜单（标题栏左侧，`app_menus.rs`）

| 菜单项 | Action | 预期结果 | 自动化 |
| --- | --- | --- | --- |
| Solo → About Solo | `zed_actions::About`（G8 重写） | 可拖动的关于窗口：版本、发布日期、提交（点击复制）、是否最新 + Check for Updates、指南/发行说明/仓库/许可证链接，Copy 复制版本和系统信息 | 人工 |
| Solo → Check for Updates | `auto_update::Check` | 检查 GitHub Releases；有新版本弹更新通知，没有显示"Up to date" | 单测（auto_update）、人工 |
| Solo → Settings → … | Open Settings / Settings File / Project Settings / Keymap / Default Key Bindings / Theme / Icon Theme | 打开对应文件或窗口；默认主题 Solo Dark、图标 Solo (Default) | 人工 |
| Solo → Extensions | `zed_actions::Extensions` | 扩展页（会访问 api.zed.dev） | 人工 |
| Solo → Quit Solo | `zed_actions::Quit` | 有未保存文件会问；运行中的终端不问，留在 terminal host 里（`keep_running_on_quit`） | 人工 |
| File / Edit / Selection / Go / Run / Window | Zed 原有 | 与 Zed 相同；File 没有 Collab，View 没有 Collab Panel 和 Agent Panel | 人工抽查 |
| View → Terminal Panel | `terminal_panel::Toggle` | 终端优先布局下切换终端区 | 人工 |
| Help → View Release Notes Locally | `auto_update_ui::ViewReleaseNotesLocally`（G9） | 打开 Solo 当前版本的发行说明，不是 Zed 的空白页 | 人工 |
| Help → View Dependency Licenses | `zed_actions::OpenLicenses` | 打开许可证列表 | 人工 |
| Help → Show Welcome | `onboarding::ShowWelcome` | 欢迎页，含"Agents in the Terminal"（查找已安装的 agent） | 人工 |
| Help → File Bug Report / Request Feature | `zed_actions::feedback::*` | 打开 Solo 仓库的 issue 页面 | 人工 |
| Help → Documentation | `zed_actions::OpenDocs` | 打开本机的用户指南（界面语言），离线可用 | 人工 |
| Help → Solo Repository | `solo::OpenRepository` | 见上 | 人工 |

### 2.2 终端标签右键菜单（`tab_extra_context_menu_actions`）

| 菜单项 | 条件 | Action | 预期结果 | Agent | 自动化 |
| --- | --- | --- | --- | --- | --- |
| Rename | 不是任务终端 | `terminal::RenameTerminal` | 标签名可编辑 | — | 冒烟（菜单项逐个派发） |
| Enable Remote Control | 跑着 Claude Code | `terminal::EnableRemoteControl` | 见 1.3 | CC | 冒烟 |
| Agent Timeline | 跑过 agent | `terminal::ShowAgentTimeline` | 见 1.3；1.28.1 起选它必崩（G1），冒烟测试会抓到 | CC Cx | 冒烟 |
| Close / Close Others / Pin 等 | Zed 原有 | | 关掉跑着 agent 的终端会先问（`confirm_close`） | CC Cx Cp 屏 | 人工 |

终端区域的右键菜单（命令块、链接）：Copy、Paste、Copy Command / Output / Command and Output、Send to AI Agent、Send to Agent（失败命令）、Save Command as Workflow、Preview Mermaid Diagram、链接的打开 / 复制（pending G26：只有行号的链接）。

### 2.3 工作区标签（标题栏）

| 入口 | 操作 | 预期结果 | Agent | 自动化 |
| --- | --- | --- | --- | --- |
| `+` 按钮 / 打开文件夹 | 点击 | 新增工作区标签 | — | 人工 |
| 标签本身 | 点击 | 切换工作区；标签上数正在工作 / 等你 / 失败的 agent（颜色 pending G16） | CC Cx Cp 屏 | 人工 |
| 标签悬停 | 悬停 | 工作区信息卡片（pending G23） | — | 人工 |
| 标签菜单（箭头或右键） | 打开 | 列出终端（跳转）、"No terminals"、"Opens when you switch to it"、Agent Tasks 小节、New Agent Task…、Manage Agent Tasks…、Reveal in File Manager、Close Workspace | — | 人工 |
| 重启后 | 重开 Solo | 只加载活动工作区，其它切过去时加载 | — | 人工 |

### 2.4 命令面板

上面所有 `namespace::Name` 都能在命令面板里搜到（名字按 `namespace: name` 显示，例如 `terminal: show agent timeline`）。另外：`solo: open log`、`solo: reveal log in file manager`、`solo: about`、`auto update: check`。

## 3. Solo 新增或改了默认值的设置（`assets/settings/default.json`）

| 设置 | Solo 默认 | 作用 | 验证方法 | 自动化 |
| --- | --- | --- | --- | --- |
| `theme` / `icon_theme` | Solo Dark / Solo (Default) | 外观 | 看 | 人工 |
| `buffer_font_family` / `ui_font_family` | Lilex / IBM Plex Sans | 字体 | 看 | 人工 |
| `bottom_dock_layout` | `terminal_first` | 终端占满窗口，编辑器有文件时出现在右侧，可拖分隔线；左 dock 变活动栏 | 打开/关闭文件 | 人工 |
| 面板 dock 位置 | 项目/git/大纲在左，调试在右（G12） | | 看 | 人工 |
| `use_system_prompts` | `false` | Solo 自绘的确认框（G15） | 关窗口、退出时看对话框 | 人工 |
| `title_bar.show_sign_in` / `show_user_picture` / `show_onboarding_banner` | `false` | 不显示 Zed 账号 | 看 | 人工 |
| `markdown.open_markdown_files_in_preview` | `true` | Markdown 文件用预览打开 | 打开 README.md | 人工 |
| `telemetry.diagnostics` / `metrics` | `false` | 不发遥测 | 看网络 | 人工 |
| `ui_language` | `"system"` | `en` / `zh`，`SOLO_LANGUAGE` 环境变量优先 | 切换后重启 | 单测（i18n）、人工 |
| `auto_update` | `true` | 启动 30 秒后、之后每 6 小时检查 GitHub Releases | 看日志 | 单测、人工 |
| `disable_ai` | `false` | 只关 Zed 自己的 AI；工作区标签、终端 agent、审阅不受影响（G14） | 打开后看标签还在 | 单测、人工 |
| `edit_predictions.provider` | `"none"` | 无编辑预测 | 看 | 人工 |
| `terminal.starts_open` | `true` | 启动就有终端 | 新窗口 | 人工 |
| `terminal.shell_integration` | `true` | 命令块、输入框、hook 都靠它 | 设 false 后新开终端：无命令块 | 人工 |
| `terminal.input_editor` | `true` | 底部输入编辑器 | 设 false 后直接在终端里打字 | 单测、人工 |
| `terminal.compact_prompt` | `true` | 紧凑提示符 | 看 | 人工 |
| `terminal.completions_as_you_type` | `false` | 边打边补全（pending G20 可能调整） | | 人工 |
| `terminal.open_links_on_click` | `true` | 单击打开路径/链接（pending G26） | 点输出里的路径 | 单测、人工 |
| `terminal.notify_long_commands_after_seconds` | `10` | Solo 在后台时长命令结束发通知；0 关 | `Start-Sleep 12` 后切走 | 人工 |
| `terminal.program_notifications` | `true` | 允许程序发 OSC 9/777 桌面通知 | | 人工 |
| `terminal.restore_contents` | `false` | 重启后显示终端旧内容 | | 单测、人工 |
| `terminal.agent_commands` | `[]` | 额外当作 agent 的程序 | 加 `"my-agent"` | 单测 |
| `terminal.claude_code_hooks` / `codex_hooks` / `copilot_hooks` / `opencode_hooks` | `true` | 每个会话通过 `--settings` / `-c hooks` / `--plugin-dir` / OpenCode 插件（环境变量加载）加状态 hook，不改用户配置 | 新终端里跑 agent 看状态 | 单测（solo_agent_hook）、人工 |
| `terminal.keep_running_on_quit` | `true` | 跑着东西的终端在退出 / 更新 / 崩溃后继续，重开时重连（pending G4/G5/G22） | 跑 agent 时退出再开 | 单测（solo_terminal_host）、人工 |
| `terminal.agent_usage_display` | `"always"` | 底部用量条 + 标签上的上下文百分比；`"tooltip"` 只在提示里（pending G16/G23） | | 单测、人工 |
| `terminal.follow_working_directory` | `false` | 文件树和搜索跟随活动终端的仓库 | | 人工 |
| `terminal.confirm_close` | `"agents"` | `agents` / `commands` / `never` | 关跑着 agent 的标签 | 单测、人工 |
| `terminal.agent_control` | `"full"` | 限制 `solo-cli agent` 能做什么 | | 单测 |
| `terminal.agent_task_command` | `"claude"` | 新任务跑什么 | | 人工 |
| `git.worktree_directory` | `../worktrees` | 任务 worktree 放哪 | | 单测 |
| agent 面板 / 协作 / 反馈相关 | 关或隐藏（G13） | 没有 Agent Panel 入口；右侧 dock 有关闭按钮 | 看 | 人工 |
| LSP 资源上限 | （pending G19） | | | 人工 |

## 4. 通知和提示

| 通知 | 何时出现 | 按钮 / 预期 | Agent | 自动化 |
| --- | --- | --- | --- | --- |
| 更新可用 | 检查到新版本 | 一行摘要（G3）；Update and Restart / Skip This Version / View Release Notes / Install When Solo Quits；更新后自动重启（pending G5） | — | 单测（solo_update）、人工 |
| 检查失败 | 没网 | "Couldn't check for updates" | — | 人工 |
| 拆分工作区 | 一个工作区里有互不相关的项目 | Split / Keep Together；终端跟着走，运行的程序不受影响 | — | 单测、人工 |
| 移动终端 | 终端 `cd` 进另一个标签的项目 | Move to That Tab / Keep It Here（之后不再问这个标签） | — | 单测、人工 |
| 上下文快满 | agent 上下文用量超过阈值 | "context is N% full… /compact" | CC Cx | 单测、人工 |
| agent 需要输入 / 停了 | Solo 在后台 | 桌面通知 | CC Cx Cp 屏 | 人工 |
| 长命令结束 | Solo 在后台，命令超过 10 秒 | 桌面通知 | — | 人工 |
| 程序通知 OSC 9/777 | Solo 在后台 | 桌面通知 | 屏 | 人工 |
| 继续会话 | 重启后，原来跑 agent 的终端没能保留 | Continue Session / Dismiss（`claude --continue` 等） | CC Cx Cp 屏（qwen、amp 除外） | 单测、人工 |
| 关闭确认 | 关跑着程序的标签 / 窗口 | 一个对话框列出会结束的程序（Solo 自绘） | CC Cx Cp 屏 | 单测、人工 |
| 没有 agent | 发送到 agent 但没有 agent 终端 | 复制到剪贴板 + 提示 | — | 冒烟 |
| Workflow 已保存 | Save Command as Workflow | 提示 ctrl-shift-r | — | 人工 |
| 回滚失败 / 撤销 | 审阅里 Roll Back | Rollback failed / The rollback was undone | CC Cx Cp 屏 | 单测 |

## 5. 外部入口

| 入口 | 操作 | 预期结果 | 自动化 |
| --- | --- | --- | --- |
| 安装程序 `Solo-Setup-<版本>.exe` | 安装（中/英） | 装到 `%LOCALAPPDATA%\Programs\Solo`，开始菜单，可选桌面图标，注册 `solo://`（HKCU） | 人工 |
| 便携 zip + `install.ps1` | 解压运行 / 安装 | 直接运行 solo.exe；install.ps1 复制并建快捷方式 | 人工 |
| 更新 | 更新通知 → Update and Restart | 下载到 `%TEMP%\solo-update`，退出（问未保存文件），`solo-update-helper.exe` 静默安装后重启；运行中的终端保留（pending G4/G5/G22） | 单测（solo_update_helper）、人工 |
| 卸载 | 设置 → 应用；或 `install.ps1 -Uninstall` | 删程序和 `solo://` 注册，不删设置和数据 | 人工 |
| `solo://settings/<路径>` 链接 | 设置窗口里复制链接（pending G24 的复制按钮），浏览器 / 运行框打开 | Solo 打开设置并定位到该项；`zed://` 链接也能开 | 单测（open_listener）、人工 |
| `solo://skill?data=` | 打开 | 等同 `zed://skill` | 单测 |
| `solo-cli agent ls / new-task / send / wait / output / open / review` | agent 在 Solo 终端里调用 | 见 `docs/agent-cli.md`；受 `agent_control` 限制 | 单测、人工 |
| `solo-agent-hook.exe report …` / OSC 6973 | agent hook 或脚本 | 标签状态：工作中 / 需要输入 / 空闲 / 完成 / 失败（见 `docs/agent-status-protocol.md`） | 单测 |
| 托管进程 `solo-terminal-host.exe` | 每个终端一个 | 退出 Solo 后跑着东西的终端继续，重开后重连并显示期间输出；空闲 shell 跟着退出 | 单测、人工 |
| 手机 Remote Control | Enable Remote Control 后在手机 Claude App 里继续 | | 人工 |
| 日志 | `%LOCALAPPDATA%\Solo\logs\Solo.log` | panic 带回溯（G2） | 人工 |

## 6. 其它可见行为

| 功能 | 操作 | 预期结果 | Agent | 自动化 |
| --- | --- | --- | --- | --- |
| 标签上的 agent 状态 | 在终端跑 agent | 转圈 / 需要输入 / 空闲 / 完成 / 失败 + agent 名和会话标题；悬停看 token 和上下文（pending G16/G23） | CC Cx Cp 屏 | 单测、人工 |
| 用量条 | agent 运行时 | 终端底部显示上下文和 token，窄时先丢后面的部分（pending G16） | CC Cx | 单测、人工 |
| 每轮快照与"N files changed" | agent 一轮结束 | 标签显示改动数，点开审阅；不打开 agent 改的文件（G6） | CC Cx Cp 屏 | 单测 |
| Solo 的 git diff 视图 | git 面板点文件 | Stage / Unstage / Restore / 跳到上一块下一块 | — | 人工 |
| Markdown 预览改动条 | agent 改了正在看的文档 | 条上显示增删行，Show Changes / Mark as Read | CC Cx Cp 屏 | 单测、人工 |
| 路径和链接 | 点终端输出里的路径（含中文文本里的路径、跨行的路径） | 打开文件到行（pending G26） | — | 单测、人工 |
| 中文界面 | `ui_language: "zh"` | 菜单、通知、对话框是中文 | — | 单测、人工 |
| 远程开发入口 | SSH / WSL / Dev Container | 隐藏；用了提示不支持，不连 Zed 服务器 | — | 人工 |
