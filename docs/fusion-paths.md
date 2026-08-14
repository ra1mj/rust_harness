# 多种融合道路

下面给出把 dsh 与 grok 融合成「Rust 驱动的 harness agent」的五条可行道路。它们从「改动最小 / 最接近某一方」到「彻底重写 / 最激进」排列，各有取舍。

> 本仓库实现的是 **路径 A + 路径 B 的正交合成**（见文末「本仓库的选择」）。

---

## 路径 A：「Rust 的 Cordis」—— 把 dsh 的插件宿主移植到 Rust

**思路**：照搬 dsh 的「everything is a plugin」世界观，用 Rust 类型系统重写 Cordis 的三件套。

- `Context`：共享组合上下文，克隆即共享（`Arc<ContextInner>`）。
- `Service`（= Definition）+ `Provider`（注册 `Arc<T>`）+ `Consumer`（从 context 解析）。
- 类型化事件：`Event`（任意 `'static` 类型）+ `ctx.on::<E>()` / `ctx.emit::<E>()`。
- `Disposer`：「注册即副作用」，注册返回 disposer，卸载即 drop。

**优点**：完整保留 dsh 的扩展性与「无特权核心」；所有能力（模型、工具、会话、loop）都可替换。
**代价**：Rust 没有 JS 的运行时挂载，`cordis.yml` 那种「字符串即配置」需要额外 DSL 或 serde 反序列化。
**适合**：想要一个**可组合、可测试、能力齐全**的 harness 内核，而非单一产品。

## 路径 B：「grok 内核 + dsh 的 seam」—— 在 grok 上补 dsh 的替换性与日志真相

**思路**：保留 grok 的 `Tool` trait / `ToolBridge` / `AgentBuilder` 与 tokio 运行时，把 dsh 的「capability seam」与「会话日志真相」嫁接上去。

- 让 `ToolCallContext` 携带 `Context`，工具从 context 解析 `dyn Shell` / `dyn FileSystem` 等服务（Consumer 角色）。
- 用 append-only `SessionEvent` 日志 + `derive_messages()` 取代/约束「直接拼消息」，强制执行 model-visible ⟺ logged。
- 换 Provider（如本地 shell → 远程沙箱）即换工具行为。

**优点**：几乎不动 grok 的「工具 + 单二进制」骨架，立刻获得 dsh 的可替换性与可重建性。
**代价**：仍然偏向「编译期组装」，运行时挂载能力弱。
**适合**：已有 grok 风格代码库，想「软着陆」引入 dsh 思想。

## 路径 C：「一切皆 Tool」—— 把模型/子代理/技能都收进 Tool trait

**思路**：把 `Tool` trait 抬升为**唯一扩展点**，连模型调用、subagent、skill、MCP 都实现成 `Tool`。

- 模型 = 一个 `Tool`（`id = "model"`）；subagent = 一个 `Tool`（`id = "task"`）；skill = 一个 `Tool`。
- 所有能力共享 `[Progress*, Terminal]` 流式契约与 `description()` JSON Schema。
- dsh 的类型化事件只用于**观测/遥测**，不参与能力组装。

**优点**：边界最少、心智模型单一；非常适合把「异构工具来源」统一。
**代价**：模型/子代理的语义被压平成「工具」，损失了 turn/step 的显式结构与 waterfall 生命周期。
**适合**：工具集庞大、来源混杂的场景。

## 路径 D：「动态插件运行时」—— WASM / dylib 运行时挂载

**思路**：为逼近 dsh 的「运行时挂载」，给 Rust harness 加一个动态加载层。

- 用 `libloading` 加载 dylib，或用 **WASM（wasmtime）** 加载沙箱化插件，插件实现 `Plugin`/`Tool` trait 的 C ABI / wasm 接口。
- 保留 `Context` 的 service/event/effect 语义，但允许 `cordis.yml`-like 的清单在运行时装配插件树。
- 沙箱（Landlock / seccomp / wasm）成为 Provider 的一等角色。

**优点**：最接近 dsh 的运行时可组合性；插件可独立分发。
**代价**：ABI 稳定、版本化、安全边界都是重工程；调试与类型检查变难。
**适合**：要做一个「插件市场」式、用户可扩展的 harness。

## 路径 E：「混合语言 harness」—— Rust 内核 + 脚本/FFI 表达层

**思路**：Rust 负责运行时与工具，嵌入脚本层表达 dsh 的组合语义。

- 内核 crate 提供 `Tool`/`Context`/`AgentLoop` 的 Rust 实现。
- 嵌入 **Rhai**（grok 已用）或 JS 引擎（如 Boa/QuickJS）解释 `cordis.yml`/脚本，做运行时组合与热更新。
- 或提供 C ABI，让 TypeScript（复用 dsh 生态）驱动 Rust 内核。

**优点**：兼顾 Rust 性能与脚本的可组合/可热更；能复用 dsh 已有配置生态。
**代价**：双运行时、双类型系统的边界治理成本高。
**适合**：需要「内核高性能 + 上层灵活脚本」的产品。

---

## 本仓库的选择：A + B 的正交合成

`rh` 选择**路径 A 与路径 B 的叠加**，因为它们正交且互补：

- **路径 A 的 `rh-core`**：`Context` + `Plugin` + 类型化 service/event/effect，提供「无特权核心」与可替换性。
- **路径 B 的 `rh-tool` + `rh-session` + `rh-agent`**：统一 `Tool` trait + append-only 会话日志 + `AgentBuilder`/turn-step loop，提供「单一工具边界」与「模型可见即已记录」。

具体结构见 [architecture.md](architecture.md)。路径 C/D/E 作为演进方向，在 `README` 的 Roadmap 中标记，不阻塞当前里程碑。
