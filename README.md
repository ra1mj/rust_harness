# rh — 一个 Rust 驱动的 harness agent

`rh`（Rust Harness）融合 [deepseek-ai/deepseek-harness](https://github.com/deepseek-ai/deepseek-harness) 与
[xai-org/grok-build](https://github.com/xai-org/grok-build) 的核心思想，是一个**可编译、可测试、可运行**的 Rust workspace。

- 取自 **dsh**：*everything is a plugin*（无特权核心）、capability seam（Definition/Provider/Consumer）、append-only 会话日志即真相（model-visible ⟺ logged）。
- 取自 **grok**：统一 `Tool` trait + 流式契约、`AgentBuilder` 组装不可变 `Agent`、tokio 运行时、单二进制 CLI。

## 文档

- [核心思想对比](docs/comparison.md) — 两个项目的逐项对比
- [多种融合道路](docs/fusion-paths.md) — 五条可行路线及取舍
- [架构](docs/architecture.md) — 本仓库的实现结构

## 快速开始

```sh
# 构建（默认 features，无网络模型依赖）
cargo build

# 无 key 运行一次任务（mock 模型，会真实执行 bash 工具）
cargo run -- run "please use bash to say hello"

# Web 界面（单页应用 + WebSocket 实时 transcript），浏览器打开 http://127.0.0.1:3080
cargo run -- web
# 换端口：cargo run -- web --addr 127.0.0.1:8080

# 列出工具 / 打印组装出的插件树
cargo run -- tools
cargo run -- dump-config

# 测试
cargo test
```

### 接入真实模型（DeepSeek / OpenAI 兼容）

```sh
# 需要 --features http（引入 reqwest）
export RH_API_KEY=...            # 必填
export RH_BASE_URL=https://api.deepseek.com   # 可选，默认 DeepSeek
export RH_MODEL=deepseek-chat    # 可选

# headless 或 web 都可用真实模型
cargo run --features http -- run "..." --http
cargo run --features http -- web --http
```

## 仓库结构

```
crates/
  rh-core/    插件宿主：Context、Plugin、Service、Event、Disposer
  rh-tool/    统一 Tool trait、ToolRegistry、ToolCallContext
  rh-session/ append-only SessionEvent 日志 + derive_messages 投影 + 每会话广播
  rh-agent/   流式 ModelProvider seam、AgentBuilder、turn/step loop
  rh-tools/   能力 seam（Shell/FileSystem）+ bash/fs_read/fs_write/todo_write
  rh-web/     axum Web 服务 + WebSocket 实时 transcript + 单页前端
  rh-cli/     组合根 + CLI（run/tools/dump-config/web）+ 可选 HTTP provider
```

## 设计要点

1. **无特权核心**：模型、工具注册表、会话存储、loop 都是插件，挂到 `Context` 上即可替换。
2. **能力 seam**：`Service`（trait）= Definition，注册实现 = Provider，面向模型的 tool = Consumer；换 Provider 即换行为。
3. **会话日志即真相**：模型请求只由 `session.derive_messages()` 构造，任何模型可见内容都可从日志重建。
4. **统一工具边界**：所有工具实现一条 `Tool` trait，流式契约 `[Progress*, Terminal]`。
5. **流式模型输出**：模型 seam 是流式的（`ModelEvent`），loop 边收边记 `assistant/chunk`，Web 前端实时打字。

## Roadmap（对应 fusion-paths）

- 路径 C：把 subagent / skill 收进 `Tool` trait。
- 路径 D：WASM / dylib 动态插件运行时。
- 路径 E：Rhai / JS 脚本层表达组合。

## License

MIT。参考实现 `.ref/` 为上游仓库的浅克隆（仅供分析，不随本仓库分发）；`dsh` 为 MIT，`grok` 为 Apache-2.0。
