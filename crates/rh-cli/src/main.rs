//! `rh` — a Rust-driven harness agent fusing DeepSeek Harness and Grok Build.
//!
//! The CLI is the composition root: it mounts a plugin tree on a shared
//! [`Context`](rh_core::Context), then either runs one task headlessly,
//! lists the assembled tools, dumps the composition, or serves the web UI.

use std::sync::Arc;

use clap::{Parser, Subcommand};

use rh_agent::{AgentBuilder, AgentDefinition, MockModelPlugin, ModelProvider};
use rh_core::{Context, Disposer, Disposers, Plugin};
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
    /// Run one task headlessly and print the session transcript.
    Run {
        /// The task to run.
        task: String,
        /// Use the real HTTP model provider from env (RH_API_KEY / RH_BASE_URL / RH_MODEL).
        #[arg(long)]
        http: bool,
    },
    /// List the tools the harness assembles.
    Tools,
    /// Print the assembled plugin tree, services, and tools.
    DumpConfig,
    /// Launch the web UI (single-page app + WebSocket live transcript + model settings).
    Web {
        /// Address to bind.
        #[arg(long, default_value = "127.0.0.1:3080")]
        addr: String,
        /// File the model catalog is persisted to.
        #[arg(long, default_value = "rh-models.json")]
        models_file: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run { task, http } => run(&task, http).await,
        Command::Tools => tools().await,
        Command::DumpConfig => dump_config().await,
        Command::Web { addr, models_file } => {
            let assembled = assemble(false)?;
            let plugins = assembled.plugins.iter().map(|s| s.to_string()).collect();
            rh_web::serve(
                assembled.ctx.clone(),
                plugins,
                addr.parse()?,
                models_file.into(),
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
    _extra: Vec<Disposer>,
}

/// Mount the plugin tree in order. Every part of the harness — session
/// store, shell, filesystem, tool registry, model — is a plugin; there is
/// no privileged core to patch.
fn assemble(http: bool) -> anyhow::Result<Assembled> {
    let ctx = Context::new();
    let mut disposers: Vec<Disposers> = Vec::new();
    let mut extra: Vec<Disposer> = Vec::new();

    let plugins: Vec<Arc<dyn Plugin>> = vec![
        Arc::new(SessionPlugin),
        Arc::new(ShellPlugin),
        Arc::new(FileSystemPlugin),
        Arc::new(ToolsPlugin),
        Arc::new(MockModelPlugin),
    ];

    let mut names = Vec::new();
    for plugin in plugins {
        names.push(plugin.name());
        disposers.push(plugin.mount(&ctx)?);
    }

    if http {
        mount_http_model(&ctx, &mut extra)?;
        names.push("model:http");
    }

    Ok(Assembled {
        ctx,
        plugins: names,
        _disposers: disposers,
        _extra: extra,
    })
}

/// Replace the mock model provider with the env-configured HTTP one.
fn mount_http_model(ctx: &Context, extra: &mut Vec<Disposer>) -> anyhow::Result<()> {
    let provider = rh_providers::OpenAiCompatibleProvider::from_env()?;
    let disposer = ctx.provide_named(
        "ModelProvider(http)",
        Arc::new(provider) as Arc<dyn ModelProvider>,
    );
    extra.push(disposer);
    Ok(())
}

async fn run(task: &str, http: bool) -> anyhow::Result<()> {
    let assembled = assemble(http)?;
    let ctx = assembled.ctx.clone();

    let store = ctx
        .service::<SessionStore>()
        .ok_or_else(|| anyhow::anyhow!("no session store registered"))?;
    let session = store.create_fresh();

    let definition = AgentDefinition {
        name: "rh-agent".to_string(),
        model: if http {
            std::env::var("RH_MODEL").unwrap_or_else(|_| "deepseek-chat".to_string())
        } else {
            "mock".to_string()
        },
        system_prompt: "You are a Rust harness agent. Use tools when useful.".to_string(),
        tool_ids: Vec::new(),
        max_steps: 8,
    };

    let agent = AgentBuilder::new(ctx, definition).build(session)?;
    let report = agent.run(task).await?;

    println!("== session transcript ({}) ==", agent.session().id());
    for event in &report.events {
        println!("{}", serde_json::to_string(event)?);
    }
    Ok(())
}

async fn tools() -> anyhow::Result<()> {
    let assembled = assemble(false)?;
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
    let assembled = assemble(false)?;
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
