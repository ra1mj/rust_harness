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
# 构建
cargo build

# Web 界面（中文单页应用 + WebSocket 实时 transcript + 会话/任务/模型管理），浏览器打开 http://127.0.0.1:3080
cargo run -- web
# 换端口：cargo run -- web --addr 127.0.0.1:8080
# 持久化：模型 hub（默认 ./rh-models.json）、会话（默认 ./.rh）、MCP 配置（默认 ./rh-mcp.json）
cargo run -- web --models-file ~/.rh/models.json --data-dir ~/.rh/sessions --mcp-file ~/.rh/mcp.json

# headless 跑一条任务（需要 RH_API_KEY）
cargo run -- run "please use bash to say hello"

# 列出工具 / 打印组装出的插件树
cargo run -- tools
cargo run -- dump-config

# 测试
cargo test
```

### 多模型支持（dsh 式：Provider 路由 + 模型发现）

借鉴 DeepSeek Harness 的 LLM 层：**adapter 绑定一个 provider 路由（endpoint + key），模型按请求选择**。

1. **Web 端**（推荐）：右上角「设置」→「添加 Provider」（名称 / Base URL / API Key）→ 保存后点「发现模型」（调用 `GET /models`）→ 在模型 chip 里点选即可切换。多个 Provider 可并存，配置持久化到 `--models-file`。
2. **环境变量**：启动前设置，Web 会自动 seed 一个 `DeepSeek` Provider。

```sh
export RH_API_KEY=...                       # 必填
export RH_BASE_URL=https://api.deepseek.com  # 可选，默认 DeepSeek
export RH_MODEL=deepseek-chat                # 可选，headless 的默认模型

cargo run -- run "..."     # headless 用环境变量
cargo run -- web           # Web 里还可添加更多 Provider / 发现模型
```

### 工作区管理（会话 / 任务 / 导出）

Web 左侧栏支持完整的工作区管理：

- **会话**：`＋ 新会话` 创建；点击切换；双击重命名；悬停 `×` 删除。会话持久化到 `--data-dir`（每个会话一个 JSON 文件），重启后仍在。
- **任务（todo）**：每会话一个 todo 清单，输入框添加、勾选完成；agent 的 `todo_write` 工具也写入当前会话任务。
- **子代理任务（Codex 式）**：agent 可用 `task` 工具 spawn 子代理（独立 session + 模型 + 工具状态），`run_in_background` 后台、`task_output`/`task_wait` 取结果、`task_kill` 取消 —— 这是 grok-build 从 openai/codex 移植的任务模型。
- **工作区（Codex 式）**：每个会话有自己的工作区文件夹（默认 `~/.rh/workspaces/<session-id>`，隔离于 harness 自身目录），可在侧栏「工作区 → 更改」指向任意文件夹；`bash`/`fs_read`/`fs_write`/`grep`/`glob` 都作用于该工作区，工作区内容 + git 状态注入 system prompt 作为上下文。
- **导出**：`导出 Markdown` / `导出 JSON` 下载当前会话 transcript（含任务）。

### 内置工具

`bash`、`fs_read`、`fs_write`、`todo_write`、`web_fetch`、`web_search`、`grep`、`glob`、`task`、`task_output`、`task_wait`、`task_kill`。

### MCP 支持（含市场）

在「设置 → MCP 服务器」：

- **市场**：内置 12 个精选服务器（filesystem / fetch / memory / thinking / git / sqlite / time / github / everything / puppeteer / postgres / brave-search），搜索框过滤，点「添加」一键安装（带 `<占位符>` 的会先填进命令框让你改路径）。
- **手动**：粘贴一行启动命令（如 `npx -y @modelcontextprotocol/server-filesystem /你的/目录`），自动拆命令 + 推导名称。
- 安装后其工具自动桥接进 harness 的 `Tool` 注册表，模型直接调用。配置持久化到 `--mcp-file`（默认 `rh-mcp.json`）。

```sh
cargo run -- web --mcp-file ~/.rh/mcp.json
```

## 仓库结构

```
crates/
  rh-core/    插件宿主：Context、Plugin、Service、Event、Disposer
  rh-tool/    统一 Tool trait、ToolRegistry、ToolCallContext
  rh-session/ append-only SessionEvent 日志 + 任务 + 持久化 SessionStore + 导出(Markdown/JSON)
  rh-agent/   流式 ModelProvider seam、AgentBuilder、turn/step loop
  rh-tools/   能力 seam（Shell/FileSystem）+ bash/fs_read/fs_write/todo_write/web_fetch/web_search/grep/glob + 子代理任务（task/task_output/task_wait/task_kill）
  rh-web/     axum Web 服务（中文 UI）+ WebSocket + 会话/任务/导出/模型/MCP REST API
  rh-providers/ LLM 层：OpenAI 兼容 adapter + ModelHub（Provider 注册 + GET /models 发现 + 双键选择）
  rh-mcp/     MCP client（stdio JSON-RPC）+ 工具桥接
  rh-cli/     组合根 + CLI（run/tools/dump-config/web）
```

## 设计要点

1. **无特权核心**：模型、工具注册表、会话存储、loop 都是插件，挂到 `Context` 上即可替换。
2. **能力 seam**：`Service`（trait）= Definition，注册实现 = Provider，面向模型的 tool = Consumer；换 Provider 即换行为。
3. **会话日志即真相**：模型请求只由 `session.derive_messages()` 构造，任何模型可见内容都可从日志重建。
4. **统一工具边界**：所有工具实现一条 `Tool` trait，流式契约 `[Progress*, Terminal]`。
5. **流式模型输出**：模型 seam 是流式的（`ModelEvent`），loop 边收边记 `assistant/chunk`，Web 前端实时打字。
6. **多 Provider / 多模型**：`ModelHub` 注册多个 provider 路由，`GET /models` 发现其模型，`provider + model` 双键选择（dsh 的 `GenerateOptions.provider` + `.model` 语义）。
7. **子代理任务**：`task` 工具 spawn 后台/前台子代理（独立 session），`task_output`/`task_wait`/`task_kill` 管理生命周期（Codex/grok 任务模型）。

### grok-build 在本项目中的作用

grok-build 的贡献是**工具与代理的运行时骨架**，不是 UI：

- **统一 `Tool` trait**（`id`/`description`(JSON Schema)/`capabilities`/`should_list`/`execute`/`run`）+ `[Progress*, Terminal]` 流契约 → `rh-tool`。
- **`ToolRegistry`（≈ ToolBridge）**：持有工具集并路由调用。
- **`AgentBuilder` 组装不可变 `Agent`**（definition + session context）→ `rh-agent`。
- **composition root 单二进制** → `rh-cli`。
- **Codex 子代理任务模型**（grok 从 openai/codex 移植的 `task`/`task_output`/`task_wait`/`task_kill`）→ `rh-tools` 的 `subagent`。
- **工具清单**（bash/web_search/web_fetch/grep/glob/todo）→ 对应 `rh-tools` 内置工具。

## Roadmap（对应 fusion-paths）

- 路径 C：把 subagent / skill 收进 `Tool` trait。
- 路径 D：WASM / dylib 动态插件运行时。
- 路径 E：Rhai / JS 脚本层表达组合。

## License

MIT。参考实现 `.ref/` 为上游仓库的浅克隆（仅供分析，不随本仓库分发）；`dsh` 为 MIT，`grok` 为 Apache-2.0。
