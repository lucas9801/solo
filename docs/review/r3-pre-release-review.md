# R3 发布前代码审阅（Solo 2.2.0 + batch 2）

- 审阅对象：分支 `g-batch2`，提交 `255aa0e593`
- 比较基准：`2bdd6a1be5`（2026-10-30，从 Zed 导入的基线）。`git diff 2bdd6a1 HEAD --stat -- crates` 共 324 个文件，+56,547 / -3,456。
- 方式：只读代码，没有编译，也没有运行。
  - 每条结论都对照代码核实过。其中以下几条由审阅人再次抽查过原文：OSC 6973 无认证、`--timeout` panic、Reject 删除、teardown、更新器缺 digest 时放行、CI tag 内插。
  - 依赖运行时行为（Git for Windows、Claude/Codex 版本、Windows 安全机制）而没能实测的，标为 **待确认**。
- 严重程度：严重 > 高 > 中 > 低。本次**没有发现严重级问题**。

## 统计

| 严重程度 | 数量 | 其中待确认 |
|---|---|---|
| 严重 | 0 | 0 |
| 高 | 3 | 1 |
| 中 | 13 | 2 |
| 低 | 45 | 5 |

---

## 一、高

### H1 OSC 6973（agent hook 上报）没有认证：任意终端输出都能伪造 agent 状态，并触发快照、`send` 等动作

- **位置**
  - `crates/terminal/src/terminal.rs:1967-1975`：解析后直接 emit。对比 OSC 633，那里有 nonce 校验。
  - `vendor/alacritty_terminal/src/term/mod.rs:1901-1905`：备用屏幕下也会转发。
  - `crates/terminal/src/shell_integration.rs:705-731`：`working_directory` 只检查 `is_absolute`，UNC 路径 `\\host\share` 也能通过。
  - `crates/terminal_view/src/agent_status.rs:343-366`
  - `crates/terminal_view/src/agent_turns.rs:66-67`、`:700-703`
  - `crates/terminal_view/src/agent_turns/snapshot.rs:107`、`:398-410`
  - `crates/terminal_view/src/agent_context.rs:151-173`
- **触发**：终端里输出一段 `ESC ] 6973 ; agent ; prompt ; … ; claude ; <编码后的目录> BEL`。下面几种情况都会触发：
  - `cat` 恶意文件；
  - ssh 远端输出、`curl` 输出；
  - 在 bash 下 `cd` 进一个名字里带 BEL 或 ESC 的目录。原因是 `bash.sh:59-62` 只编码 `%` 和空格，OSC 7 会被提前结束，后续字节被当作新序列，见 L-S8。
- **后果**
  1. 以报文给出的目录（可以是 UNC）为 `current_dir`，自动运行 `git rev-parse`、`git add --all` 做快照，用户无需任何交互。
     - 后果一：向攻击者主机发起 SMB 连接，可能泄露 NTLM 哈希。
     - 后果二：对任意大目录做快照，消耗 CPU 和磁盘。
     - 如果该目录是攻击者可控的仓库，`.git/config` 里的 filter 驱动是否会被执行：**待确认**。`core.fsmonitor` 已经在 `snapshot.rs:380` 关掉。
  2. `runs_agent` 变为 true 后，`solo-cli agent send` 和 `enable_remote_control`（`agent_context.rs:780-791`）会把文本加回车直接写进一个普通 shell，等于执行命令。
  3. 伪造 NeedsInput 通知。伪造的 session_id 会让 `output` 读到同用户其他会话的 transcript。`sent_at_ms = u64::MAX` 会压住所有真实 hook（`agent_status.rs:122-127`）。
- **修复**
  - 让 hook 从环境变量读取一个只有 Solo 知道的 token（例如复用 `SOLO_SHELL_NONCE`，或新增 `SOLO_AGENT_HOOK_TOKEN`），写进序列，终端侧对不上就丢弃。
  - 至少做到：拒绝 `\\` 和 `//` 开头的目录，要求目录在项目内或 shell 当前目录之下；没有 token 的报文只用于显示，不触发快照和 `send`。

### H2 Reject All 会删除本轮之前就存在、只是之前被忽略的文件，而且无法撤销

- **位置**
  - `crates/terminal_view/src/agent_turns/review.rs:526-542`：`base` 为 None 时直接 `fs.remove_file`。
  - 数据来自整树前后 diff：`agent_turns.rs:464-465`。
  - 快照遵守 .gitignore：`snapshot.rs:248-252`，`add --all --ignore-errors`。
- **触发**
  - 本轮中 .gitignore 删掉了某条规则（如 `.env`、`data/`），这些早已存在的文件在结束快照里被记成 Added。
  - 或者开始快照时文件被锁，被 `--ignore-errors` 跳过。
- **后果**：审查页把它们当作"本轮新建的文件"，Reject 或 Reject All 会把它们永久删除。开始快照里没有这些文件，所以无法撤销。整体回滚走另一条路径，那条路径有撤销点。
- **修复**
  - 在开始快照时额外记录 `ls-files --others --ignored` 的结果，或记录 mtime，删除前确认文件当时确实不存在。
  - 删除前把文件移到回收站或快照存储，而不是直接删除。
  - Reject All 遇到 Added 文件时单独列出，让用户确认。

### H3（待确认）目录联接（junction）会被当成普通目录：回滚可能改写或删除仓库外的文件

- **位置**：`snapshot.rs:252`（`add --all`）、`:310-325`（`checkout-index --force`）、`:335-347`（对 Added 路径调用 `remove_file`）。
- **前提（待确认）**：Git for Windows 的 lstat 只把 `IO_REPARSE_TAG_SYMLINK` 当成链接，`mklink /J` 建出的联接会被当成目录递归进去。
- **触发**：工作树里有一个没被忽略的联接，例如 agent 在本轮中建了 `link -> C:\Users\x\Documents`。
- **后果**
  - 回滚时会穿过联接逐个删除 `link/...` 下的文件，或用 `checkout-index` 覆盖它们。
  - 审查页只显示相对路径，用户很难察觉删的是仓库外的文件。
- **修复**
  - 删除或写入前，对路径的每一级父目录做 `symlink_metadata` 检查，遇到 reparse point 就跳过并报告。
  - canonicalize 之后确认路径仍在 `work_tree` 之内。

---

## 二、中

### M1 `solo-cli agent wait --timeout 1e20` 会让整个 Solo 崩溃

- **位置**：`crates/terminal_view/src/agent_cli.rs:214-223`。只过滤了 `is_finite() && >= 0`，随后调用 `Duration::from_secs_f64`，超过约 1.8e19 秒时 panic。
- **触发**
  - `parse()` 在 `run()` 的 `:422` 执行，早于 `:432` 的 `agent_control` 检查，所以设置成 off 也挡不住。
  - `run()` 跑在主进程 `open_listener.rs` 的 `handle_cli_connection` 里。panic hook 在 `crates/crashes/src/crashes.rs:636` 调用 `abort()`。
- **后果**：任何同用户进程（包括终端里的 agent）都能让 Solo 立刻退出，未保存的编辑丢失。
- **修复**：改用 `Duration::try_from_secs_f64`，并设上限（例如 1 天）；把解析放到权限检查之后，同时避免在主进程里 panic。

### M2 回滚或撤销中途失败时，撤销点丢失，工作树停在混合状态

- **位置**
  - `snapshot.rs:299` 先算出撤销点 `current`，但 `:325` 的 `check()`、`:332` 的 `result?`、`:344` 的 `return Err` 直接返回，`current` 没有交给调用方。
  - `review.rs:653-657` 只显示一条错误。
- **触发**：Windows 上文件被占用（编辑器、开发服务器、杀毒软件），`checkout-index` 只写成了一部分；或者遇到 M3 的情况。
- **后果**：一部分文件已回滚、一部分没有，界面上也没有"撤销回滚"可点。
- **修复**：失败时也返回撤销点（例如 `Err((undo, error))`）；或者处理完所有文件后再汇总错误。

### M3 agent 新建嵌套仓库后回滚必然失败；group 任务的快照基本无效

- **位置**：`snapshot.rs:340`。
- **触发一**：agent 在本轮中 `git clone` 了一个子仓库。它在 diff 里是 gitlink（160000）。回滚时对目录调用 `remove_file`，Windows 返回 PermissionDenied，于是返回 Err，再叠加 M2。
- **触发二**：多仓库任务的 group 目录下都是 linked worktree，被当成嵌套仓库，agent 在这些 worktree 里的改动不会进入快照。
- **修复**：用 `diff-tree --raw` 读出 mode，跳过 160000；group 目录改为对每个子 worktree 分别建快照。

### M4 回滚确认页内容会过期：确认之后发生的改动会被静默丢弃

- **位置**：`review.rs:245-248` 在加载确认页时快照一次；`review.rs:622-637` 加 `snapshot.rs:299` 在真正回滚时重新快照，并丢弃当时的一切。
- **触发**：确认页打开后，agent 或用户仍在写文件。代码也没有检查本终端有没有正在进行的轮次。
- **后果**：没展示给用户的改动被回滚。撤销点还在，但用户不一定会发现。
- **修复**：把确认页加载时的树 id 传给 `restore`，不一致就中止并重新展示；有正在进行的轮次时禁止回滚，或给出警告。

### M5 本轮改动按整棵工作树前后 diff 归属：同期的用户改动和其他 agent 的改动都算进来

- **位置**：`agent_turns.rs:464-465`；`snapshot.rs:106-149` 在仓库内时用的是 `--show-toplevel`，不是 agent 的工作目录；`agent_turns.rs:335-363` 的开始快照是异步拍的。
- **触发**：本轮期间用户在编辑器里保存了文件，或者同一仓库另一个终端的 agent 也在改文件。
- **后果**：这些改动出现在本轮审查里，Reject All 会一并撤掉，而且没有撤销。大仓库首次快照可能要几秒，agent 在这几秒里的改动会被算进"之前"。
- **修复**：结合文件事件或 hook 上报的路径，标出"非本 agent 改动"；Reject All 前提示数量；开始快照完成后才把这一轮标为开始。

### M6 仓库里的 `.solo/tasks.json` 未经信任检查就执行命令

- **位置**
  - `crates/terminal_view/src/agent_tasks/config.rs:33-40`：定义 `setup`、`teardown`、`env`。
  - `agent_tasks.rs:101`、`:236-242`、`:823`：setup 被敲进终端执行。
  - `agent_tasks.rs:1682-1690` 加 `config.rs:205-236`：teardown 用 `cmd.exe /D /S /C` 执行。
  - `agent_tasks.rs:100`、`:1477-1481`：env 被注入任务终端。
  - `agent_cli.rs:799`：`solo-cli agent new-task` 也走这条路径。
  - 在 `agent_tasks*` 里没有找到任何 trust 检查。
- **触发**：克隆一个恶意仓库后，对它新建或删除 agent 任务。
- **后果**：以用户身份执行仓库作者写的任意命令。另外，`--agent` 允许值里带 `.`（`agent_tasks.rs:265-271`），例如 `--agent x.cmd` 会执行 worktree 里的同名脚本。
- **修复**：接入上游的 `project/src/trusted_worktrees.rs`；或者首次使用时展示 setup、teardown、env 的内容，让用户确认。

### M7 更新包和远程服务器缺少 GitHub digest 时，不校验就执行

- **位置**
  - `crates/auto_update/src/solo_update.rs:407-422`：`None => log::warn!` 之后照样返回 Ok。
  - `solo_update.rs:115-118`、`:146`：`sha256_of_digest` 在格式不符时返回 None。
  - `crates/auto_update/src/solo_remote_server.rs:97` 复用了同一个函数。
- **触发**：release 资产没有 `digest`（例如早期资产、手动重传，或 API 字段变了），或者格式不是 `sha256:<64hex>`。
- **后果**：未经校验的安装包被 helper 静默安装（`/SILENT`），未经校验的 Linux 二进制被上传到远端执行。
- **修复**：缺少 hash 时直接失败（fail closed），退回到打开 release 页面让用户手动下载。

### M8 期望的 hash 和安装包来自同一渠道，客户端也不做签名校验

- **位置**：`solo_update.rs:51-52`、`:288-311`、`:340-424`；`crates/solo_update_helper/src/main.rs:100-105`；`.github/workflows/solo-package.yml:53`（SIGN 默认关闭）。
- **后果**：校验只能防传输损坏。仓库或账号被攻破、发出恶意 release 时，所有用户都会自动安装它。
- **修复**：下载后或安装前调用 WinVerifyTrust，并钉住签名者（SignPath 证书）；开启签名后把"已签名"作为必要条件。远程服务器的 hash 可以由 CI 在构建时写入客户端。

### M9 终端 host 管道可被抢注，客户端也不验证服务端身份

- **位置**
  - host 端：`crates/solo_terminal_host/src/host.rs:500`。后续实例用 `Pipes::make(.., false)` 创建，不带 `FILE_FLAG_FIRST_PIPE_INSTANCE`；`:599-601` 先关闭旧实例再建新的，中间有空窗期。
  - 客户端：`crates/terminal/src/host.rs:351-413`，只检查 header 文本；`:383-386` 直接采信其中的 `child_pid`、`nonce`、`terminal_id`。
  - 管道名保存在 DB 里：`terminal_view.rs:2589-2601`。
  - `\\.\pipe\` 目录任何本地用户都能枚举，所以名字等于公开。
- **触发**：host 退出后（或在 keep 模式的重连空窗里），另一个本地账户抢先建同名管道，下次 Solo 恢复终端时就会连上去。
- **后果**
  - 用户在该终端的输入（包括密码）被对方读到。
  - 对方能伪造输出，并自选 nonce，从而伪造 OSC 633。
  - 在空窗期抢注还会让 host 在 5 秒后杀掉程序（`host.rs:414-424`、`:605`），构成 DoS。
  - 客户端 `open_pipe`（`:425`）使用默认的 SecurityImpersonation 级别。如果抢注方有 SeImpersonatePrivilege，就能模拟 Solo 用户。
- **前提**：需要机器上有另一个本地账户。单用户家用机上风险低。
- **修复**
  - host 用 `DisconnectNamedPipe` 断开后复用同一个实例，永不释放名字。
  - 客户端用 `GetNamedPipeServerProcessId` 拿到服务端进程，校验它的 token 用户 SID。
  - 打开管道时加 `SECURITY_IDENTIFICATION` 的 QoS 标志。
  - 把 nonce 存进 DB，重连时比对 header 里的值，而不是直接采信。

### M10 agent CLI 通道不按调用方授权（SOLO_TERMINAL_ID 可伪造、主管道用默认安全描述的评估）

- **主管道**：`crates/zed/src/zed/windows_only_instance.rs:66-75`（上游代码），安全属性传的是 None。
  - 默认 DACL：LocalSystem、Administrators、创建者 owner 有完全控制；Everyone 和 Anonymous 只有读权限。
  - 管道是 `PIPE_ACCESS_INBOUND`，发请求需要写权限。所以其他标准用户、低完整性进程、AppContainer 都**无法**下发请求。能驱动这个通道的只有同用户 medium 完整性进程、管理员和 SYSTEM。默认安全描述本身**不构成**跨用户漏洞。
- **SOLO_TERMINAL_ID**：它只用于解析 `self`、在 `ls` 里标记 current、选择目标窗口和工作目录（`agent_cli.rs:360-365`、`:378`、`:635`、`:757-761`），不是授权依据。所以伪造它**拿不到额外能力**。它本身也只是 32 位的公开值（`shell_integration.rs:49`）。
- **真正的问题是 agent 之间的越权**：任何能连上通道的进程都可以执行以下命令，唯一的闸门是全局设置 `agent_control`（`agent_cli.rs:432-444`）。
  - `ls`：列出所有终端。
  - `output`：读取 agent 的回答。
  - `send`：向任意 agent 终端粘贴文本并回车（`:498-501`）。
  - `open`：打开任意文件。
  - `new-task`：执行 tasks.json 里的命令，见 M6。

  因此，一个受权限模式限制的 agent（或被提示注入的 agent）可以借助另一个以 bypass 模式运行的 agent 间接执行命令。
- **修复**：给每个终端发一个 128 位的秘密 token（放在环境变量里），由 Solo 映射到调用方；`send` 和 `new-task` 默认只允许作用于自己创建的终端，否则需要用户确认；考虑把默认值改成 `read_only`。

### M11 渲染路径里有同步文件 IO：`git_branch`

- **位置**
  - `crates/terminal_view/src/input.rs:1090` → `:1013-1028` → `:1434-1460`：`TerminalInput::render` 里从 cwd 逐级向上做 `is_dir`/`is_file` 和 `read_to_string(HEAD)`。
  - `crates/terminal_view/src/tab_title.rs:302`：在 `terminal_view.rs:1520` 的 `Event::Wakeup` 处理里（主线程）也会调用。
- **触发**：cwd 位于网络盘、UNC 路径或休眠的磁盘上；cwd 被伪造的 OSC 7 改成 UNC，见 L-S8；或者 `.git` 文件写着 `gitdir: \\attacker\...`。
- **后果**：UI 卡顿，访问 UNC 时可能泄露 NTLM 哈希。
- **修复**：放到 `background_spawn` 里执行并缓存结果；不跟随指向 UNC 的 gitdir。

### M12（待确认）用户 index 里的暂存 blob 被 gc 后，该仓库的快照会持续失败

- **位置**：`snapshot.rs:229-231` 用用户的 index 做种子 index；`:262` 执行 `write-tree`。
- **触发**：种子 index 引用了一个已经悬空、随后被 `git gc` prune 掉的 blob，而私有 index 按 stat 认为文件干净，不会重新哈希。
- **后果**：之后每一轮都提示"快照失败"，直到 store 闲置 7 天后被清空。不会丢数据。
- **修复**：`write-tree` 报 invalid object 时，删除私有 index，重新播种后重试一次。

### M13（待确认）solo:// 把 zed:// 的全部入口暴露给网页

- **位置**：`crates/zed/src/zed/open_listener.rs:161`、`:170-226`、`:290-326`；`script/solo.iss:79-82`。
- **说明**：Solo 只是加了一个协议别名，没有新增处理器。但通过 solo:// 打开的入口包括：
  - `solo://file/<任意路径>`
  - `solo://ssh/user@host/...`：连接任意 SSH 主机，之后会上传 remote server。
  - `solo://git/clone?repo=`
  - `solo://git/commit/<sha>?repo=`

  没有发现能运行命令、向终端输入或写入设置的路径。ssh 和 git clone 在执行前是否一定弹出确认框：**待确认**。
- **修复**：对外部协议来的 file、ssh、git 请求增加一次确认，或者改为白名单。

---

## 三、低

### 3.1 进程启动与 shell 文本

- **L-S1** `crates/terminal_view/src/input/ai_command.rs:247-255`
  - 问题：Unix 下用固定的 `/tmp/solo-ai-command` 目录，不像 `shell_integration.rs:183-203` 的 `scripts_dir` 那样校验属主并设成 0700。同机其他用户可以预先建好这个目录，替换 `claude-settings.json`（植入 hooks）或用符号链接覆盖文件。
  - 定级：Windows 下 `%TEMP%` 是用户私有目录，所以当前只发布 Windows 版时定为低。
  - 修复：复用 `scripts_dir` 的校验，或改用 `tempfile::TempDir`。
- **L-S2** `shell_integration.rs:255`、`:354`
  - 问题：PowerShell 单引号字符串只把 `'` 加倍。PowerShell 也把 U+2018 到 U+201B 当作单引号。
  - 后果：路径里含 `’` 时（例如用户名 O’Brien），集成脚本和补全进程会加载失败。
  - 修复：这四个字符也加倍。
- **L-S3（待确认）** `shell_integration.rs:594-599`
  - 问题：Claude Code 的 hook 设置使用了 `"args"` 字段。如果所用版本不支持该字段，`command` 会交给 shell 执行，而 exe 路径没有加引号，含空格时会失效。
  - 修复：确认最低版本，或者在 `command` 里放加引号的路径。
- **L-S4（待确认）** `shell_integration.rs:293-297`、`:527-534`
  - 问题：Codex hook 命令如果经由 `cmd /C` 执行，路径里含 `&`、`(`、`)`、`^` 时会失效。
- **L-S5** `shell_integration.rs:213-233`、`:335-338`、`:551`
  - 问题：`claude-settings.json`、copilot 和 opencode 插件、`bash-integration.sh` 都用固定文件名。多个 Solo 构建或版本并存时会互相覆盖，hook 指向另一个构建的 exe，那个构建被删除后 hook 会静默失效。
  - 另外：partial 文件名只含 pid，补全进程的两个线程会同时写 `powershell-completer-*.ps1`（`powershell.rs:193`、`:267-268`）。
  - 修复：所有文件都按内容哈希命名。
- **L-S6** `crates/terminal_view/src/agent_resume.rs:82-89`、`:102-119`
  - 问题：恢复会话命令里的程序名只用 `file_stem` 判断，像 `x\claude.x;calc` 这样的值能骗过去。来源是保存的命令块，只有 nonce 为空（自定义了 shell 参数）时才可能被输出伪造。
  - 后果：命令需要用户按回车才会执行。
  - 修复：程序 token 只允许路径字符。
- **L-S7** `crates/terminal_view/src/agent_context.rs:146`、`:340`；`terminal.rs:3051`
  - 问题：在非 bracketed paste 模式下，只拦截了 `\n`。`\r`、ESC、`\x03` 等字符会原样送出，可以多次提交或注入按键序列（例如 `\x1b[Z`）。
  - 修复：除 `\t` 外拒绝所有控制字符。
- **L-S8** `shell_integration.rs:1043-1067`、`bash.sh:44-46`、`:59-62`
  - 问题：OSC 7 和 OSC 133 没有 nonce 校验，cwd 可以被伪造成 UNC（`path_from_file_uri` 接受 `//host/share`）。bash 只编码 `%` 和空格，目录名里的控制字符会截断序列（这是 H1 的一种触发方式）；633 E 只转义了 `\n`。
  - 修复：拒绝 UNC；bash、zsh、fish 对小于 0x20 的字节和 0x7f 都做百分号编码。
- **L-S9** `ai_command.rs:438-455`、`:443`、`:487`、`:492-504`；`input.rs:140-142`
  - 问题一：生成命令时，在后台线程同步轮询最多 90 秒。
  - 问题二：`try_wait()?` 出错时不 kill 子进程；非 Windows 下只杀直接子进程。
  - 问题三：`clean_answer` 不过滤控制字符。
  - 问题四：注释说不会加载 MCP，但 Codex 仍会启动全局 config 里的 MCP。
  - 已确认：参数里没有 bypass 类开关，提示词经 stdin 传入。
- **L-S10** `crates/terminal/src/host.rs:425`、`terminal.rs:1251`
  - 问题：`SOLO_TERMINAL_HOST_RECONNECT` 不校验前缀。值可以来自 settings 的 `terminal.env`，设成 `\\server\pipe\x` 时会发起 SMB 连接。
  - 修复：要求前缀为 `\\.\pipe\solo-terminal-`。

### 3.2 terminal host、CLI、hook

- **L-H1（待确认）** `solo_terminal_host/src/host.rs:26`
  - 问题：SDDL `D:P(A;;GA;;;OW)` 不带完整性标签。同用户的低完整性进程理论上能以只读方式打开 `-out` 管道，读到回放输出；在 keep 模式下还会让 host 卡在 `:361`。
  - 另外：Solo 以管理员身份运行时，OW 是 Administrators 组。
  - 修复：改用显式的当前用户 SID，并加上 `S:(ML;;NWNR;;;ME)`。
- **L-H2** `windows_only_instance.rs:34`、`:77-81`、`:104`（上游代码）
  - 问题：主管道名是全局的，但实例互斥量只在 session 内有效，也没有 `FILE_FLAG_FIRST_PIPE_INSTANCE`。另一个 session 的用户可以抢注，冒充 Solo 读取 CLI 参数，并返回任意 stdout（`cli/src/main.rs:501` 会原样打印）。
  - DoS 情形：**待确认**。
- **L-H3** `solo_terminal_host/src/host.rs:451-461`、`:601`
  - 问题：面对恶意的同用户客户端，只关闭 `-ctl` 而不关闭 `-in` 会让 join 永久阻塞；客户端不读数据时，输出线程会持锁阻塞。
  - 已确认：协议是定长的，没有无界分配；`R` 消息里 u16 转 i16 只会导致 resize 失败，不会 panic。
- **L-H4** `solo_terminal_host/src/log.rs:57`、`host.rs:564`
  - 问题：日志没有大小上限，每次 resize 都记一行。只在 host 启动时清理 7 天前的日志。日志里有命令行和工作目录，不记录 env 和终端内容。
- **L-H5** `terminal/src/host.rs:450-463`
  - 问题：读取 header 时没有超时，抢注者可以让后台线程永久挂起。
  - 另外：`:167`、`:178` 在主线程上同步写管道，只有缓冲写满才会阻塞。
- **L-H6** `agent_cli.rs:548-587`
  - 问题：`wait` 不带 `--timeout` 时，CLI 退出后 Solo 端仍每 250ms 轮询一次，任务泄漏。
- **L-H7** `crates/cli/src/main.rs:548-549`、`crates/solo_agent_hook/src/main.rs:27`
  - 问题：`std::env::args()` 遇到非 Unicode 参数会 panic，只影响该进程本身。
  - 修复：改用 `args_os`。
- **L-H8** `solo_agent_hook/src/main.rs:44`、`:298-306`
  - 问题：stdin 读取没有上限；`SOLO_AGENT_CONSOLE_PID` 可以让它 attach 到任意同用户控制台。
  - 已确认：输入校验（session id、agent 名、目录编码）做得完善。

### 3.3 更新器与远程服务器

- **L-U1** `solo_update.rs:1243`、`:138-141`
  - 问题：`download_dir.join(&installer.name)` 只校验了文件名的前缀和后缀，没有拒绝 `\` 和 `..`。只有 API 响应被篡改时才可利用。
  - 修复：只取 `file_name()`。
- **L-U2** `solo_update.rs:348-352`、`:1085`、`:1362`、`:1411`
  - 问题：下载地址和 `html_url` 都没有限定 https 和 github.com，`html_url` 会被直接传给 `open_url`。
- **L-U3** `solo_update.rs:296-301`、`:382-396`
  - 问题：API 响应读取没有上限；下载超过 `size` 时不会提前中止。
- **L-U4** `solo_update.rs:407-417` 对比 `:1188`、`:1289`
  - 问题：校验和执行之间可能隔几个小时（例如"退出时安装"），执行前不再校验。文件在 `%TEMP%` 下，只有同一用户能篡改，不跨提权边界。
- **L-U5** `solo_update_helper/src/main.rs:46-49`、`:122-137`
  - 问题：helper 按 PID 等待 Solo 退出，PID 可能被复用。PID 解析失败时会直接开始安装。
- **L-U6** `solo_update.rs:847-851`、`:818`、`:1244`
  - 问题：按 pid 分的下载目录从不清理，每次更新会留下约 100MB 的安装包；失败提示里给出的日志路径用的是新进程的 pid，路径不对。
- **L-U7** `solo_update.rs:492`、`:1093`、`:1131`、`:1181`、`:1200`
  - 问题：主线程上有小文件的同步 IO。
- **L-U8** `solo_update.rs:390`
  - 问题：`written.min(total) * 100` 在 total 极大时会溢出（debug 构建下 panic）。
  - 修复：改用 `saturating_mul`。
- **L-U9** `solo_remote_server.rs:56`、`:84`
  - 问题：缓存只检查文件是否存在，以后不再校验；`*.download-<pid>` 临时文件从不清理。

### 3.4 会话记录解析（agent_sessions）

- **L-A1** `crates/agent_sessions/src/agent_sessions.rs:576-603`、`live.rs:44`、`:56-57`、`:92`
  - 问题：transcript 一次性整份读入内存（峰值约为文件大小的 3 倍），首次读取的全部条目在主线程上一次性应用。
  - 后果：几百 MB 的 transcript 会占用 GB 级内存并造成卡顿。
- **L-A2** `agent_sessions.rs:223-238`、`codex.rs:406`
  - 问题：token 计数用 `+` 和 `+=`。debug 构建下溢出会 panic，release 构建下回绕成错误数值。
  - 修复：改用 `saturating_add`。
- **L-A3** `codex.rs:100-121`、`:155`；`claude_code.rs:285`
  - 问题：递归遍历目录时用的 `is_dir()` 会跟随 junction，遇到环会死循环；首行读取和 `read_to_string` 都没有上限。
- **L-A4** `agent_sessions.rs:580`
  - 问题：只靠 `len < offset` 判断文件被重写。文件被换成一个更长的文件时，会从旧偏移继续读，数据错乱，但不会 panic。

### 3.5 agent 任务与 git

- **L-G1** `agent_tasks/git.rs:536-539`
  - 问题：`diff --no-index` 没有加 `--no-ext-diff` 和 `--no-textconv`，会对最多 200 个文件启动用户配置的外部 diff 工具。
- **L-G2** `git.rs:649-663`
  - 问题：`worktree remove --force` 失败后无条件 `remove_dir_all`，绕过了 `git worktree lock` 的保护。兜底删除失败时直接返回，prune 和 `branch -D` 都没执行。
- **L-G3** `agent_tasks.rs:1533`、`git.rs:483`
  - 问题：删除任务的确认页不列出被忽略的文件（如 `.env`、本地数据库），这些文件会随 `--force` 被静默删除。
- **L-G4** `git.rs:322-333`、`:463`、`:499`、`:521`、`:593`
  - 问题：分支用短名传给 git，有同名 tag 时会解析到 tag。`base_branch` 读自 git config 且不校验，以 `-` 开头时会被当成选项。
  - 修复：传 `refs/heads/<name>`，并加 `--end-of-options`。
- **L-G5** `git.rs:599-605`
  - 问题：merge 因钩子失败（而不是冲突）时直接 bail，没有 abort，base worktree 会留下一个进行中的合并。
- **L-G6** `snapshot.rs:437-454`
  - 问题：用 lossy UTF-8 解析文件名，非 UTF-8 文件名会出错并触发 M2。Windows 上几乎不会出现。
- **L-G7（待确认）** `review.rs:342` 对比 `:1312`
  - 问题：`expected` 去掉了 BOM，`base` 没去。带 BOM 的文件第一行总会出现一个假 hunk，拒绝它可能写出双 BOM。
- **L-G8** `review.rs:526-542`
  - 问题：删除新建文件前只比对了 buffer，没有比对磁盘，存在很小的竞态窗口。
- **L-G9** `snapshot.rs:493`、`review.rs:268-285`、`:1359-1397`
  - 问题：快照对象库在使用期间没有大小上限，也不做 gc；打开审查页时每个文件起 2 个 git 进程，没有数量上限。
- **L-G10** `agent_tasks.rs:524`
  - 问题：diff 临时文件按分支名命名，多仓库任务里分支同名时会互相覆盖。
- **L-G11** `agent_tasks.rs:1263-1272`、`pull_request.rs:307-341`
  - 问题：第三方 PR 评论原样粘贴进 agent 输入，存在提示注入面。需要用户按回车才会提交。
- **L-G12** `agent_timeline.rs:1222`、`agent_tasks.rs:1477`、`input.rs:1610` 加 `workflows.rs:43`、`command_blocks.rs:383`、`terminal_view.rs:170`、`history.rs:111`、`:235`
  - 问题：主线程上有少量小文件读写和 stat。

### 3.6 CI / 供应链

- **L-C1** `.github/workflows/solo-package.yml:73`
  - 问题：`"${{ github.ref_name }}"` 直接内插进 pwsh 脚本。git ref 名允许 `$(`，所以一个特制的 tag 能在 runner 上执行命令。只有有 push 权限的人能创建 tag。
  - 修复：通过 `env:` 传入，脚本里用 `$env:REF_NAME`。
- **L-C2** `solo-package.yml:186`、`:375`
  - 问题：`choco install innosetup` 没有锁版本；`pip install` 锁了版本但没有 `--require-hashes`；WebRTC 缓存命中时不再做 SHA-256 校验（`solo-ci.yml:74-76`、`solo-package.yml:99-101`）。
- **已确认没有问题**
  - 所有第三方 action 都钉了 40 位 commit SHA。
  - 顶层权限是 `contents: read`，只在需要的 job 里提权。
  - 没有 `pull_request_target`；checkout 设了 `persist-credentials: false`。
  - 唯一的 secret `SIGNPATH_API_TOKEN` 只在 tag 触发时使用，PR 拿不到。
  - `install-solo.ps1` 没有 iex，也不下载远程内容。
  - `release-solo.ps1` 只做本地 git 操作，有版本和 tag 检查。

---

## 四、可被用户数据或外部数据触发的 unwrap / expect / panic（第 3 项）

做法：在 `git diff 2bdd6a1 HEAD -- crates` 里找出新增的 `.unwrap()` 和 `.expect(` 共 605 处，去掉测试文件和 `#[cfg(test)]` 之后剩 44 处，再逐处看上下文。另外人工检查了上述各模块里的切片、下标和算术。

**能被外部数据触发的 panic**

| 位置 | 输入来源 | 说明 |
|---|---|---|
| `crates/terminal_view/src/agent_cli.rs:220` `Duration::from_secs_f64` | solo-cli 参数 | 见 M1，会让主进程崩溃 |
| `crates/cli/src/main.rs:548-549`、`crates/solo_agent_hook/src/main.rs:27` `std::env::args()` | 非 Unicode 命令行参数 | 只有该进程本身退出（L-H7） |
| `crates/agent_sessions/src/agent_sessions.rs:223-238`、`codex.rs:406` 整数加法 | transcript 里的数值 | 只在 debug 构建 panic（L-A2） |
| `crates/auto_update/src/solo_update.rs:390` 乘法 | GitHub 返回的 size | 只在 debug 构建 panic（L-U8） |

**已核实安全的 44 处非测试 unwrap/expect**
- `paths.rs`、`new_process_modal.rs` 中的 `RelPath::from_unix_str(<常量>)`。
- `terminal_settings.rs:143-176`：设置值，default.json 保证一定存在，和上游的写法一致。
- `powershell.rs:84/169/180/237/239/258/301/307` 和 `spec.rs:29/44`：`lock().unwrap()`，只在锁中毒时 panic。
- `powershell.rs:214`：线程创建失败时触发。
- `review.rs:491`：`expect("just pushed")`，紧跟在 push 之后。
- `windows_resources.rs:128`：只在 build 期间执行。

**已核实安全的切片和下标**
- `solo_terminal_host` 中的定长缓冲。
- `shell_integration.rs:49/728/1056/1062-1064`，前面都有长度保护。
- agent_sessions 用 `chars().take` 截断，不会切在 UTF-8 字符中间。
- `ai_command.rs:384/389` 按 `char_indices` 切片。
- `agent_cli.rs:194/597`。

**待确认**：`crates/terminal_view/src/input/completion.rs:122` 的 `input[cursor..]` 只把 cursor 截到长度以内，没有检查字符边界，安全与否取决于调用方是否总传入合法的 offset。

---

## 五、建议发布前处理

1. **H1**：给 OSC 6973 加 token；至少拒绝 UNC 目录，并且不让 hook 报来的状态触发快照和 `send`。
2. **H2**：Reject 删除"新建文件"前，先确认文件在本轮开始时确实不存在，或者改为移到回收站。
3. **M1**：改用 `try_from_secs_f64`，改动只有一行。
4. **M7**：缺少 digest 时 fail closed。
5. **M2 + M4**：回滚失败时保留撤销点；确认页内容过期时中止回滚。
6. **M6**：tasks.json 接入 worktree trust。
7. **L-C1**：CI 里的 tag 改为通过 env 传入，改动只有一行。
