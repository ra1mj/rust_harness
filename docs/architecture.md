# rh 架构

`rh`（Rust Harness）是把 dsh 与 grok 的核心思想融合后的最小可运行实现：**一个 Rust 驱动的 harness agent**。它选择 [fusion-paths.md](fusion-paths.md) 中的「路径 A + B」正交合成。

---

## 1. 仓库布局

```
crates/
  rh-core/    插件宿主：Context、Plugin、Service、Event、Disposer
  rh-tool/    统一 Tool trait、ToolRegistry（bridge）、ToolCallContext
  rh-session/ append-only SessionEvent 日志、derive_messages 投影、SessionStore
  rh-agent/   ModelProvider seam、AgentDefinition/AgentBuilder、turn/step loop
  rh-tools/   能力 seam（Shell、FileSystem）+ 内置工具（bash/fs_read/fs_write/todo_write）
  rh-cli/     组合根 + CLI（run / tools / dump-config）+ 可选 HTTP model provider
```

依赖方向：`rh-cli → rh-tools/rh-agent → rh-session/rh-tool → rh-core`。`rh-core` 不依赖任何业务 crate，是「无特权核心」的宿主。

---

## 2. rh-core：插件宿主（dsh 的 Cordis 移植）

- `Context`：`Arc<ContextInner>` 的包装，`Clone` 即共享；持有三张表——类型化服务、事件订阅、服务名。
- `Plugin::mount(&Context) -> Disposers`：插件贡献并返回 disposer 列表。
- `Context::provide::<T>(Arc<T>)` / `service::<T>()`：类型化服务注册与解析；**同一类型后注册覆盖先注册**（换 Provider 即换行为）。
- `Context::on::<E>()` / `emit::<E>()`：类型化事件，订阅/发布。
- `Disposer`：「注册即副作用」，drop 即 unwind。

关键实现点：服务表用 `TypeId` 作 key，值用 `Arc<dyn Any + Send + Sync>` 存 `Arc<T>`（`Arc<T>` 本身是 sized，即使 `T` 是 `dyn Trait`），这样 `dyn ModelProvider` 这类 trait object 也能注册与解析。

---

## 3. rh-tool：统一工具边界（grok 的 `Tool` 移植）

- `Tool` trait：`id()` / `description()`（JSON Schema）/ `capabilities()` / `should_list()`，以及
  `execute()`（流式，默认包装 `run`）/ `run()`（阻塞）。
- `ToolStream` 契约：`[Progress*, Terminal]`，恰一个终止项。
- `ToolRegistry`（≈ grok 的 `ToolBridge`）：注册/查找/列清单/路由调用。
- `ToolCallContext`：携带 `Context`，工具在调用时解析能力服务（**Consumer 角色**）。

---

## 4. rh-session：会话日志即真相（dsh 的 model-visible ⟺ logged）

- `SessionEvent`：append-only 的 durable 事实（user/assistant/chunk/tool_call/tool_result/turn/step）。
- `Session::derive_messages()`：**从日志投影模型历史的唯一路径**；agent loop 只从该投影构造请求。
- `Session::append()`：落日志的同时广播到 `Context`（观察者读同一事实流）。
- `SessionStore` + `SessionPlugin`：会话存储作为服务挂载。

---

## 5. rh-agent：模型 seam + 循环（dsh 的 loop 形式 + grok 的 Builder）

- `ModelProvider` trait = **模型适配器的 seam**（Service Definition）；注册 `MockModelProvider`（无 key 可跑）或 HTTP provider。
- `AgentBuilder::build(session)`：从 `AgentDefinition` + 会话解析出**不可变** `Agent`（grok 的 Agent 形态）。
- turn/step loop：`turn_start → (step_start → 模型请求 → 工具调用 → tool_result → step_end)* → turn_end`。
- 模型请求**只**由 `session.derive_messages()` 构造，强制执行 model-visible ⟺ logged。

---

## 6. rh-tools：能力 seam + 内置工具

演示 dsh 的 seam 三角色：

| Seam | Definition | Provider | Consumer |
|---|---|---|---|
| Shell | `trait Shell` | `LocalShell`（`sh -c`） | `BashTool` |
| FileSystem | `trait FileSystem` | `LocalFileSystem` | `FsReadTool` / `FsWriteTool` |
| TodoList | `TodoList`（`Arc<RwLock<Vec>>`） | 内存实现 | `TodoWriteTool` |

换 Provider（如把 `Shell` 指向远程沙箱）即换工具行为，工具代码不动。

---

## 7. rh-cli：组合根 + Web

`assemble()` 按顺序挂载插件树：`session → shell:local → fs:local → tools`（模型不在插件树里，按请求注入）。

`rh web` 是一个中文浏览器界面（axum + WebSocket，最接近 dsh）：每个 WebSocket 连接拥有一个独立 `Session`，前端订阅 `Session::subscribe()` 的实时广播，把 agent loop 追加的每个 durable 事实**实时**渲染到 transcript（含模型逐字流式输出 `assistant/chunk`）。

**LLM 层（学自 dsh）**：adapter 绑定一个 **provider 路由**（endpoint + 凭证），**模型按请求选择**。

- `/api/providers`（GET/POST）、`/api/providers/{id}`（DELETE）、`/api/providers/{id}/discover`（`GET /models`）、`/api/active`（`provider + model` 双键选择）。
- `ModelHub`（`rh-providers`）注册多个 provider、发现其模型、持久化到 `--models-file`；每个 turn 用当前 `(provider, model)` 重建 adapter（`AgentBuilder::with_model`），切换对下一条消息立即生效。
- 无 mock 模式：`rh run` 走环境变量（`RH_API_KEY`/`RH_BASE_URL`/`RH_MODEL`）。

```text
plugins (mount order):   session / shell:local / fs:local / tools
services:                SessionStore / Shell(local) / FileSystem(local) / TodoList / ToolRegistry
tools:                   bash / fs_read / fs_write / todo_write
```

`rh run "…"` 的一次端到端运行：

```text
turn_start → user_message → step_start
  → assistant_chunk*（逐字流式） → assistant_message
  → tool_call → tool_result → step_end
  → … → turn_end
```

---

## 8. 与两源项目的对应关系

| rh 组件 | 取自 dsh | 取自 grok |
|---|---|---|
| `rh-core` Context/Plugin/Disposer | Cordis 插件宿主 | — |
| `rh-tool` Tool trait / ToolStream | 能力 seam 的 Consumer | 统一 `Tool` trait |
| `rh-session` SessionEvent / derive_messages | 会话日志真相 | session-events |
| `rh-agent` AgentBuilder / Agent / 流式 loop | agent-loop 插件形式 | AgentBuilder + Agent |
| `rh-providers` ModelHub / 模型发现 | LLM adapter seam（`ctx.llm`） | — |
| `rh-tools` web_search/web_fetch/grep/glob | web/搜索能力（dsh 插件生态） | grok 同名工具 |
| `rh-tools` subagent（task/task_output/task_wait/task_kill） | subagent 能力 | **Codex 任务模型**（grok 从 openai/codex 移植） |
| `rh-web` axum + WebSocket | Web UI（`dsh web`） | — |
| `rh-cli` 组合根 | profile/bundle 组合 | composition root 单二进制 |

> grok-build 的贡献集中在**运行时骨架**：统一 `Tool` trait + `ToolRegistry`、`AgentBuilder`、composition root、以及 vendor 的 Codex 子代理任务模型——它提供的是"工具/代理如何装配与运行"，而不是 UI。
