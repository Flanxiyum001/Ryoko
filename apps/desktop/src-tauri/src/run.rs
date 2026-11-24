//! One-click CLI runs from Mission Control.
//!
//! The desktop never implements agent execution itself. It starts the installed
//! `ryoko` CLI as a child process with an explicit argv, writes a bounded event
//! log and console log beneath `.ryoko`, and exposes only start/status/pause/
//! resume/cancel. The CLI remains the owner of model calls, worktree isolation
//! and validation.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::paths::create_directories_without_symlinks;
use crate::workspace::{WorkspaceState, current_root};

const MAX_GOAL_CHARS: usize = 8_000;
const MAX_LOG_TAIL: usize = 16_000;
const MAX_BUDGET_USD: f64 = 1_000.0;

#[derive(Default)]
pub struct RunState {
    active: Mutex<Option<ActiveRun>>,
    last: Mutex<Option<FinishedRun>>,
    paused: AtomicBool,
}

struct ActiveRun {
    child: Child,
    pid: u32,
    event_log: String,
    log_path: PathBuf,
    started_ms: u64,
}

struct FinishedRun {
    event_log: String,
    log_path: PathBuf,
    started_ms: u64,
    exit_code: Option<i32>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SandboxInitResult {
    pub image: String,
    pub dockerfile: String,
    pub built: bool,
    pub ecosystem: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RunStatus {
    running: bool,
    paused: bool,
    exit_code: Option<i32>,
    event_log: Option<String>,
    started_ms: Option<u64>,
    log_tail: String,
}

impl RunStatus {
    fn idle() -> Self {
        Self {
            running: false,
            paused: false,
            exit_code: None,
            event_log: None,
            started_ms: None,
            log_tail: String::new(),
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn executable_name() -> &'static str {
    if cfg!(windows) { "ryoko.exe" } else { "ryoko" }
}

/// Resolve the CLI without a shell. An explicit `RYOKO_CLI` override wins so a
/// packaged desktop build can point at a signed/installed CLI.
pub(crate) fn resolve_cli(root: &Path) -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("RYOKO_CLI") {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(parent) = current.parent() {
            candidates.push(parent.join(executable_name()));
            for ancestor in parent.ancestors().take(6) {
                candidates.push(ancestor.join("target/debug").join(executable_name()));
                candidates.push(ancestor.join("target/release").join(executable_name()));
            }
        }
    }
    candidates.push(root.join("target/debug").join(executable_name()));
    candidates.push(root.join("target/release").join(executable_name()));
    if let Some(paths) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&paths) {
            candidates.push(directory.join(executable_name()));
        }
    }
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            "ryoko CLI was not found; set RYOKO_CLI or install `ryoko` on PATH".to_owned()
        })
}

/// A provider-only CLI catalog. The secret itself never appears in this JSON.
fn desktop_provider_catalog(
    setup: &crate::onboarding::SetupState,
) -> Result<serde_json::Value, String> {
    let (base_url, wire) = crate::onboarding::provider_endpoint(&setup.provider)
        .ok_or("unsupported desktop model provider")?;
    let mut profile = serde_json::json!({
        "id": setup.provider,
        "base_url": base_url,
    });
    if setup.provider != "ollama" {
        profile["api_key_env"] = serde_json::json!("RYOKO_DESKTOP_API_KEY");
    }
    if wire != "openai" {
        profile["wire"] = serde_json::json!(wire);
    }
    let catalog = serde_json::json!({
        "default_provider": setup.provider,
        "providers": [profile],
    });
    ryoko_models::ProviderCatalog::parse_json(&catalog.to_string())
        .map_err(|error| format!("invalid native model setup: {error}"))?;
    Ok(catalog)
}

/// Mission Control delegates execution to the ryoko CLI. Forward the chosen
/// model and ephemeral credentials to that child process only. The on-disk
/// provider catalog contains an environment variable NAME, never the API key.
fn configure_desktop_cli(command: &mut Command, logs: &Path, stamp: u64) -> Result<(), String> {
    let setup = crate::onboarding::load()?;
    if !setup.configured {
        return Ok(());
    }
    let catalog = desktop_provider_catalog(&setup)?;
    let secret = if setup.provider == "ollama" {
        None
    } else {
        Some(crate::credential_store::retrieve(&setup.provider)?.ok_or(
            "API key is missing from the OS keychain. Configure the provider again on Welcome.",
        )?)
    };
    let path = logs.join(format!("desktop-provider-{stamp}.json"));
    let mut file = private_file(&path)?;
    if let Err(error) = (|| -> Result<(), String> {
        serde_json::to_writer_pretty(&mut file, &catalog).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())
    })() {
        let _ = std::fs::remove_file(&path);
        return Err(format!(
            "could not create private CLI provider catalog: {error}"
        ));
    }
    command
        .arg("--config")
        .arg(&path)
        .env("RYOKO_MODEL", &setup.model);
    if let Some(secret) = secret {
        command.env("RYOKO_DESKTOP_API_KEY", secret);
    }
    Ok(())
}

fn private_file(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))
}

fn ensure_run_dir(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let directory = root.join(relative);
    create_directories_without_symlinks(root, &directory)?;
    let canonical = directory
        .canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", directory.display()))?;
    if !canonical.starts_with(root) || !canonical.is_dir() {
        return Err("run directory escapes the workspace".to_owned());
    }
    Ok(canonical)
}

fn log_tail(path: &Path) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let Ok(metadata) = file.metadata() else {
        return String::new();
    };
    let length = metadata.len();
    let start = length.saturating_sub(MAX_LOG_TAIL as u64);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    if file
        .take(MAX_LOG_TAIL as u64)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return String::new();
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0 {
        text.insert_str(0, "… earlier output truncated …\n");
    }
    text
}

fn finish(active: ActiveRun, exit_code: Option<i32>) -> FinishedRun {
    FinishedRun {
        event_log: active.event_log,
        log_path: active.log_path,
        started_ms: active.started_ms,
        exit_code,
    }
}

fn snapshot(
    running: bool,
    paused: bool,
    exit_code: Option<i32>,
    event_log: Option<String>,
    started_ms: Option<u64>,
    log_path: &Path,
) -> RunStatus {
    RunStatus {
        running,
        paused,
        exit_code,
        event_log,
        started_ms,
        log_tail: log_tail(log_path),
    }
}

#[tauri::command]
pub async fn sandbox_init(
    workspace: State<'_, WorkspaceState>,
) -> Result<SandboxInitResult, String> {
    let root = current_root(&workspace)?;
    let cli = resolve_cli(&root)?;
    tauri::async_runtime::spawn_blocking(move || {
        let output = Command::new(&cli)
            .current_dir(&root)
            .args(["sandbox", "init", "--repo", ".", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| format!("cannot start sandbox initialization: {error}"))?;
        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "sandbox initialization failed: {}",
                detail.chars().take(4_000).collect::<String>()
            ));
        }
        let stdout = String::from_utf8(output.stdout)
            .map_err(|_| "sandbox initialization returned non-UTF-8 output".to_owned())?;
        let line = stdout
            .lines()
            .rev()
            .find(|line| line.trim_start().starts_with('{'))
            .ok_or_else(|| "sandbox initialization returned no result".to_owned())?;
        let value: serde_json::Value = serde_json::from_str(line)
            .map_err(|error| format!("invalid sandbox result: {error}"))?;
        let field = |name: &str| {
            value
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("sandbox result is missing {name}"))
        };
        Ok(SandboxInitResult {
            image: field("image")?,
            dockerfile: field("dockerfile")?,
            ecosystem: field("ecosystem")?,
            built: value
                .get("built")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        })
    })
    .await
    .map_err(|error| format!("sandbox initialization task failed: {error}"))?
}

#[tauri::command]
#[allow(clippy::too_many_arguments)] // flat Tauri IPC signature
pub fn start_run(
    goal: String,
    sandbox_image: String,
    budget_usd: f64,
    max_concurrency: usize,
    team: bool,
    agent: Option<String>,
    attempts: Option<u32>,
    config_path: Option<String>,
    check_command: Option<String>,
    state: State<'_, RunState>,
    workspace: State<'_, WorkspaceState>,
) -> Result<RunStatus, String> {
    let goal = goal.trim();
    if goal.is_empty() {
        return Err("goal cannot be empty".to_owned());
    }
    if goal.chars().count() > MAX_GOAL_CHARS {
        return Err(format!("goal must be at most {MAX_GOAL_CHARS} characters"));
    }
    if !budget_usd.is_finite() || budget_usd <= 0.0 || budget_usd > MAX_BUDGET_USD {
        return Err(format!(
            "budget must be a positive finite number no greater than {MAX_BUDGET_USD}"
        ));
    }
    if team && !(1..=4).contains(&max_concurrency) {
        return Err("max concurrency must be between 1 and 4 for a team run".to_owned());
    }
    let image = sandbox_image.trim();
    if image.is_empty() {
        return Err("a sandbox image is required for pre-flight validation".to_owned());
    }
    if image.starts_with('-') || image.chars().any(char::is_whitespace) {
        return Err("sandbox image must be a single OCI image reference".to_owned());
    }
    let root = current_root(&workspace)?;
    let cli = resolve_cli(&root)?;

    let mut active = state
        .active
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(run) = active.as_mut() {
        match run.child.try_wait() {
            Ok(None) => return Err("an agent run is already active".to_owned()),
            Ok(Some(status)) => {
                let finished = finish(active.take().expect("active run"), status.code());
                *state.last.lock().unwrap_or_else(|error| error.into_inner()) = Some(finished);
            }
            Err(error) => return Err(format!("cannot inspect active run: {error}")),
        }
    }

    let events = ensure_run_dir(&root, ".ryoko/events")?;
    let logs = ensure_run_dir(&root, ".ryoko/runs")?;
    let stamp = now_ms();
    let event_name = format!("desktop-{stamp}.jsonl");
    let event_path = events.join(&event_name);
    private_file(&event_path)?;
    let log_path = logs.join(format!("desktop-{stamp}.log"));
    let log_file = private_file(&log_path)?;
    let stderr = log_file
        .try_clone()
        .map_err(|error| format!("cannot share run log: {error}"))?;

    let mut command = Command::new(&cli);
    command
        .current_dir(&root)
        .arg("run")
        .args(["--repo", "."])
        .args(["--event-log"])
        .arg(format!(".ryoko/events/{event_name}"))
        .args(["--sandbox-image", image])
        .args(["--budget-usd", &budget_usd.to_string()])
        .args([
            "--attempts",
            &attempts.unwrap_or(3).clamp(1, 10).to_string(),
        ]);
    if team {
        command
            .arg("--team")
            .args(["--max-concurrency", &max_concurrency.to_string()]);
    }
    if let Some(agent) = agent
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if agent.starts_with('-')
            || agent.chars().count() > 80
            || agent.chars().any(char::is_control)
        {
            return Err("agent name is invalid".to_owned());
        }
        command.args(["--agent", agent]);
    }
    if let Some(config) = config_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let path = PathBuf::from(&config);
        let resolved = if path.is_absolute() {
            path
        } else {
            root.join(path)
        };
        if !resolved.is_file() {
            return Err("provider config file does not exist".to_owned());
        }
        command.args(["--config"]).arg(resolved);
    }
    if config_path
        .as_deref()
        .is_none_or(|config| config.trim().is_empty())
    {
        let workspace_catalog =
            crate::settings::settings_dir(&root).join(crate::settings::PROVIDERS_FILE);
        if workspace_catalog.is_file() {
            command.arg("--config").arg(workspace_catalog);
            // The workspace catalog is an explicit override of onboarding.
            if let Ok(setup) = crate::onboarding::load() {
                if setup.configured {
                    command.env("RYOKO_MODEL", setup.model);
                }
            }
        } else {
            configure_desktop_cli(&mut command, &logs, stamp)?;
        }
    }
    if let Some(check) = check_command
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    {
        if check.starts_with('-')
            || check.chars().count() > 500
            || check.chars().any(char::is_control)
        {
            return Err("check command is invalid".to_owned());
        }
        command.args(["--check-command", &check]);
    }
    // The goal is positional and untrusted. `--` prevents a goal that begins
    // with `-` from being parsed as an additional CLI flag.
    command.arg("--").arg(goal);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(stderr))
        .env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command
        .spawn()
        .map_err(|error| format!("cannot start ryoko CLI: {error}"))?;
    let pid = child.id();
    let started_ms = now_ms();
    *active = Some(ActiveRun {
        child,
        pid,
        event_log: event_name,
        log_path: log_path.clone(),
        started_ms,
    });
    state.paused.store(false, Ordering::SeqCst);
    Ok(snapshot(
        true,
        false,
        None,
        active.as_ref().map(|run| run.event_log.clone()),
        Some(started_ms),
        &log_path,
    ))
}

#[derive(Serialize, Deserialize)]
pub struct ReviewValidation {
    passed: bool,
    summary: String,
    logs: Vec<String>,
    duration_ms: u64,
}

#[tauri::command]
pub async fn validate_review(
    sandbox_image: String,
    timeout_secs: Option<u64>,
    workspace: State<'_, WorkspaceState>,
) -> Result<ReviewValidation, String> {
    let image = sandbox_image.trim();
    if image.is_empty() {
        return Err("a sandbox image is required for validation".to_owned());
    }
    if image.starts_with('-') || image.chars().any(char::is_whitespace) {
        return Err("sandbox image must be a single OCI image reference".to_owned());
    }
    let root = current_root(&workspace)?;
    let cli = resolve_cli(&root)?;
    let timeout = timeout_secs.unwrap_or(300).clamp(1, 1_800);
    let mut command = Command::new(cli);
    command
        .current_dir(&root)
        .args(["check", "--repo", ".", "--sandbox-image", image])
        .args(["--timeout-secs", &timeout.to_string(), "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = tauri::async_runtime::spawn_blocking(move || command.output())
        .await
        .map_err(|_| "validation worker failed".to_owned())?
        .map_err(|error| format!("cannot start validation: {error}"))?;
    if output.stdout.is_empty() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "validation produced no report: {}",
            detail.chars().take(2_000).collect::<String>()
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("cannot parse validation report: {error}"))
}

#[tauri::command]
pub fn run_status(state: State<'_, RunState>) -> Result<RunStatus, String> {
    let mut active = state
        .active
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(run) = active.as_mut() {
        match run.child.try_wait() {
            Ok(None) => {
                return Ok(snapshot(
                    true,
                    state.paused.load(Ordering::SeqCst),
                    None,
                    Some(run.event_log.clone()),
                    Some(run.started_ms),
                    &run.log_path,
                ));
            }
            Ok(Some(status)) => {
                let finished = finish(active.take().expect("active run"), status.code());
                let result = snapshot(
                    false,
                    false,
                    finished.exit_code,
                    Some(finished.event_log.clone()),
                    Some(finished.started_ms),
                    &finished.log_path,
                );
                *state.last.lock().unwrap_or_else(|error| error.into_inner()) = Some(finished);
                return Ok(result);
            }
            Err(error) => return Err(format!("cannot inspect active run: {error}")),
        }
    }
    let last = state.last.lock().unwrap_or_else(|error| error.into_inner());
    Ok(match last.as_ref() {
        Some(finished) => snapshot(
            false,
            false,
            finished.exit_code,
            Some(finished.event_log.clone()),
            Some(finished.started_ms),
            &finished.log_path,
        ),
        None => RunStatus::idle(),
    })
}

#[cfg(unix)]
fn signal_group(pid: u32, signal: &str) -> Result<(), String> {
    let status = Command::new("kill")
        .args([signal, &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("cannot signal run: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("the run has already stopped".to_owned())
    }
}

#[tauri::command]
pub fn pause_run(state: State<'_, RunState>) -> Result<RunStatus, String> {
    let active = state
        .active
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let run = active
        .as_ref()
        .ok_or_else(|| "no run is active".to_owned())?;
    #[cfg(unix)]
    {
        signal_group(run.pid, "-STOP")?;
        state.paused.store(true, Ordering::SeqCst);
        Ok(snapshot(
            true,
            true,
            None,
            Some(run.event_log.clone()),
            Some(run.started_ms),
            &run.log_path,
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = run;
        Err("pause is not supported on this platform; cancel the run instead".to_owned())
    }
}

#[tauri::command]
pub fn resume_run(state: State<'_, RunState>) -> Result<RunStatus, String> {
    let active = state
        .active
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let run = active
        .as_ref()
        .ok_or_else(|| "no run is active".to_owned())?;
    #[cfg(unix)]
    {
        signal_group(run.pid, "-CONT")?;
        state.paused.store(false, Ordering::SeqCst);
        Ok(snapshot(
            true,
            false,
            None,
            Some(run.event_log.clone()),
            Some(run.started_ms),
            &run.log_path,
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = run;
        Err("resume is not supported on this platform; cancel the run instead".to_owned())
    }
}

#[tauri::command]
pub fn cancel_run(state: State<'_, RunState>) -> Result<RunStatus, String> {
    let mut active = state
        .active
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let Some(mut run) = active.take() else {
        state.paused.store(false, Ordering::SeqCst);
        return Ok(RunStatus::idle());
    };
    #[cfg(unix)]
    {
        let _ = signal_group(run.pid, "-TERM");
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &run.pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    for _ in 0..20 {
        match run.child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break,
        }
    }
    if run.child.try_wait().ok().flatten().is_none() {
        #[cfg(unix)]
        {
            let _ = signal_group(run.pid, "-KILL");
        }
        let _ = run.child.kill();
    }
    let status = run.child.wait().ok();
    let finished = finish(run, status.and_then(|status| status.code()));
    let result = snapshot(
        false,
        false,
        finished.exit_code,
        Some(finished.event_log.clone()),
        Some(finished.started_ms),
        &finished.log_path,
    );
    *state.last.lock().unwrap_or_else(|error| error.into_inner()) = Some(finished);
    state.paused.store(false, Ordering::SeqCst);
    Ok(result)
}

/// Best-effort cleanup when the desktop window exits. No UI action or
/// confirmation is possible here, so only a polite terminate signal is sent.
pub fn shutdown(state: &RunState) {
    let mut active = state
        .active
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(run) = active.as_mut() {
        #[cfg(unix)]
        {
            let _ = signal_group(run.pid, "-TERM");
        }
        #[cfg(not(unix))]
        {
            let _ = run.child.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{desktop_provider_catalog, executable_name};
    use crate::onboarding::SetupState;

    #[test]
    fn cli_provider_catalog_does_not_contain_any_secrets() {
        for provider in ["openai", "anthropic", "gemini", "ollama"] {
            let setup = SetupState {
                provider: provider.to_owned(),
                model: "sample-model".to_owned(),
                configured: true,
                recent_folders: vec![],
            };
            let catalog = desktop_provider_catalog(&setup).expect("catalog is valid");
            let json = catalog.to_string();
            assert!(!json.contains("api-key-secret"));
            if provider == "ollama" {
                assert!(catalog["providers"][0].get("api_key_env").is_none());
            } else {
                assert_eq!(
                    catalog["providers"][0]["api_key_env"],
                    "RYOKO_DESKTOP_API_KEY"
                );
            }
            if provider == "anthropic" || provider == "gemini" {
                assert_eq!(catalog["providers"][0]["wire"], provider);
            }
        }
    }

    #[test]
    fn cli_name_matches_the_host() {
        if cfg!(windows) {
            assert_eq!(executable_name(), "ryoko.exe");
        } else {
            assert_eq!(executable_name(), "ryoko");
        }
    }
}
