//! Config-driven benchmark runner.
//!
//! Example:
//!   cargo run -p hermes-eval --bin hermes-bench -- \
//!     --benchmark crates/hermes-eval/benchmarks/configured-smoke.toml \
//!     --rollout noop --print-json

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, ValueEnum};
use hermes_config::{load_config, GatewayConfig};
use hermes_core::SkillProvider;
#[cfg(feature = "agent-loop")]
use hermes_eval::AgentLoopRollout;
use hermes_eval::{
    ConfiguredBenchmarkAdapter, EvalError, EvalResult, JsonReporter, NoopRollout, Reporter, Runner,
    RunnerConfig,
};
use hermes_skills::{FileSkillStore, SkillManager};
use hermes_tools::ToolRegistry;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RolloutKind {
    Agent,
    Noop,
}

#[derive(Debug, Parser)]
#[command(name = "hermes-bench")]
#[command(about = "Run configured benchmark datasets against Hermes Agent rollouts")]
struct Args {
    /// Benchmark dataset path (.toml or .json).
    #[arg(long)]
    benchmark: PathBuf,

    /// Override model for the run (e.g. nous:Hermes-4-70B).
    #[arg(long)]
    model: Option<String>,

    /// Rollout backend (agent = real AgentLoop, noop = deterministic stub).
    #[arg(long, value_enum, default_value_t = RolloutKind::Agent)]
    rollout: RolloutKind,

    /// Config directory used by load_config() for provider/tool settings.
    #[arg(long)]
    config_dir: Option<PathBuf>,

    /// Parallel task concurrency.
    #[arg(long, default_value_t = 1)]
    concurrency: u32,

    /// Optional cap on number of tasks from the dataset.
    #[arg(long)]
    max_tasks: Option<u32>,

    /// Comma-separated task filter tokens (task_id/category substring match).
    #[arg(long)]
    task_filter: Option<String>,

    /// Deterministic run seed.
    #[arg(long, default_value_t = 0)]
    seed: u64,

    /// Stop immediately on first failed/errored task.
    #[arg(long, default_value_t = false)]
    fail_fast: bool,

    /// Embed full AgentResult in verifier input (larger artifacts).
    #[arg(long, default_value_t = false)]
    full_state: bool,

    /// Output path for JSON run record.
    #[arg(long)]
    output: Option<PathBuf>,

    /// Print the run record JSON to stdout.
    #[arg(long, default_value_t = false)]
    print_json: bool,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hermes-bench error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> EvalResult<()> {
    let args = Args::parse();
    let benchmark_path = args.benchmark.clone();

    let cfg = load_gateway_config(args.config_dir.as_deref())?;
    let model = resolve_model(&args, &cfg);
    let output_path = args
        .output
        .clone()
        .unwrap_or_else(|| default_output_path(&benchmark_path));

    let runner = Runner::new(RunnerConfig {
        model: model.clone(),
        concurrency: args.concurrency.max(1),
        max_tasks: args.max_tasks,
        task_filter: args.task_filter.clone(),
        seed: args.seed,
        continue_on_error: !args.fail_fast,
    });

    let adapter = Arc::new(ConfiguredBenchmarkAdapter::from_path(&benchmark_path)?);
    let record = match args.rollout {
        RolloutKind::Noop => runner.run(adapter, Arc::new(NoopRollout)).await?,
        RolloutKind::Agent => {
            #[cfg(feature = "agent-loop")]
            {
                run_agent_rollout(adapter, &runner, &cfg, &model, args.full_state).await?
            }
            #[cfg(not(feature = "agent-loop"))]
            {
                return Err(EvalError::Other(
                    "agent rollout requested but hermes-eval built without `agent-loop` feature"
                        .to_string(),
                ));
            }
        }
    };

    JsonReporter.write_run(&record, &output_path)?;
    if args.print_json {
        let json = serde_json::to_string_pretty(&record)?;
        println!("{json}");
    } else {
        println!(
            "run={} benchmark={} total={} passed={} failed={} error={} timeout={} output={}",
            record.run_id,
            record.benchmark.id,
            record.metrics.total,
            record.metrics.passed,
            record.metrics.failed,
            record.metrics.error,
            record.metrics.timeout,
            output_path.display()
        );
    }

    Ok(())
}

fn load_gateway_config(config_dir: Option<&Path>) -> EvalResult<GatewayConfig> {
    let config_dir_owned = config_dir.map(|p| p.to_string_lossy().to_string());
    load_config(config_dir_owned.as_deref()).map_err(|e| EvalError::Other(e.to_string()))
}

fn resolve_model(args: &Args, config: &GatewayConfig) -> String {
    if let Some(m) = args
        .model
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        return m.to_string();
    }
    if let Some(m) = config
        .model
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        return m.to_string();
    }
    std::env::var("HERMES_EVAL_MODEL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "nous:Hermes-4-70B".to_string())
}

fn default_output_path(benchmark: &Path) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let stem = benchmark
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("benchmark");
    PathBuf::from("evals").join(format!("{stem}-{stamp}.json"))
}

#[cfg(feature = "agent-loop")]
async fn run_agent_rollout(
    adapter: Arc<ConfiguredBenchmarkAdapter>,
    runner: &Runner,
    config: &GatewayConfig,
    model: &str,
    full_state: bool,
) -> EvalResult<hermes_eval::RunRecord> {
    let tool_registry = Arc::new(ToolRegistry::new());
    let terminal_backend = hermes_cli::terminal_backend::build_terminal_backend(config);
    let skill_store = Arc::new(FileSkillStore::new(FileSkillStore::default_dir()));
    let skill_provider: Arc<dyn SkillProvider> = Arc::new(SkillManager::new(skill_store));
    hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);
    hermes_cli::runtime_tool_wiring::wire_stdio_clarify_backend(&tool_registry);

    let agent_tools = Arc::new(hermes_cli::app::bridge_tool_registry(&tool_registry));
    let provider = hermes_cli::app::build_provider(config, model);
    let agent_config = hermes_cli::app::build_agent_config(config, model);
    let agent = Arc::new(hermes_agent::AgentLoop::new(
        agent_config,
        agent_tools,
        provider,
    ));
    let rollout = Arc::new(AgentLoopRollout::new(agent).with_full_state(full_state));
    runner.run(adapter, rollout).await
}
