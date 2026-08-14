# 核心思想对比：deepseek-harness vs grok-build

本文对比 [deepseek-ai/deepseek-harness](https://github.com/deepseek-ai/deepseek-harness)（下称 **dsh**）与
[xai-org/grok-build](https://github.com/xai-org/grok-build)（下称 **grok**）的核心思想，作为本仓库 `rh` 的融合依据。

结论先行：**dsh 用一套动态插件宿主（Cordis）把「能力」抽象成可替换的 seam，并以 append-only 会话日志作为唯一真相；grok 用 Rust 的 trait 系统把「工具」统一成一条 `Tool` trait，并以 composition root + 单二进制交付。两者解决的是同一个问题的两端：一个偏「可组合的运行时」，一个偏「可静态类型检查的高性能单机内核」。**

---

## 1. 一句话概括

| | deepseek-harness (`dsh`) | grok-build (`grok`) |
|---|---|---|
| 定位 | 开源 agent harness，**everything is a plugin** | 终端 AI 编码 agent（TUI），Rust 单二进制 |
| 语言/运行时 | TypeScript / Node，ESM，pnpm monorepo（~50 包） | Rust，Cargo workspace（~90 crate），edition 2024 |
| 关键抽象 | Cordis 插件上下文：service / event / effect | 统一 `Tool` trait + `ToolBridge` + `AgentBuilder` |
| 交付形态 | npm 库 + `web` / `headless` profile | 单二进制（TUI / headless / ACP） |

---

## 2. deepseek-harness 的核心思想

### 2.1 一切皆插件，没有特权核心

dsh 建立在 Cordis 之上，设计哲学是 **「everything is a plugin」**：

- 模型适配器、工具注册表、会话日志、agent loop 本身，**全都是插件**。
- 插件向共享 `Context` 贡献三类东西：
  - **typed services**（可替换能力的接口）；
  - **typed events**（扩展点，用 TypeScript declaration merging 做类型化事件表）；
  - **reversible effects**（`ctx.effect()` / `ctx.on()`，注册都返回 disposer）。
- 「注册即副作用」：`register()` 返回 disposer，插件卸载时 unwind 它挂载的一切。

> 工程含义：**不存在需要打补丁的核心**。你通过「在旁边挂一个插件」来扩展，而不是改 loop。

### 2.2 capability seam：三角色可替换能力

dsh 把每个可替换能力建模为 **seam（缝隙）**，包含三个角色：

- **Service Definition**（接口声明）
- **Service Provider**（实现，注册到 context）
- **Consumer**（通常是面向模型的 tool）

关键洞察：**换一个 Provider 就换整个产品**。例如 filesystem / subprocess 共享同一执行世界，把它们指向远程沙箱时，Bash、PTY、LSP 一起被搬走，而无需 fork 任何 provider。

### 2.3 会话日志是唯一真相（model-visible ⟺ logged）

- 会话是一个 **append-only 的 `SessionEvent` 日志**。
- `deriveMessages()` 从日志投影出模型历史；**任何进入模型请求的东西都必须能从日志重建**，并由运行时不变量强制。
- fork / resume / transcript / telemetry / persistence 全部从同一条日志派生。

### 2.4 组合与 profile

- 运行中的 dsh 是启动时从有序 layer 组成的**插件树**：profile 列出 bundle，bundle 是 patch 层，`cordis.patch.yml` 可覆盖任意行。
- `--dump-config` 打印实际 boot 出的树。

---

## 3. grok-build 的核心思想

### 3.1 统一 `Tool` trait：单一工具边界

grok 用 Rust trait 把**所有工具来源统一成一条 `Tool`**（`xai-tool-runtime`）：

- 工具实现 `id` / `description`（schemars 生成 JSON Schema）/ `capabilities` / `should_list`，以及
  `execute`（流式）或 `run`（阻塞）二选一。
- 流式契约：`[Progress*, Terminal]`，恰好一个终止项。
- 类型擦除走 `ToolDyn` / `ArcTool`；MCP、computer-hub、codex/opencode 移植实现都收敛到同一 trait。

### 3.2 组合根 + Builder：静态组装

- `xai-grok-pager-bin` 是 **composition root**，在编译期把 crate 闭包组装成二进制。
- `AgentBuilder` 从 `AgentDefinition` + session context 构建出**不可变**的 `Agent`（定义 + 渲染后的 system prompt + `ToolBridge` + 策略）。
- `ToolBridge` 拥有 `ToolRegistry + ToolState + SessionContext`。

### 3.3 单二进制 + 重运行时

- 异步 tokio、gRPC/protobuf（tonic）、SQLite journal、session-events / session-search。
- ratatui TUI、pty、terminal；sandbox、checkpoints（fast-worktree、hunk-tracker）。
- plugins / skills / hooks / marketplace，Rhai 脚本扩展。
- 交付为**单二进制**：交互式 TUI、headless（脚本/CI）、ACP（编辑器嵌入）。

---

## 4. 共性（融合的锚点）

1. **可替换的工具/能力抽象**：dsh 的 seam（Definition/Provider/Consumer）↔ grok 的 `Tool` trait + 适配器。
2. **定义与上下文分离**：dsh 的 preset/cordis.yml ↔ grok 的 `AgentBuilder`（definition + session context）。
3. **会话事件/日志**：dsh 的 `SessionEvent` log ↔ grok 的 session-events + SQLite journal。
4. **subagent / 委托**、**ACP**、**compaction / hooks / skills / plan / todo** 两边都有。
5. **composition-first**：dsh 的插件树 ↔ grok 的 composition root。

---

## 5. 差异（融合要解的张力）

| 维度 | dsh | grok |
|---|---|---|
| 类型/扩展 | 动态插件树，运行时挂载 | 静态 trait + 代码生成 + Rhai |
| 插件模型 | service/event/effect，一切皆插件 | `Tool` trait + registry + composition root |
| 循环形式 | 显式 agent-loop 插件，turn/step，waterfall | Agent + sampler + shell，turn 模型较隐式 |
| 会话真相 | append-only log，model-visible⟺logged | session-events + SQLite journal + search |
| 类型安全 | TS strict + declaration merging | Rust 类型系统 + schemars |
| 沙箱 | Landlock / e2b | sandbox + fast-worktree + checkpoints |
| 交付 | npm 库 + web/headless | 单二进制 |

---

## 6. 融合的基本原则

- **用 Rust 的 trait + `Arc<T>` 表达 dsh 的「seam」**：`Service`（Definition）= trait，`Provider` = 注册到 `Context` 的实现，`Consumer` = 面向模型的 tool。
- **用 `Tool` trait 作为唯一的「模型可见」边界**（取自 grok），用**会话日志作为唯一真相**（取自 dsh）。
- **保留 dsh 的「无特权核心」**：把模型、工具注册表、会话存储、agent loop 都做成可注册/可替换的组件（在 Rust 里是「静态插件树 + 组合根」）。

具体到本仓库的落地方案，见 [architecture.md](architecture.md)；可选的多条融合道路，见 [fusion-paths.md](fusion-paths.md)。
