//! `rh` — a Rust-driven harness agent fusing DeepSeek Harness and Grok Build.
//!
//! The CLI is the composition root: it mounts a plugin tree on a shared
//! [`Context`](rh_core::Context), then either runs one task headlessly,
//! lists the assembled tools, dumps the composition, or serves the web UI.

use std::sync::Arc;

use clap::{Parser, Subcommand};

use rh_agent::{AgentBuilder, AgentDefinition, ModelProvider};
use rh_core::{Context, Disposers, Plugin};
use rh_session::{SessionPlugin, SessionStore};
use rh_tool::{ToolCallContext, ToolRegistry};
use rh_tools::{FileSystemPlugin, ShellPlugin, ToolsPlugin};

#[derive(Parser)]
#[command(name = "rh", version, about = "A Rust-driven harness agent fusing deepseek-harness and grok-build")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run one task headlessly (requires RH_API_KEY) and print the transcript.
    Run {
        /// The task to run.
        task: String,
    },
    /// List the tools the harness assembles.
    Tools,
    /// Print the assembled plugin tree, services, and tools.
    DumpConfig,
    /// Launch the web UI (single-page app + WebSocket live transcript + workspace management).
    Web {
        /// Address to bind.
        #[arg(long, default_value = "127.0.0.1:3080")]
        addr: String,
        /// File the model hub is persisted to.
        #[arg(long, default_value = "rh-models.json")]
        models_file: String,
        /// Directory sessions are persisted to.
        #[arg(long, default_value = ".rh")]
        data_dir: String,
        /// File MCP server configs are persisted to.
        #[arg(long, default_value = "rh-mcp.json")]
        mcp_file: String,
        /// Directory user skills are loaded from.
        #[arg(long, default_value = "skills")]
        skills_dir: String,
        /// Directory native (dylib) plugins are loaded from.
        #[arg(long, default_value = "plugins")]
        plugins_dir: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run { task } => run(&task).await,
        Command::Tools => tools().await,
        Command::DumpConfig => dump_config().await,
        Command::Web {
            addr,
            models_file,
            data_dir,
            mcp_file,
            skills_dir,
            plugins_dir,
        } => {
            let assembled = assemble()?;
            let plugins = assembled.plugins.iter().map(|s| s.to_string()).collect();
            rh_web::serve(
                assembled.ctx.clone(),
                plugins,
                rh_web::WebOptions {
                    addr: addr.parse()?,
                    models_file: models_file.into(),
                    data_dir: data_dir.into(),
                    mcp_file: mcp_file.into(),
                    skills_dir: skills_dir.into(),
                    plugins_dir: plugins_dir.into(),
                },
            )
            .await
        }
    }
}

/// The assembled runtime: a context plus the disposers that keep its
/// registrations alive for the process lifetime.
struct Assembled {
    pub(crate) ctx: Context,
    plugins: Vec<&'static str>,
    _disposers: Vec<Disposers>,
}

/// Mount the plugin tree in order. Every part of the harness — session store,
/// shell, filesystem, tool registry — is a plugin; the model is injected per
/// request (there is no privileged model path to patch).
fn assemble() -> anyhow::Result<Assembled> {
    let ctx = Context::new();
    let plugins: Vec<Arc<dyn Plugin>> = vec![
        Arc::new(SessionPlugin),
        Arc::new(ShellPlugin),
        Arc::new(FileSystemPlugin),
        Arc::new(ToolsPlugin),
    ];

    let mut names = Vec::new();
    let mut disposers = Vec::new();
    for plugin in plugins {
        names.push(plugin.name());
        disposers.push(plugin.mount(&ctx)?);
    }

    Ok(Assembled {
        ctx,
        plugins: names,
        _disposers: disposers,
    })
}

async fn run(task: &str) -> anyhow::Result<()> {
    let assembled = assemble()?;
    let ctx = assembled.ctx.clone();

    let store = ctx
        .service::<SessionStore>()
        .ok_or_else(|| anyhow::anyhow!("no session store registered"))?;
    let session = store.create_fresh();

    let provider = rh_providers::OpenAiCompatibleProvider::from_env()?;
    let model = std::env::var("RH_MODEL").unwrap_or_else(|_| "deepseek-chat".to_string());

    let definition = AgentDefinition {
        name: "rh-agent".to_string(),
        model,
        system_prompt: "You are a Rust harness agent. Use tools when useful.".to_string(),
        tool_ids: Vec::new(),
        max_steps: 8,
    };

    // Register the provider on the context so the agent and its subagents
    // resolve the same model.
    let _registration = ctx.provide_named("ModelProvider", Arc::new(provider) as Arc<dyn ModelProvider>);
    let agent = AgentBuilder::new(ctx, definition).build(session)?;
    let report = agent.run(task).await?;

    println!("== session transcript ({}) ==", agent.session().id());
    for event in &report.events {
        println!("{}", serde_json::to_string(event)?);
    }
    Ok(())
}

async fn tools() -> anyhow::Result<()> {
    let assembled = assemble()?;
    let ctx = assembled.ctx;
    let registry = ctx
        .service::<ToolRegistry>()
        .ok_or_else(|| anyhow::anyhow!("no tool registry registered"))?;
    let listing_ctx = ToolCallContext::new(ctx, "tools", "tools");
    for tool in registry.list(&listing_ctx) {
        println!("- {:<12} {}", tool.name, tool.description);
    }
    Ok(())
}

async fn dump_config() -> anyhow::Result<()> {
    let assembled = assemble()?;
    let ctx = assembled.ctx;

    println!("plugins (mount order):");
    for name in &assembled.plugins {
        println!("  - {name}");
    }

    println!("\nservices:");
    for name in ctx.service_names() {
        println!("  - {name}");
    }

    let registry = ctx
        .service::<ToolRegistry>()
        .ok_or_else(|| anyhow::anyhow!("no tool registry registered"))?;
    let listing_ctx = ToolCallContext::new(ctx, "dump", "dump");
    println!("\ntools:");
    for tool in registry.list(&listing_ctx) {
        println!("  - {}", tool.name);
    }

    Ok(())
}
