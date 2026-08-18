# rh · Rust 驱动的 AI Agent Harness

> 融合 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) 的「一切皆插件」与 [Grok Build](https://github.com/xai-org/grok-build) 的「统一 Tool 运行时」，用 Rust 构建的一个**可编译、可测试、可运行**的 agent harness。

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange.svg)](rust-toolchain.toml)
![Crates](https://img.shields.io/badge/crates-9-informational)

`rh` 提供浏览器端工作台 + 多模型 + 工具调用 + 子代理任务 + MCP，覆盖从「会话管理」到「能力编排」的完整 agent 运行时。

---

## ✨ 特性

**架构**

- 🧩 **一切皆插件**：模型、工具注册表、会话存储、agent loop 都是可挂载/可替换的插件（无特权核心）。
- 🔌 **能力 seam**：每个能力由 `Service`(Definition) / `Provider` / `Consumer` 三角色构成，换 Provider 即换行为。
- 📜 **会话日志即真相**：append-only `SessionEvent` 日志是唯一真相，模型可见内容必可从日志重建。

**模型**

- 🎯 **多 Provider / 多模型**：注册多个 provider 路由（endpoint + key），`GET /models` 发现模型，`provider + model` 双键选择。
- ⚡ **流式输出**：模型 seam 是流式的，边收边记 `assistant/chunk`，前端实时打字。

**能力**

- 🔧 **统一 Tool trait**：所有工具实现一条 `Tool`（`[Progress*, Terminal]` 流契约）。
- 🌐 **Web 能力**：`web_fetch` / `web_search`。
- 🔍 **代码搜索**：`grep` / `glob`。
- 🐚 **Shell / 文件**：`bash` / `fs_read` / `fs_write`（跨平台 shell，作用域限定在工作区）。
- 🤖 **子代理任务**（Codex 式）：`task` / `task_output` / `task_wait` / `task_kill`，后台子代理并行执行。
- 🔗 **MCP**：stdio MCP client + 内置市场，第三方工具一键桥接。
- 🧭 **工作模式**：直接对话 / 计划模式 / Trellis 工作流 三选一，带可见的阶段 stepper。
- 📝 **计划模式**：先规划后执行，规划阶段禁止写操作（bash/fs_write 门禁）。
- 🧠 **Skills 系统**：内置 + 目录技能，`skill`/`skill_list` 工具按需加载。

**工作区**

- 💬 **会话管理**：创建 / 切换 / 重命名 / 删除，持久化。
- ✅ **任务（todo）**：每会话任务清单，带状态（待办/进行中/完成/取消）+ 进度条的可视化面板。
- 📁 **工作区隔离**：每会话独立工作目录，`bash`/`fs_*`/`grep`/`glob` 均作用于此，隔离于 harness 自身。
- 📤 **导出**：Markdown / JSON。

---

## 🚀 快速开始

### 前置要求

- [Rust](https://rustup.rs) 1.75+（`rust-toolchain.toml` 已 pin）

### 构建与运行

```sh
git clone git@github.com:ra1mj/rust_harness.git
cd rust_harness
cargo build
```

```sh
# 浏览器工作台（中文 UI + WebSocket 实时 transcript），打开 http://127.0.0.1:3080
cargo run -- web

# headless 单任务（需 RH_API_KEY）
export RH_API_KEY=sk-...
cargo run -- run "帮我写一个 hello world"

# 查看工具 / 打印插件树
cargo run -- tools
cargo run -- dump-config

# 测试
cargo test
```

### 接入真实模型

两种方式：

1. **Web 端**：右上「设置」→「添加 Provider」（名称 / Base URL / API Key）→「发现模型」→ 点选。
2. **环境变量**：

```sh
export RH_API_KEY=sk-...                       # 必填
export RH_BASE_URL=https://api.deepseek.com     # 可选，默认 DeepSeek
export RH_MODEL=deepseek-chat                   # 可选
```

---

## 📖 使用指南

### 模型

「设置 → 模型设置」：添加多个 Provider（DeepSeek / OpenAI / 本地 Ollama 等），每个 Provider 下可「发现模型」并点选当前使用；输入框下方有模型下拉可随时切换。

### 工作区

每个会话有独立工作区（默认 `~/.rh/workspaces/<session-id>`）。侧栏「工作区 → 更改」可指向任意文件夹，其内容 + git 状态会注入 system prompt。

### MCP

「设置 → MCP 服务器」：市场内置 12 个精选服务器（filesystem / fetch / memory / git / sqlite / github …），搜索 + 一键安装；也可粘贴任意启动命令。MCP 工具自动桥接进 `Tool` 注册表。

### 子代理任务

模型可调用 `task` 工具 spawn 子代理（独立 session），`run_in_background` 后台并行，`task_output`/`task_wait` 取结果，`task_kill` 取消。

### 工作模式

输入框旁的工作模式下拉三选一：

- **直接对话**：普通 agent。
- **计划模式**：先规划后执行，规划阶段禁止写操作（`bash`/`fs_write` 被门禁），产出计划后等用户切回执行。
- **Trellis 工作流**：结构化流程（头脑风暴 → 调研 → 计划 → 实现 → 审查 → 完成），每进入一个阶段调用 `workflow_step` 工具，顶部 stepper 实时高亮。

### Skills 系统

内置 `code-review` / `write-tests` / `debugging` / `commit-message` 技能；`skill_list` 列出、`skill <name>` 加载。用户技能放在 `--skills-dir`（默认 `skills/`，每个 `.md` 一个技能），自动合并进侧栏「技能」列表。

---

## 🧰 内置工具

| 工具 | 说明 |
|---|---|
| `bash` | 执行 shell 命令（作用域 = 工作区） |
| `fs_read` / `fs_write` | 读 / 写文件 |
| `todo_write` | 写入当前会话任务清单 |
| `web_fetch` | 抓取 URL 为文本 |
| `web_search` | DuckDuckGo 网页搜索 |
| `grep` | 正则搜索文件内容 |
| `glob` | 按 glob 模式列出文件 |
| `task` | spawn 子代理任务 |
| `task_output` / `task_wait` | 读结果 / 等待结果 |
| `task_kill` | 取消子代理 |
| （MCP） | 任意 MCP 服务器暴露的工具 |

---

## ⚙️ 配置

### 环境变量

| 变量 | 说明 | 默认 |
|---|---|---|
| `RH_API_KEY` | API Key | 无 |
| `RH_BASE_URL` | OpenAI 兼容端点 | `https://api.deepseek.com` |
| `RH_MODEL` | headless 默认模型 | `deepseek-chat` |

### `rh web` 参数

| 参数 | 说明 | 默认 |
|---|---|---|
| `--addr` | 监听地址 | `127.0.0.1:3080` |
| `--models-file` | 模型 hub 持久化文件 | `rh-models.json` |
| `--data-dir` | 会话持久化目录 | `.rh` |
| `--mcp-file` | MCP 配置持久化文件 | `rh-mcp.json` |

---

## 🏗️ 项目结构

```
crates/
  rh-core/      插件宿主：Context、Plugin、Service、Event、Disposer
  rh-tool/      统一 Tool trait、ToolRegistry、ToolCallContext
  rh-session/   会话日志 + 任务 + 工作区 + 持久化 + 导出
  rh-agent/     流式 ModelProvider seam、AgentBuilder、turn/step loop
  rh-tools/     能力 seam + 内置工具 + 子代理任务
  rh-web/       axum Web 服务 + WebSocket + REST API
  rh-providers/ OpenAI 兼容 adapter + ModelHub（模型发现/选择）
  rh-mcp/       MCP client（stdio JSON-RPC）+ 工具桥接
  rh-cli/       组合根 + CLI（run / tools / dump-config / web）
```

依赖方向：`rh-cli → rh-web/rh-tools/rh-agent → rh-session/rh-tool/rh-core`。

---

## 🧭 设计要点与融合来源

| rh 组件 | 取自 DeepSeek Harness | 取自 Grok Build |
|---|---|---|
| `rh-core` | Cordis 插件宿主（service/event/effect） | — |
| `rh-tool` | 能力 seam 的 Consumer | 统一 `Tool` trait + 流契约 |
| `rh-session` | 会话日志真相（model-visible ⟺ logged） | session-events |
| `rh-agent` | agent-loop 插件形式 | AgentBuilder + 不可变 Agent |
| `rh-providers` | LLM adapter seam（`ctx.llm`） | — |
| `rh-tools` subagent | subagent 能力 | **Codex 任务模型**（grok 移植） |
| `rh-web` | Web UI（`dsh web`） | — |
| `rh-cli` | profile/bundle 组合 | composition root 单二进制 |

详细对比见 [docs/comparison.md](docs/comparison.md)、融合道路见 [docs/fusion-paths.md](docs/fusion-paths.md)、架构见 [docs/architecture.md](docs/architecture.md)。

---

## 🗺️ Roadmap

- [ ] MCP SSE / HTTP 传输
- [ ] 动态插件运行时（WASM / dylib）
- [ ] skills 系统
- [ ] agent plan / todo 可视化面板
- [ ] 会话搜索 / 多会话并行

---

## 🤝 贡献

欢迎提 Issue / PR。参考实现位于 `.ref/`（上游仓库浅克隆，仅作分析，不随本仓库分发）。

---

## 📄 License

[MIT](LICENSE) © 2026

参考的上游项目：DeepSeek Harness（MIT）、Grok Build（Apache-2.0），各自遵循其原始许可证。
