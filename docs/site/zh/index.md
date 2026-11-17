# Solo

**让 agent 写代码，你来读、判断和指挥。**

Solo 是一个给 AI agent 用的编辑器，基于 [Zed](https://github.com/zed-industries/zed)。终端占满窗口，Claude Code、Codex 这些 agent 就跑在你熟悉的真终端里，用你自己的命令行工具和订阅；需要看代码时，一个完整的编辑器从右边打开。

它是给还要自己读代码、读文档的人做的：agent 每做完一轮，你能马上看清它改了什么、有没有引入新错误，留下或者撤回，再把意见发回给它。

[下载（Windows，Microsoft Store）](https://apps.microsoft.com/detail/9NBW452JVWN6?mode=direct) · [用户指南](../../guide/zh-CN.md) · [和其他工具比较](compare/index.md) · [English](../en/index.md)

---

## 读：agent 改了什么，一眼看清

- **按轮审阅。** agent 每结束一轮，终端标签上出现"N 个文件已修改"。点开就是这一轮的全部改动，逐块保留或拒绝，在代码行上写意见，一次发回给 agent。[详细 →](features/review.md)
- **诊断增量。** 一轮结束后，Solo 让语言服务器检查改过的文件，告诉你这一轮**新增**了几个错误和警告、修掉了几个。
- **在审阅里跳转。** 审阅界面就是编辑器：跳到定义、查引用、搜索都能用，每个改动块上方写着它在哪个函数里。
- **点开就看。** 终端输出里的文件路径点一下就打开：代码进编辑器，Markdown 进渲染好的预览，图片进图片查看器；agent 输出里的 Mermaid 代码块直接看图。
- **文档也能审。** agent 写的计划、设计文档在预览里打开（Mermaid 图照常显示），选中文字写批注，一次发回去；文档再被改动时，会告诉你改了哪几块。[详细 →](features/documents.md)

## 判：留下、撤回，都有底

- 快照存在 Solo 自己的目录里，不碰你的分支、暂存区和 stash。
- 可以**回滚到任意一轮之前**，回滚本身也能撤销；中途失败会保留撤销点。
- agent 上报了自己改的文件时（Claude Code），一轮只算它改的，你自己或别的 agent 同时改的不会被撤回。
- 支持 Claude Code、Codex、Copilot CLI、OpenCode 等二十多种 agent：前四个通过 Solo 为每次会话加的 hooks 或插件准确上报（不改它们的配置），其余按各自的屏幕规则识别。

## 指挥：几个 agent 同时干活

- **状态一目了然：** 终端标签上是 agent 的图标、会话名和红绿灯状态点（工作中、等你回复、做完待看、出错），以及上下文用量；悬停看到模型、token、首字时间和输出速度。Solo 在后台时，agent 等你回复会发桌面通知。
- **工作区标签：** 一个项目一个标签，切换时终端、编辑器、文件树一起换；标签上显示这个项目里 agent 的状态，后台项目有 agent 在等你也看得到。
- **agent 看板：** 左侧栏列出所有 agent 和任务，等你回复的排在最前面。
- **agent 任务：** 一个任务一个 git worktree 和分支，互不干扰；做完合并、开 PR 或丢弃。工作区里有几个仓库时，一个任务可以同时横跨它们。[详细 →](features/parallel.md)
- **agent 也能指挥 Solo：** 终端里的 agent 可以用 `solo-cli agent` 查看、等待、给别的 agent 发消息，或者新开一个任务。

## 终端本身也好用

命令块、底部的输入编辑器、边输边弹的补全、失败命令一键把错误发给 agent、用一句话让你自己的 Claude Code 或 Codex 给出命令。[详细 →](features/terminal.md)

完整的功能列表见 [README](../../../README.md#全部功能)。

---

## 现在的状态

- **只有 Windows 版**，从 [Microsoft Store](https://apps.microsoft.com/detail/9NBW452JVWN6?mode=direct) 安装，在 Solo 里更新。
- **关掉 Solo，agent 照样在跑。** 每个终端的 shell 跑在一个小的后台进程里：退出 Solo、更新重启，甚至 Solo 崩溃，正在运行东西（agent、编译）的终端都会继续跑，Solo 再打开时自动连回去，画面也都在。空闲的 shell 还是随 Solo 一起结束。
- 不用注册账号，不发送遥测。默认只会定期检查更新（可以关掉），其余网络访问都由你用到的功能触发，详见[隐私说明](../../guide/zh-CN.md#9-隐私与网络)。
- 开源，GPL-3.0。
