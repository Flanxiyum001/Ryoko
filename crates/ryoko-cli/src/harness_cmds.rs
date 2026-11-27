//! `ryoko find`, `ryoko review-diff` and `ryoko swarm`.

use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use clap::Args;
use ryoko_harness::review::{self, Severity, Verdict};
use ryoko_harness::swarm::push_branch;
use ryoko_harness::{
    FastContext, GitSwarmWorkspace, HarnessEvent, Swarm, SwarmConfig, SwarmEvent, SwarmTaskStatus,
};

#[derive(Debug, Args)]
pub struct FindArgs {
    /// What you are looking for, in words or identifiers.
    pub query: String,
    #[arg(long, default_value = ".")]
    pub workspace: PathBuf,
    #[arg(long, default_value_t = 8)]
    pub limit: usize,
}

pub fn find(args: FindArgs) -> Result<(), Box<dyn Error>> {
    let index = FastContext::build(&args.workspace)?;
    let started = Instant::now();
    let hits = index.search(&args.query, args.limit.clamp(1, 50));
    let search_time = started.elapsed();
    for hit in &hits {
        println!("{}:{}-{}", hit.path, hit.start_line, hit.end_line);
        for (offset, line) in hit.snippet.lines().enumerate() {
            println!("  {:>5} │ {line}", hit.start_line + offset);
        }
        println!();
    }
    eprintln!(
        "{} result(s) · indexed {} files in {:.0?} · searched in {:.1?}{}",
        hits.len(),
        index.file_count(),
        index.build_time(),
        search_time,
        if index.truncated() {
            " · index capped"
        } else {
            ""
        }
    );
    Ok(())
}

#[derive(Debug, Args)]
pub struct ReviewDiffArgs {
    #[arg(long)]
    pub config: PathBuf,
    #[arg(long)]
    pub model: String,
    #[arg(long, default_value = ".")]
    pub workspace: PathBuf,
    /// Review staged changes only (`git diff --cached`).
    #[arg(long, conflicts_with = "base")]
    pub staged: bool,
    /// Review everything since this branch or commit, for example `main`.
    #[arg(long)]
    pub base: Option<String>,
    /// Exit with status 1 when the review finds a blocker.
    #[arg(long)]
    pub fail_on_blocker: bool,
}

pub async fn review_diff(args: ReviewDiffArgs) -> Result<(), Box<dyn Error>> {
    let mut git_args = vec![
        "diff".to_owned(),
        "--no-color".to_owned(),
        "--no-ext-diff".to_owned(),
    ];
    if args.staged {
        git_args.push("--cached".into());
    } else if let Some(base) = &args.base {
        if base.starts_with('-') || base.chars().any(char::is_whitespace) {
            return Err("invalid --base".into());
        }
        git_args.push(format!("{base}...HEAD"));
    } else {
        git_args.push("HEAD".into());
    }
    let output = tokio::process::Command::new("git")
        .args(&git_args)
        .current_dir(&args.workspace)
        .output()
        .await?;
    if !output.status.success() {
        return Err(format!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let diff = String::from_utf8_lossy(&output.stdout).into_owned();
    let catalog = crate::load_catalog(&args.config)?;
    let router = catalog.build_router_for_models([args.model.as_str()])?;
    let index = FastContext::build(&args.workspace).ok();
    eprintln!(
        "Ryoko Review is reading {} file(s)…",
        review::diff_files(&diff).len()
    );
    let report = review::review_diff(&router, &args.model, &diff, index.as_ref()).await?;
    println!(
        "{}  {}\n",
        match report.verdict {
            Verdict::Approve => "✓ Looks good",
            Verdict::NeedsChanges => "✗ Needs changes",
        },
        report.summary
    );
    for finding in &report.findings {
        let label = match finding.severity {
            Severity::Blocker => "BLOCKER   ",
            Severity::Warning => "warning   ",
            Severity::Suggestion => "suggestion",
        };
        let at = finding.line.map_or_else(
            || finding.path.clone(),
            |line| format!("{}:{line}", finding.path),
        );
        println!("{label} {at}  {}", finding.title);
        if !finding.detail.is_empty() {
            println!(
                "           {}",
                finding.detail.replace('\n', "\n           ")
            );
        }
    }
    if args.fail_on_blocker
        && report
            .findings
            .iter()
            .any(|f| f.severity == Severity::Blocker)
    {
        std::process::exit(1);
    }
    Ok(())
}

#[derive(Debug, Args)]
pub struct SwarmArgs {
    #[arg(long)]
    pub config: PathBuf,
    #[arg(long)]
    pub model: String,
    /// Model for Ryoko Review; defaults to --model.
    #[arg(long)]
    pub review_model: Option<String>,
    #[arg(long)]
    pub goal: String,
    /// Git repository to work in.
    #[arg(long, default_value = ".")]
    pub workspace: PathBuf,
    /// Sub-agents working at the same time (1–6).
    #[arg(long, default_value_t = 3)]
    pub parallel: usize,
    #[arg(long, default_value_t = 40)]
    pub max_turns: usize,
    /// Push every ready branch to origin and print a link that opens its pull request.
    #[arg(long)]
    pub push: bool,
    /// Save the full report as JSON.
    #[arg(long)]
    pub output: Option<PathBuf>,
}

pub async fn swarm(args: SwarmArgs) -> Result<(), Box<dyn Error>> {
    if args.goal.trim().is_empty() {
        return Err("goal cannot be empty".into());
    }
    let mut models = vec![args.model.as_str()];
    if let Some(review) = &args.review_model {
        models.push(review.as_str());
    }
    let catalog = crate::load_catalog(&args.config)?;
    let router = Arc::new(catalog.build_router_for_models(models)?);
    let mut config = SwarmConfig::new(args.model.clone());
    config.review_model = args.review_model.clone();
    config.max_parallel = args.parallel.clamp(1, 6);
    config.max_turns_per_task = args.max_turns.clamp(1, 200);
    let workspace = Arc::new(GitSwarmWorkspace::new(&args.workspace));
    let swarm = Swarm::new(router, workspace, config).on_event(|event| match event {
        SwarmEvent::Planned { tasks } => {
            eprintln!("Plan: {} task(s)", tasks.len());
            for (index, task) in tasks.iter().enumerate() {
                eprintln!("  {}. {}", index + 1, task.title);
            }
        }
        SwarmEvent::TaskStarted { index, branch } => {
            eprintln!("[{}] started on {branch}", index + 1)
        }
        SwarmEvent::TaskProgress {
            index,
            event: HarnessEvent::ChangeStaged { path },
        } => {
            eprintln!("[{}] changed {path}", index + 1);
        }
        SwarmEvent::TaskReviewing { index } => eprintln!("[{}] reviewing…", index + 1),
        SwarmEvent::TaskFinished { index, result } => {
            let state = match &result.status {
                SwarmTaskStatus::Ready => "ready".to_owned(),
                SwarmTaskStatus::NoChanges => "no changes".to_owned(),
                SwarmTaskStatus::Failed(error) => format!("failed: {error}"),
            };
            eprintln!("[{}] {state}", index + 1);
        }
        SwarmEvent::TaskProgress { .. } => {}
    });
    eprintln!("Each task runs in its own worktree and branch. Your checkout is not changed.\n");
    let report = swarm.run(&args.goal).await;

    println!("\nResults");
    for (index, task) in report.tasks.iter().enumerate() {
        println!("\n{}. {}", index + 1, task.pr_title);
        match (&task.status, &task.branch, &task.base_branch) {
            (SwarmTaskStatus::Ready, Some(branch), Some(base)) => {
                println!("   branch: {branch} ({} file(s))", task.files.len());
                if let Some(review) = &task.review {
                    let blockers = review
                        .findings
                        .iter()
                        .filter(|f| f.severity == Severity::Blocker)
                        .count();
                    println!(
                        "   review: {} ({} finding(s), {blockers} blocker(s))",
                        review.summary,
                        review.findings.len()
                    );
                }
                if args.push {
                    match push_branch(&args.workspace, branch, base, &task.pr_title, &task.pr_body)
                        .await
                    {
                        Ok(Some(url)) => println!("   pushed · open the pull request: {url}"),
                        Ok(None) => println!(
                            "   pushed (origin is not GitHub; open the pull request on your host)"
                        ),
                        Err(error) => println!("   push failed: {error}"),
                    }
                } else {
                    println!(
                        "   review it with `ryoko sessions`, or rerun with --push to open a pull request"
                    );
                }
            }
            (SwarmTaskStatus::Failed(error), ..) => println!("   failed: {error}"),
            _ => println!("   no changes"),
        }
    }
    if let Some(path) = args.output {
        let json = serde_json::to_string_pretty(&report)?;
        crate::experience::create_private_file(&path, &json)?;
        eprintln!("\nSaved the report to {}", path.display());
    }
    Ok(())
}
