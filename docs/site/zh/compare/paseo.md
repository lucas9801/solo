# Solo 和 Paseo

[← 比较总览](index.md)

[Paseo](https://github.com/getpaseo/paseo) 是一个后台守护进程加多个客户端：桌面、手机、网页和命令行都能连上同一组 agent。由一位开发者维护。

> 信息截至 2026 年 11 月初，以 Paseo 官方为准。

## Paseo 做得更好的

- **在哪都能接着管。** 完整的手机 App 和网页端，离开电脑也能看进度、回复 agent。
- **agent 不会因为关窗口而中断**，由守护进程托管。
- **很开放。** 提供 CLI、SDK、MCP 和 skills，方便接进你自己的工具链。
- **多平台**，Windows 有 x64 和 ARM64 版。
- 不收集遥测。

## Solo 不一样的地方

- **完整的编辑器和按轮审阅。** Paseo 有改动视图；Solo 按 agent 的每一轮记录，可以逐块保留或拒绝、回滚、看这一轮新增了几个错误，审阅时还能跳转到定义、查引用。
- **agent 用原生命令行跑。** 按 Paseo 的 issue #1029，它接 Claude 走的是 Agent SDK；Solo 直接在终端里跑你自己的 `claude` 等命令行工具，不经过 SDK。
- **文档审阅**：在预览里批注 agent 写的文档，一次发回给它。

## 怎么选

想随时在手机上管 agent，或者要把 agent 接进自己的自动化：Paseo。想在一台 Windows 电脑上把 agent 的改动读透、审细：可以试试 Solo。
