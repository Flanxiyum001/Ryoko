//! `ryoko agent` and `ryoko harness`: run Ryoko's agent harness from the terminal.
//!
//! Features DeepSeek Harness-inspired operating profiles (`code`, `standard`,
//! `headless`, `minimal`), durable execution traces (`SessionJournal`),
//! loop guards, and staged edit reviews.
//!
//! Edits are staged, never written. The command prints a summary of every
//! staged file and can save the full proposal as JSON for review. Shell
//! commands are offered to the model only with `--allow-commands`, and each
//! one is confirmed interactively.

use std::error::Error;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use clap::{Args, Subcommand};
use ryoko_harness::{
    ApprovalGate, CommandRequest, Harness, HarnessEvent, HarnessProfile, PlanStatus,
    SessionJournal, StopCause,
};

#[derive(Debug, Args)]
pub struct AgentArgs {
    /// Provider configuration file.
    #[arg(long)]
    pub config: PathBuf,
    /// `provider/model` to drive the agent.
    #[arg(long)]
    pub model: String,
    /// What the agent should do.
    #[arg(long)]
    pub goal: String,
    /// Workspace root. Defaults to the current directory.
    #[arg(long, default_value = ".")]
    pub workspace: PathBuf,
    /// Most model turns before stopping.
    #[arg(long, default_value_t = 40)]
    pub max_turns: usize,
    /// Offer the run_command tool. Every command still asks for confirmation.
    #[arg(long)]
    pub allow_commands: bool,
    /// Disable sub-agents.
    #[arg(long)]
    pub no_delegate: bool,
    /// Save the outcome, including full staged file contents, as JSON.
    #[arg(long)]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct HarnessArgs {
    #[command(subcommand)]
    pub command: HarnessCommand,
}

#[derive(Debug, Subcommand)]
pub enum HarnessCommand {
    /// Run the harness on a goal with a chosen profile (code, standard, headless, minimal).
    Run(Box<HarnessRunArgs>),
    /// List all available harness profiles and their capabilities.
    Profiles,
    /// Inspect a saved session execution trace JSON file.
    Trace(HarnessTraceArgs),
}

#[derive(Debug, Args)]
pub struct HarnessRunArgs {
    /// Provider configuration file.
    #[arg(long)]
    pub config: PathBuf,
    /// `provider/model` to drive the agent (e.g. `deepseek/deepseek-chat`).
    #[arg(long)]
    pub model: String,
    /// What the agent should do.
    #[arg(long)]
    pub goal: String,
    /// Workspace root. Defaults to the current directory.
    #[arg(long, default_value = ".")]
    pub workspace: PathBuf,
    /// Operating profile: code, standard, headless, or minimal. Defaults to `code`.
    #[arg(long, default_value = "code")]
    pub profile: String,
    /// Most model turns before stopping.
    #[arg(long)]
    pub max_turns: Option<usize>,
    /// Offer the run_command tool. Every command still asks for confirmation.
    #[arg(long)]
    pub allow_commands: bool,
    /// Disable sub-agents.
    #[arg(long)]
    pub no_delegate: bool,
    /// Save the outcome, including full staged file contents, as JSON.
    #[arg(long)]
    pub output: Option<PathBuf>,
    /// Save durable execution trace to this JSON file.
    #[arg(long)]
    pub journal: Option<PathBuf>,
    /// Print execution trace at the end of the run.
    #[arg(long)]
    pub trace: bool,
}

#[derive(Debug, Args)]
pub struct HarnessTraceArgs {
    /// Path to a saved session journal JSON file.
    pub journal_file: PathBuf,
}

struct TerminalApproval;

#[async_trait]
impl ApprovalGate for TerminalApproval {
    async fn approve_command(&self, request: &CommandRequest) -> bool {
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            if !io::stdin().is_terminal() {
                eprintln!(
                    "  ! refused `{}`: no terminal to confirm it",
                    request.program
                );
                return false;
            }
            eprint!(
                "\n  The agent wants to run: {} {}\n  Reason: {}\n  Allow? [y/N] ",
                request.program,
                request.args.join(" "),
                request.reason
            );
            let _ = io::stderr().flush();
            let mut answer = String::new();
            io::stdin().lock().read_line(&mut answer).is_ok()
                && matches!(answer.trim(), "y" | "Y" | "yes")
        })
        .await
        .unwrap_or(false)
    }
}

fn print_event(event: HarnessEvent) {
    match event {
        HarnessEvent::TurnStarted { .. } => {}
        HarnessEvent::AssistantText { depth, text } => {
            let indent = "  ".repeat(depth as usize);
            for line in text.lines() {
                eprintln!("{indent}│ {line}");
            }
        }
        HarnessEvent::ToolStarted { depth, name } => {
            eprintln!("{}· {name}", "  ".repeat(depth as usize));
        }
        HarnessEvent::ToolFinished {
            ok: false,
            depth,
            name,
        } => {
            eprintln!("{}  ✗ {name} failed", "  ".repeat(depth as usize));
        }
        HarnessEvent::ToolFinished { .. } => {}
        HarnessEvent::PlanUpdated { items } => {
            eprintln!("  Plan:");
            for item in items {
                let mark = match item.status {
                    PlanStatus::Done => "✓",
                    PlanStatus::InProgress => "▸",
                    PlanStatus::Pending => "○",
                };
                eprintln!("    {mark} {}", item.task);
            }
        }
        HarnessEvent::ChangeStaged { path } => eprintln!("  staged {path}"),
        HarnessEvent::MemorySaved { note } => eprintln!("  remembered: {note}"),
        HarnessEvent::CommandDenied { program } => eprintln!("  command refused: {program}"),
        HarnessEvent::ContextCompacted { results_elided } => {
            eprintln!("  (trimmed {results_elided} old tool results to save context)");
        }
        HarnessEvent::SubAgentStarted { task } => eprintln!("  ↳ sub-agent: {task}"),
        HarnessEvent::SubAgentFinished { .. } => eprintln!("  ↳ sub-agent finished"),
    }
}

pub async fn run(args: AgentArgs) -> Result<(), Box<dyn Error>> {
    run_harness_internal(HarnessRunArgs {
        config: args.config,
        model: args.model,
        goal: args.goal,
        workspace: args.workspace,
        profile: "code".to_owned(),
        max_turns: Some(args.max_turns),
        allow_commands: args.allow_commands,
        no_delegate: args.no_delegate,
        output: args.output,
        journal: None,
        trace: false,
    })
    .await
}

pub async fn run_harness(args: HarnessArgs) -> Result<(), Box<dyn Error>> {
    match args.command {
        HarnessCommand::Profiles => {
            println!("Available Ryoko Harness profiles:");
            for (name, desc) in HarnessProfile::available_profiles() {
                println!("  {name:<10} - {desc}");
            }
            Ok(())
        }
        HarnessCommand::Trace(trace_args) => {
            let journal = SessionJournal::load_from_json(&trace_args.journal_file)?;
            println!("{}", journal.inspect());
            Ok(())
        }
        HarnessCommand::Run(run_args) => run_harness_internal(*run_args).await,
    }
}

async fn run_harness_internal(args: HarnessRunArgs) -> Result<(), Box<dyn Error>> {
    if args.goal.trim().is_empty() {
        return Err("goal cannot be empty".into());
    }
    if args
        .max_turns
        .is_some_and(|turns| !(1..=200).contains(&turns))
    {
        return Err("--max-turns must be between 1 and 200".into());
    }
    let catalog = crate::load_catalog(&args.config)?;
    let router = Arc::new(catalog.build_router_for_models([args.model.as_str()])?);

    let mut profile = HarnessProfile::from_name(&args.profile, args.model.clone())?;
    if let Some(turns) = args.max_turns {
        profile.config.max_turns = turns;
    }
    profile.config.allow_commands = args.allow_commands;
    if args.no_delegate {
        profile.config.max_delegation_depth = 0;
    }

    let harness = Harness::new(&args.workspace, router, profile.config.clone())?
        .with_profile(profile)
        .with_approvals(Arc::new(TerminalApproval))
        .on_event(print_event);

    if !harness.skills().is_empty() {
        eprintln!("Loaded {} skills.", harness.skills().len());
    }
    eprintln!(
        "Running Ryoko Harness [{}] (profile: {}). Edits are staged for review.\n",
        args.model,
        harness.profile().name
    );

    let outcome = harness.run(&args.goal).await;

    println!("{}", outcome.final_text);
    match &outcome.stop {
        StopCause::Completed => {}
        StopCause::TurnLimit => eprintln!("\nStopped after {} turns.", outcome.turns),
        StopCause::Cancelled => eprintln!("\nCancelled."),
        StopCause::ModelError(error) => eprintln!("\nModel error: {error}"),
    }
    if outcome.changes.is_empty() {
        eprintln!("\nNo file changes proposed.");
    } else {
        eprintln!("\nProposed changes ({}):", outcome.changes.len());
        for change in &outcome.changes {
            let (before, after) = (
                change
                    .original
                    .as_deref()
                    .map_or(0, |text| text.lines().count()),
                change.content.lines().count(),
            );
            let label = if change.original.is_none() {
                "new"
            } else {
                "edit"
            };
            eprintln!("  {label:4} {}  ({before} → {after} lines)", change.path);
        }
    }
    if let Some(path) = args.output {
        let json = serde_json::to_string_pretty(&outcome)?;
        crate::experience::create_private_file(&path, &json)?;
        eprintln!("Saved the full proposal to {}", path.display());
    }
    if let Some(path) = args.journal {
        let json = serde_json::to_string_pretty(&harness.journal().events())?;
        crate::experience::create_private_file(&path, &json)?;
        eprintln!("Saved execution trace to {}", path.display());
    }
    if args.trace {
        println!("\n{}", harness.journal().inspect());
    }
    if matches!(outcome.stop, StopCause::ModelError(_)) {
        return Err("agent stopped because of a model error".into());
    }
    Ok(())
}
