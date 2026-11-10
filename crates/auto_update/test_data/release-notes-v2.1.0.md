Solo 2.1：agent 状态更准，常驻终端更可靠，Codex 的会话信息和 Claude 一样详细。
Solo 2.1: more accurate agent status, more reliable terminals that outlive Solo, and Codex sessions as detailed as Claude's.

[用户指南](https://github.com/lucas9801/solo/blob/main/docs/guide/zh-CN.md) · [User guide](https://github.com/lucas9801/solo/blob/main/docs/guide/en.md)

## 修复

- **agent 还在工作却显示"已完成"：**
  - Claude Code 长时间工作中途自动压缩上下文后，不再被当成结束；
  - 滚动 agent 的界面、切换窗口不再被当成你的输入；
  - 屏幕上明确显示 agent 在工作（"esc to interrupt"）时，以屏幕为准；
  - "已完成"的通知只在 agent 明确结束时才发。
- **常驻终端：**
  - 同时恢复很多终端时，后台进程不再有一半启动失败；没能常驻的终端在标签上有 ⚠ 标记；
  - 重新连上后，命令前面不再多出 `[?6c` 之类的乱码。
- **按轮审阅对准 agent 实际的目录：** 用 `codex resume` 等方式在别的目录工作时，快照、审阅、回滚都对着 agent 改的那个仓库。
- **远程开发：** SSH、WSL、开发容器的远程开发从来没能用（Zed 的服务器上没有对应版本的服务端），入口先隐藏，触发时会说明，不再请求 Zed 的服务器。
- 设置页的项目列表不再重复列出同一个目录。

## 新功能和改进

- **用量常驻显示：** 运行中的 agent 终端底部有一条细栏，显示上下文、模型、本会话和本轮的 token；标签上显示上下文百分比，超过 80% 变色。`terminal.agent_usage_display` 设为 `"tooltip"` 可以恢复只在悬停提示里显示。
- **Codex：** 标签提示、用量细栏和时间线显示 Codex 会话的标题、token、上下文和模型。
- **工作区标签：**
  - 名字固定为工作区的目录名，切换时不再改名；同名时加上级目录；
  - 主 worktree 只显示分支，不再显示 "main / 分支"；
  - 后台完成的 agent 在标签上显示 ✓，看过后消失；
  - 一个工作区里混着互不相关的项目时，提示拆成各自的标签（你确认后才拆，终端跟着移过去，程序不中断）。
- **`solo://` 链接：** 设置页复制的链接、分享的技能链接改用 `solo://`，安装程序为当前用户注册，在 Solo 里打开，不再交给 Zed。

## Fixes

- **Agents shown as done while still working:**
  - Claude Code compacting its context in the middle of a long turn no longer counts as the turn ending;
  - scrolling the agent's screen and switching windows no longer count as your input;
  - when the screen plainly shows the agent at work ("esc to interrupt"), the screen wins;
  - the "done" notification comes only when the agent plainly stopped.
- **Terminals that outlive Solo:**
  - restoring many terminals at once no longer leaves half of them without their background process; a terminal that couldn't get one shows ⚠ on its tab;
  - reconnecting no longer puts stray text like `[?6c` in front of the next command.
- **Per-turn review follows the agent's own folder:** an agent working elsewhere than its shell (`codex resume`, say) is snapshotted, reviewed and rolled back where it works.
- **Remote development:** SSH, WSL and dev container remote development never worked (Zed's servers have no remote server for Solo's version); its entries are hidden, using one explains why, and Zed's servers aren't asked.
- The settings window lists each project folder once.

## New and improved

- **Usage at a glance:** a running agent's terminal has a thin bar at the bottom with its context, model, and the session's and turn's tokens; its tab shows the context used, colored past 80%. Set `terminal.agent_usage_display` to `"tooltip"` to keep them in the tooltip only.
- **Codex:** the tooltip, usage bar and timeline show a Codex session's title, tokens, context and model.
- **Workspace tabs:**
  - named after the workspace's folder, steady when switching; same-named folders get their parent's name;
  - the main worktree shows just the branch, not "main / branch";
  - agents that finished in the background show ✓ on the tab until you look;
  - a workspace mixing unrelated projects is offered to be split into a tab each (only on your yes; terminals move along, still running).
- **`solo://` links:** links copied from the settings window and shared skills use `solo://`, which the installer registers for your user, so they open in Solo rather than Zed.

