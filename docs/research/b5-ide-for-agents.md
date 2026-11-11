# B5 调研：把 Solo 的 IDE 能力开放给终端里的 agent

调研日期：2026-11-12。只读了官方文档和 Solo 源码，没有运行真实的 Claude Code，也没有写代码。
标"未验证"的内容既没有官方文档支持，也没有实际试过。

## 1. 结论

1. **Claude Code 已经内置 LSP 支持。** 装上官方 "code intelligence" 插件（如 `clangd-lsp`）后，
   Claude 会多出一个只读的 `LSP` 工具（定义、引用、hover、文件符号、工作区符号、实现、调用层级）；
   它编辑文件后，还会自动把语言服务器报的错误和警告放进上下文。不过语言服务器由 Claude Code
   **自己启动**：每个会话一份，要求 PATH 上有对应的二进制，配置固定写在插件的 `.lsp.json` 里，
   不能连接已经在运行的服务器（`transport: socket` 虽然能写，但实际仍走 stdio）。
   在 Solo 里同时跑着 Solo 自己的 clangd 和 Claude 的 clangd，Godot 这种大仓库就会被**索引两遍**，
   内存翻倍，这正是 G19 想控制的问题。
2. **IDE 集成协议（VS Code / JetBrains）只有一部分公开。** 官方文档写了传输方式
   （127.0.0.1 上的 `ws://`）、锁文件位置（`~/.claude/ide/<port>.lock`）、认证头名称，以及模型能看到的
   两个工具：`getDiagnostics` 和 `executeCode`。握手、锁文件的 JSON 格式和其余大约 10 个内部 RPC
   （openFile、openDiff、选区等）**没有官方文档**，只有社区从 VS Code 扩展逆向出来的 PROTOCOL.md。
   这个协议**不提供定义和引用**，对"少搜索"帮助有限；而且协议还在变（v2.1.285 起聊天面板换成了另一个
   `claude-vscode` 服务器），要跟着改。
3. **Solo 自己做 MCP server 最合适：** 复用 Solo 已经在运行的语言服务器，并且遵守 G19 的模式；
   Claude Code、Codex 等支持 MCP 的 agent 都能用；能用 Solo 现有的 `--settings` 包装加上
   `--mcp-config`，不写用户的 `~/.claude`。Zed 代码里已经有现成部件：MCP server 骨架
   `crates/context_server/src/listener.rs`，以及 agent 工具 `go_to_definition` / `find_references` /
   `diagnostics` / `symbol_locator`。
4. **"能不能少很多 Grep 和 Read"目前只是推测。** 建议**第一步先做实验，不写代码**：
   用官方 `clangd-lsp` 插件做"有 LSP"组，和不装插件的对照组比较。如果收益明显，再做 Solo 的
   MCP server（估计 5–7 个工作日），用它把"复用服务器"和"多 agent 通用"的价值补上。

## 2. 问题 1：Claude Code 现在能不能直接用语言服务器？

能（以官方文档为准）：

- Code intelligence 插件页：插件"gives Claude the live diagnostics and go-to-definition that your editor
  has"。它带来两项能力：
  - **Diagnostics after edits**：每次编辑后，自动拿到该文件的错误和警告；
  - **Code navigation**：多一个只读的 `LSP` 工具，"looks up symbols through the server instead of
    searching text"。
  插件本身**不包含语言服务器**，要用户自己装好二进制并放到 PATH 上。C/C++ 用 `clangd-lsp`，
  需要 `clangd`。
- Tools reference 的 "LSP tool behavior" 一节列出了 `LSP` 工具能做的事：跳到定义、查所有引用、
  看某个位置的类型、列出文件符号、按名字搜工作区符号、查接口的实现、追调用层级。没装插件时
  这个工具不启用。
- 插件 manifest 的 `lspServers` 字段：`command`、`args`、`transport`（"Claude Code accepts `socket`
  but runs every server over stdio"）、`workspaceFolder`、`diagnostics`（编辑后是否推送诊断，默认开）、
  `restartOnCrash` 等。每个文件扩展名只对应一个服务器，服务器在 Claude 第一次编辑该类文件时启动。
- 推荐弹窗：PATH 上已有 clangd、但没装插件时，Claude Code 会主动提示安装 `clangd-lsp`。

**对 Solo 意味着什么**

| 问题 | 说明 |
|---|---|
| 重复的语言服务器 | Claude 的 clangd 和 Solo 的 clangd 各自索引一遍 Godot。G19 把 clangd 列为 heavy server（`crates/project/src/lsp_store/solo_server_modes.rs` 中的 `is_heavy_server`），在 Solo 之外再起一份，G19 就管不到了。 |
| 配置不同 | Solo 用的是用户在 Solo 里配好的服务器设置（compile_commands 路径、参数、G19 的模式）；Claude 插件只认 `.lsp.json` 里写死的命令，需要用户另外配一遍。 |
| 只对 Claude 有效 | Codex、Copilot 用不上这个插件。 |
| Solo 能补什么 | ① 复用已在运行、已建好索引的服务器（没有冷启动，也不重复占内存）；② Solo 掌握的额外上下文：用户在编辑器里还没保存的内容、Problems 面板里的诊断、当前选区；③ 同一套工具给所有 agent 用。 |

因此 Solo 仍然值得做，但重点应放在**复用服务器 + 多 agent 通用**，不是"让 Claude 第一次拥有 LSP"。

可以考虑的一个变体：做一个"LSP 门面"，即 Claude 插件的 `command` 指向 `solo-cli lsp-facade`，
由 Solo 冒充语言服务器来回答 definition、references 等请求。这样能直接用上 Claude 原生的 `LSP` 工具
和"编辑后自动诊断"。缺点：要模拟完整的 LSP 生命周期和文档同步语义（didOpen/didChange 的版本号、
位置编码、诊断推送的时机）；只对 Claude 有效；Claude 的 LSP 客户端具体会发哪些请求、对时序有什么
要求，文档里都没写（**未验证**）。所以不建议作为第一选择。

## 3. 问题 2：Claude Code 和 VS Code / JetBrains 之间的 IDE 集成协议

### 官方公开的部分（VS Code 文档）

- 传输方式：扩展在 `127.0.0.1` 上随机选一个 10000–65535 的端口，用不加密的 `ws://`，端口不能配置。
- 认证：扩展每次激活都会生成随机 token，写进 `~/.claude/ide/<port>.lock`（文件权限 0600，目录 0700；
  设了 `CLAUDE_CONFIG_DIR` 时改写到 `$CLAUDE_CONFIG_DIR/ide/`）。CLI 连接时要在
  `X-Claude-Code-Ide-Authorization` 头里带上这个 token。
- 能力：
  - CLI 每次发送提示时，带上当前选区和活动文件路径；
  - diff 在 VS Code 原生的 diff 视图里打开；
  - 模型能看到的工具只有两个：`mcp__ide__getDiagnostics`（Problems 面板里的诊断，可以只看一个文件）
    和 `mcp__ide__executeCode`（在 Jupyter 内核里运行代码，要用户确认）；
  - 原文："The server hosts a dozen tools, but only two are visible to the model. The rest are internal
    RPC the CLI uses for its own UI, such as opening diffs, reading selections, and saving files."
- 连接方式：在集成终端里自动连接；外部终端里用 `/ide`；也可以用 `claude --ide` 启动参数
  （"connect to IDE on startup if exactly one valid IDE is available"）。
- JetBrains 文档描述的功能类似（diff 视图、共享选区、诊断；这一条只看了搜索摘要，没有通读原文），协议细节没有更多公开。

### 没有公开、只有逆向资料的部分

社区的 coder/claudecode.nvim 项目有一份 PROTOCOL.md，自称"based on reverse-engineering the VS Code
extension"。按这份资料：

- 锁文件里有 `pid`、`workspaceFolders`、`ideName`、`transport`、`authToken`；
- IDE 给终端设置 `CLAUDE_CODE_SSE_PORT` 和 `ENABLE_IDE_INTEGRATION=true`，CLI 读到后自动连接；
- 协议是 WebSocket 上的 JSON-RPC 2.0，"基于 MCP 2025-03-26"；
- 工具有 `openFile`、`openDiff`（会阻塞，直到用户保存或拒绝，返回 `FILE_SAVED` / `DIFF_REJECTED`）、
  `getCurrentSelection`、`getLatestSelection`、`getOpenEditors`、`getWorkspaceFolders`、`getDiagnostics`、
  `checkDocumentDirty`、`saveDocument`、`close_tab`、`closeAllDiffTabs`、`executeCode`；
- IDE 主动发给 CLI 的通知有 `selection_changed` 和 `at_mentioned`。

上面这些都**未经官方确认（未验证）**。

### Solo 实现它的可行性和风险

- **技术上可行。** 工作区已有 `async-tungstenite`（根 `Cargo.toml`），MCP 消息类型在
  `crates/context_server/src/types.rs`。Neovim、Emacs、Obsidian 都有社区实现；网上也能搜到给 Zed
  打的 Claude Code IDE 集成补丁（链接已经 404，**未验证**）。
- **收益：** 模型能用 `getDiagnostics`；diff 在 Solo 自己的 diff/审查视图里打开，Claude 改文件前可以
  在 Solo 里批准，这和 Solo 的 agent 审查功能（`agent_turns`）有交集；选区自动成为上下文。
  **但没有定义和引用**，对"减少 Grep/Read"帮助很小。
- **风险：**
  1. 协议大半没有文档，Claude Code 每次升级都可能改，而且改动不会提前通知；
  2. 要往用户的 `~/.claude/ide/` 写锁文件，等于往 Claude 自己的配置目录写东西；
  3. ws 端口在本机是开放的，安全只靠锁文件里的 token；
  4. 只对 Claude 有效。
- 工作量：大约 1.5–2 周（ws 服务器、12 个 RPC、openDiff 阻塞流程接入 Solo 的 diff UI、选区通知），
  之后还要长期跟进协议变化。

**结论：** 不作为第一步。等 MCP 那条路做完以后，可以单独评估"diff 在 Solo 里审批"这个体验值不值得做。

## 4. 问题 3：Solo 作为 MCP server

### 现有部件（源码位置）

| 部件 | 位置 | 说明 |
|---|---|---|
| MCP server 骨架 | `crates/context_server/src/listener.rs`（`McpServer`、`McpServerTool` trait、`add_tool`，用 schemars 自动生成 inputSchema） | 本仓库里**没有任何地方调用它**（只有定义）。它监听的是临时目录下的 AF_UNIX socket，Windows 版来自 `crates/net/src/async_net.rs`。 |
| LSP 查询 API | `crates/project/src/project.rs`：`definitions`、`type_definitions`、`implementations`、`references`、`prepare_call_hierarchy` / `incoming_calls` / `outgoing_calls`、`document_symbols`、`symbols`（工作区）、`hover`、`diagnostic_summaries`；`open_buffer` | 查询都以 buffer 为单位，所以要先在后台打开 buffer（不打开编辑器标签页）。 |
| 现成的 agent 工具 | `crates/agent/src/tools/go_to_definition_tool.rs`（111 行）、`find_references_tool.rs`（102）、`diagnostics_tool.rs`（258）、`symbol_locator.rs`（236，用"文件 + 1 基行号 + 符号名"定位，适合 LLM，不需要列号） | 用的是 `AgentTool` 接口，要改成 `McpServerTool`，核心逻辑可以直接复用。 |
| 回连通道 | `solo-cli agent ...`：`crates/cli/src/main.rs` 中的 `run_agent` 通过 `\\.\pipe\<app>-Named-Pipe` 加 ipc-channel 单次握手，把 `CliRequest::Agent { caller: SOLO_TERMINAL_ID }` 发给 Solo；Solo 端在 `crates/terminal_view/src/agent_cli.rs` 处理 | 已经能做到"终端里的进程连回启动它的 Solo"。 |
| 终端环境变量 | `crates/terminal/src/shell_integration.rs`：`SOLO_TERMINAL_ID`、`SOLO_CLI`、`SOLO_SHELL_NONCE`（每个终端一个，不公开）、`SOLO_CLAUDE_SETTINGS` | shell 脚本（`bash.sh`、`powershell.ps1`、`fish.fish`、zsh）把 `claude` 包装成 `claude --settings "$SOLO_CLAUDE_SETTINGS"`，在这里加 `--mcp-config` 就行。 |
| 只允许所有者访问的命名管道 | `crates/solo_terminal_host/src/host.rs`：`OWNER_ONLY = "D:P(A;;GA;;;OW)"`、`FILE_FLAG_FIRST_PIPE_INSTANCE`、`PIPE_REJECT_REMOTE_CLIENTS` | 新管道照搬这套设置即可。 |
| 终端在 Solo 重启后继续运行 | `crates/terminal/src/terminal.rs` 的 `HOST_RECONNECT_ENV`：重连时从终端宿主取回原来的 nonce 和 terminal_id | 终端里的 agent 会比 Solo 活得久，回连设计必须考虑 Solo 重启（见下文）。 |
| G19 模式 | `solo_server_modes.rs` 的 `AnalysisMode::{Full, OpenFilesOnly, Off}` | `OpenFilesOnly` 关闭了后台索引，此时 references 和工作区符号**不完整**，工具要在结果里说明这一点；`Off` 时不能为了回答查询而启动服务器。 |

### 连接方式比较

| 方式 | 做法 | 优点 | 缺点 |
|---|---|---|---|
| A. stdio 子进程转发（**推荐**） | `--mcp-config` 写 `{"mcpServers":{"solo":{"command":"<solo-cli>","args":["mcp"]}}}`。Claude 启动 `solo-cli mcp`，它连上 Solo 的专用命名管道，先发 hello（terminal_id + token），之后在 stdio 和管道之间原样转发 JSON-RPC 字节 | Claude 那边就是普通 stdio MCP server，最稳；不监听网络端口；转发进程很薄，Solo 重启时可以自己重连；Codex 也能用同样的配置 | 要新增一个 `solo-cli` 子命令和管道服务端 |
| B. 本地 HTTP 端口 | Solo 在 127.0.0.1 上开 MCP HTTP 端口，配置里写 `url`，headers 里带 `${SOLO_IDE_TOKEN}`（`.mcp.json` 和 `--mcp-config` 支持环境变量展开） | 不需要转发进程 | 本机所有进程都能连这个端口，只靠 token 防护；端口冲突；Solo 重启后端口会变，而终端里的 env 是旧的 |
| C. 直接用 listener.rs 的 AF_UNIX socket | 让 agent 用支持 unix socket 的 MCP 客户端连接 | 代码现成 | Claude Code 没有 unix socket 传输（**未验证**）；socket 路径在临时目录里，每次重启都变 |

**推荐 A**。Solo 端可以把 `listener.rs` 的 `serve_connection` 抽成"接受任意 AsyncRead/AsyncWrite"，
再从命名管道喂给它，这样可以复用它的工具注册和 JSON-RPC 分发。

### 安全设计（重点：只允许那个终端里的 agent 连接）

1. **管道只允许所有者访问、拒绝远程客户端：** DACL 用 `D:P(A;;GA;;;OW)`，加上
   `PIPE_REJECT_REMOTE_CLIENTS`。第一个实例用 `FILE_FLAG_FIRST_PIPE_INSTANCE` 创建，防止别的进程
   抢先占用这个名字来冒充 Solo。管道名按 Solo 的数据目录派生，保持固定，Solo 重启后名字不变。
   转发进程可以再用 `GetNamedPipeServerProcessId` 核对服务端进程的映像路径是不是 Solo（可选，
   **未验证**开销）。
2. **每个终端一个 token：** 建终端时生成 128 位随机数，放进环境变量 `SOLO_IDE_TOKEN`。不写进
   命令行（命令行比环境变量更容易被别的进程看到），也不写进 `--mcp-config` 文件（转发进程从
   环境变量读，前提是 stdio 子进程继承 claude 的环境，**未验证**；不继承的话就在配置里写
   `"env": {"SOLO_IDE_TOKEN": "${SOLO_IDE_TOKEN}"}`，官方文档确认 `env` 字段支持 `${VAR}` 展开）。
   Solo 用常量时间比较 `(terminal_id, token)`。通过以后，这个连接**绑定到那个终端**：只能查询该终端
   所在项目的 worktree，路径一律按这个项目解析。
3. **Solo 重启后还能用：** token 跟 terminal_id、nonce 一起交给终端宿主保存（`host.rs` 的 Spec /
   重连握手）。重连后 Solo 能取回 token，旧终端里的 agent 照样能用。
4. **为什么不直接用 SOLO_TERMINAL_ID：** 现在 `solo-cli agent` 只用 `SOLO_TERMINAL_ID` 识别调用方，
   而它是可以被伪造的（任何本用户进程都能自称某个终端），主管道也是用默认安全属性创建的
   （`windows_only_instance.rs` 中 `CreateNamedPipeW(..., None)`）。只读的 LSP 查询风险不大，
   但新通道不应该沿用这个弱点。
5. **能力上的限制：** 第一版只提供**只读**工具（定义、引用、符号、hover、诊断），标
   `readOnlyHint`；限制输出大小（Claude Code 默认把 MCP 输出截到 25k token，超过 1 万 token 会告警）；
   每次查询设超时；受 `agent_control` 设置控制，再加一个总开关。远程、WSL、SSH 终端不设这些环境变量。
6. **剩下的风险：** agent 启动的子进程也会继承 token，它们和 agent 同属一个信任域，可以接受。
   同一用户下的恶意进程可以读别的进程的环境变量，Windows 上这本来就是同一个信任边界，
   命名管道的 ACL 已经挡住了其他用户。

### 和"编辑后自动诊断"对齐

Solo 已经通过 `--settings` 注入 hooks，可以再加一个 `PostToolUse`（matcher 为 `Edit|Write`）的
`mcp_tool` hook，调用 `solo` server 的 `diagnostics`，并把结果作为 `additionalContext` 放进上下文。
官方 hooks 文档确认支持 `type: "mcp_tool"`，也支持 PostToolUse 的 `additionalContext`。
需要注意：agent 写的是磁盘上的文件，Solo 的 buffer 要先等文件系统事件触发重载，服务器再重新检查，
所以诊断工具要先等这个过程稳定下来再返回，否则拿到的是旧结果。

### 工作量估计（推荐方案 A）

| 项目 | 估计 |
|---|---|
| 命名管道服务端 + hello/token 认证 + 把 `listener.rs` 改成接受通用流 | 1.5 天 |
| `solo-cli mcp` 转发进程（stdio 和管道互转，断线重连） | 0.5 天 |
| 5 个只读工具（diagnostics、definition、references、symbols 含文件和工作区、hover），从 `crates/agent/src/tools` 改写；后台打开 buffer；处理 G19 模式 | 2 天 |
| shell 包装加 `--mcp-config`（4 种 shell）+ token 存进终端宿主 | 1 天 |
| gpui 测试 + 文档（agent-cli.md、guide 中英文） | 1 天 |
| **合计** | **约 5–7 个工作日** |

## 5. 方案比较

| 方案 | 少搜索（定义/引用） | 诊断 | 复用 Solo 的服务器 | 适用的 agent | 协议稳定性 | Solo 工作量 |
|---|---|---|---|---|---|---|
| 0. 用户自己装官方 `clangd-lsp` 插件 | 有 | 有（编辑后自动） | 否，再起一份 | 只有 Claude | 官方，稳定 | 0 |
| 1. 实现 Claude IDE 协议 | 没有 | 有（`getDiagnostics`） | 是 | 只有 Claude | 大半未公开，风险高 | 1.5–2 周 |
| 2. Solo MCP server（方案 A） | 有 | 有（工具 + 可选 hook） | 是 | 所有支持 MCP 的 agent | MCP 是公开标准 | 5–7 天 |
| 3. LSP 门面（假装是语言服务器） | 有（原生 `LSP` 工具） | 有（原生） | 是 | 只有 Claude | Claude 的 LSP 客户端行为没有文档 | 1–2 周，较脆弱 |
| 4. 只加 `solo-cli agent lsp ...` 命令 | 有（通过 Bash 调用） | 有 | 是 | 所有 agent | 内部协议，稳定 | 2–3 天 |

方案 4 的成本最低，但 agent 要先被告知这些命令才会用；每次调用都走 Bash 工具，可能弹权限确认；
模型对 MCP 工具的使用意愿可能比对自定义命令高（**未验证**，可以放进实验）。

## 6. 建议和最小的第一步

1. **第 0 步，不写代码（约 0.5–1 天用户时间）：** 按第 7 节做实验，比较"无 LSP"和"官方 `clangd-lsp`"
   两组，先回答"有符号级工具，Grep/Read 能不能明显减少、能不能更快完成"。
2. **如果收益明显（比如 Grep+Read 减少 30% 以上，或耗时明显缩短）：** 按方案 A 做 Solo 的 MCP server，
   第一版只提供 5 个只读工具，用 `--mcp-config` 注入，不碰 `~/.claude`。之后再做第三组实验，
   和官方插件比较效果，同时比较 clangd 的总内存（一份还是两份）。
3. **如果收益不明显：** 停在这里。最多加一个"编辑后自动诊断"的 hook（这件事本身就有价值，能减少
   "编译一遍才发现错"的往返），不做导航工具。
4. 方案 1（IDE 协议）以后单独评估，只为"在 Solo 里审批 diff"的体验。

## 7. 实验设计（需要用户用真实 Claude Code 运行）

**目标：** 同一个任务，有、无代码导航工具，比较 Grep、Read 次数和完成时间。

### 准备

1. 仓库：Godot，固定一个 commit：`git worktree add ../godot-exp <commit>`。每次运行前执行
   `git -C ../godot-exp reset --hard <commit>` 和 `git clean -fdx`（注意不要删掉索引文件）。
2. 给 clangd 准备 `compile_commands.json`：`scons compiledb=yes`，或者用 Godot 文档里的生成方式。
   没有这个文件，clangd 找到的引用会很不全，实验就没有意义了。
3. 安装 clangd（`Get-Command clangd` 能找到），**先让 clangd 把后台索引建完**（在 Solo 里打开项目，
   G19 模式设为 Full，等索引完成），避免把冷启动时间算进去。
4. 固定模型和设置：每组都加 `--model <同一个模型>`，用同一个 Claude Code 版本（记下 `claude --version`）。

### 实验组

| 组 | 启动方式 |
|---|---|
| A 对照组 | `claude -p "<任务>" --output-format stream-json --verbose --strict-mcp-config`，并且不启用任何 LSP 插件（可以临时 `claude plugin disable clangd-lsp@claude-plugins-official`，或者在没装插件的配置下运行） |
| B 官方 LSP | 先 `/plugin install clangd-lsp@claude-plugins-official`；命令和 A 相同 |
| C Solo MCP（第 2 步做完后才有） | 在 Solo 终端里运行，命令和 A 相同，但用 `--mcp-config` 指向 Solo 的配置 |

可选的变体：在 B、C 组的提示末尾加一句"可以使用代码导航工具"，用来区分"有工具但没用"和"用了也没帮助"。

### 任务（每个任务都在 3 组里各跑 3 次，顺序随机）

1. 导航类："找出 `Node::_notification` 在 `scene/` 目录下的所有重写，列出各自处理了哪些通知。"
2. 影响分析："如果给 `Object::get_class()` 增加一个参数，需要修改哪些调用点？列出文件和行号，不要修改代码。"
3. 修改类："把 `<某个只有 10–30 处引用的函数>` 改名为 `<新名字>`，更新所有调用点，并确认能编译通过。"
   要选一个引用分布在多个文件里、并且有同名干扰（文本搜索会搜到不相关结果）的符号。
4. 修错类：事先在某个头文件里改错一个类型，让 3–5 个文件编译失败，然后提示："构建失败，请修好。"

### 采集

- 每次运行把 stream-json 输出保存为 `runs/<任务>-<组>-<序号>.jsonl`。
- 用下面的脚本统计工具调用（Python，用 `python -I count.py runs/*.jsonl` 运行）：

```python
import json, sys, collections
for path in sys.argv[1:]:
    tools = collections.Counter(); result = {}
    for line in open(path, encoding="utf-8"):
        try: ev = json.loads(line)
        except ValueError: continue
        if ev.get("type") == "assistant":
            for block in ev.get("message", {}).get("content", []):
                if block.get("type") == "tool_use":
                    tools[block["name"]] += 1
        if ev.get("type") == "result":
            result = ev
    print(path, dict(tools), "duration_ms=", result.get("duration_ms"),
          "turns=", result.get("num_turns"), "cost=", result.get("total_cost_usd"))
```

  stream-json 里 `result` 事件的字段名请以实际输出为准（**未验证**）。交互式会话的记录在
  `~/.claude/projects/<项目>/<会话>.jsonl`，可以用同一个脚本统计。
- 指标：Grep 次数、Read 次数、Glob 次数、`LSP` 或 `mcp__solo__*` 调用次数、总轮数、耗时、费用或 token；
  任务是否正确完成，由人按事先写好的标准判断，比如改名后能不能编译、影响分析的清单对照
  clangd 的 references 结果算召回率。
- B 组和 C 组另外记下 clangd 进程的峰值内存（任务管理器或 `Get-Process clangd`），用来体现"复用服务器"的价值。

### 判定

- 每个任务每组取 3 次的中位数。在**正确率不下降**的前提下，Grep+Read 合计减少 30% 以上，
  或耗时减少 20% 以上，就认为有收益，可以进入第 2 步。
- 如果 B 组明显有效、C 组和 B 组差不多，而 C 组少起一份 clangd，那么 Solo 的价值主要在资源和多 agent 支持。
- 如果 B 组的工具几乎没被调用，先试"提示里加一句"的变体，再决定要不要做。

## 8. 来源

官方文档（2026-11-12 读取）：
- Code intelligence 插件：https://code.claude.com/docs/en/plugins/code-intelligence
- 插件 manifest，`lspServers` 字段：https://code.claude.com/docs/en/plugins-reference
- 插件组件（LSP servers、MCP servers、hooks）：https://code.claude.com/docs/en/plugins/components
- Tools reference，"LSP tool behavior"：https://code.claude.com/docs/en/tools-reference
- VS Code 集成（IDE MCP server、锁文件、认证头、`getDiagnostics`）：https://code.claude.com/docs/en/ide-integrations
- JetBrains 集成：https://code.claude.com/docs/en/jetbrains
- MCP（环境变量展开、作用域、输出上限）：https://code.claude.com/docs/en/mcp
- CLI 参数（`--mcp-config`、`--strict-mcp-config`、`--plugin-dir`、`--settings`、`--ide`）：https://code.claude.com/docs/en/cli-reference
- Hooks（`mcp_tool` 类型、PostToolUse 的 `additionalContext`）：https://code.claude.com/docs/en/hooks

非官方资料（逆向结果，内容未验证）：
- coder/claudecode.nvim PROTOCOL.md：https://raw.githubusercontent.com/coder/claudecode.nvim/main/PROTOCOL.md

Solo 源码（分支 g-batch2，提交 c78f6d0ab9）：见第 4 节表格中的路径。
