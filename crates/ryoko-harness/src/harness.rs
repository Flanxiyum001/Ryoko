//! The bounded agent loop.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use ryoko_models::{ChatRequest, ContentBlock, Message, ModelRouter, Role, ToolDefinition};
use ryoko_tools::{ReadOnlyTools, ToolRequest, ToolResponse};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::context::{self, FastContext};
use crate::loop_guard::LoopGuard;
use crate::memory::ProjectMemory;
use crate::plugin::{
    HarnessPlugin, PluginContext, PluginRegistry, SessionMeta, StepDecision, ToolDecision,
};
use crate::profile::HarnessProfile;
use crate::prompt::{self, SystemPromptComposer};
use crate::session::{SessionEvent, SessionJournal};
use crate::skills::{self, Skill};
use crate::tools::{
    self, ApprovalGate, BUILTIN_NAMES, DenyAllCommands, ExternalTool, MAX_STAGED_FILE_BYTES,
    MAX_STAGED_FILES, ProposedChange, ToolSet,
};

/// Host-chosen limits. Agents cannot change these.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessConfig {
    /// `provider/model` identifier routed through [`ModelRouter`].
    pub model: String,
    /// Model turns before the run stops.
    pub max_turns: usize,
    /// Tool calls accepted in one model turn.
    pub max_calls_per_turn: usize,
    pub max_output_tokens: u32,
    /// Bytes of one tool result kept in context.
    pub tool_output_limit: usize,
    /// Approximate characters of conversation kept before old tool output is
    /// compacted.
    pub context_char_budget: usize,
    /// Offer `run_command` (each call still needs approval).
    pub allow_commands: bool,
    /// Sub-agent nesting allowed; 0 disables `delegate`.
    pub max_delegation_depth: u8,
    /// Ledger scope for budget accounting.
    pub scope: Option<String>,
}

impl HarnessConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            max_turns: 40,
            max_calls_per_turn: 16,
            max_output_tokens: 8192,
            tool_output_limit: 32 * 1024,
            context_char_budget: 240_000,
            allow_commands: false,
            max_delegation_depth: 1,
            scope: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Pending,
    InProgress,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    pub task: String,
    pub status: PlanStatus,
}

/// Progress reported to the host while a run is in flight.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum HarnessEvent {
    TurnStarted { depth: u8, turn: usize },
    AssistantText { depth: u8, text: String },
    ToolStarted { depth: u8, name: String },
    ToolFinished { depth: u8, name: String, ok: bool },
    PlanUpdated { items: Vec<PlanItem> },
    ChangeStaged { path: String },
    MemorySaved { note: String },
    CommandDenied { program: String },
    ContextCompacted { results_elided: usize },
    SubAgentStarted { task: String },
    SubAgentFinished { summary: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "cause", content = "detail", rename_all = "snake_case")]
pub enum StopCause {
    /// The model answered without asking for more tools.
    Completed,
    TurnLimit,
    Cancelled,
    ModelError(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct HarnessOutcome {
    pub final_text: String,
    /// Staged edits for review, sorted by path. Nothing has been written.
    pub changes: Vec<ProposedChange>,
    pub plan: Vec<PlanItem>,
    pub turns: usize,
    pub stop: StopCause,
}

#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("workspace is not a readable directory: {0}")]
    Workspace(String),
}

type EventFn = Arc<dyn Fn(HarnessEvent) + Send + Sync>;

#[derive(Default)]
struct RunState {
    changes: BTreeMap<String, ProposedChange>,
    plan: Vec<PlanItem>,
    file_revisions: HashMap<String, usize>,
}

pub struct Harness {
    root: PathBuf,
    reader: ReadOnlyTools,
    models: Arc<ModelRouter>,
    config: HarnessConfig,
    profile: HarnessProfile,
    approvals: Arc<dyn ApprovalGate>,
    external: Vec<Arc<dyn ExternalTool>>,
    events: Option<EventFn>,
    cancel: Arc<AtomicBool>,
    memory: ProjectMemory,
    skills: Vec<Skill>,
    fast_context: OnceLock<Result<FastContext, String>>,
    plugins: PluginRegistry,
    journal: Arc<SessionJournal>,
}

impl Harness {
    pub fn new(
        root: &Path,
        models: Arc<ModelRouter>,
        config: HarnessConfig,
    ) -> Result<Self, HarnessError> {
        let reader =
            ReadOnlyTools::new(root).map_err(|error| HarnessError::Workspace(error.to_string()))?;
        let root = root
            .canonicalize()
            .map_err(|error| HarnessError::Workspace(error.to_string()))?;
        let profile = HarnessProfile::code(config.model.clone());
        let journal = Arc::new(SessionJournal::new());
        let plugins = PluginRegistry::new();

        Ok(Self {
            memory: ProjectMemory::for_workspace(&root),
            skills: skills::discover(&root),
            root,
            reader,
            models,
            config,
            profile,
            approvals: Arc::new(DenyAllCommands),
            external: Vec::new(),
            events: None,
            cancel: Arc::new(AtomicBool::new(false)),
            fast_context: OnceLock::new(),
            plugins,
            journal,
        })
    }

    /// Sets the operating profile (e.g. `Code`, `Standard`, `Headless`, `Minimal`).
    pub fn with_profile(mut self, profile: HarnessProfile) -> Self {
        self.config = profile.config.clone();
        self.profile = profile;
        self
    }

    /// Mounts a plugin into the harness's plugin registry.
    pub fn with_plugin(mut self, plugin: Arc<dyn HarnessPlugin>) -> Self {
        self.plugins.register(plugin);
        self
    }

    /// Configures a custom session journal for durable trace logging.
    pub fn with_journal(mut self, journal: Arc<SessionJournal>) -> Self {
        self.journal = journal;
        self
    }

    /// Returns a reference to the active session journal.
    pub fn journal(&self) -> Arc<SessionJournal> {
        self.journal.clone()
    }

    /// Returns the active operating profile.
    pub fn profile(&self) -> &HarnessProfile {
        &self.profile
    }

    /// Returns the mounted plugin registry.
    pub fn plugins(&self) -> &PluginRegistry {
        &self.plugins
    }

    pub fn with_approvals(mut self, gate: Arc<dyn ApprovalGate>) -> Self {
        self.approvals = gate;
        self
    }

    /// Adds a host-approved tool. Tools that reuse a built-in name are ignored.
    pub fn with_external_tool(mut self, tool: Arc<dyn ExternalTool>) -> Self {
        if !BUILTIN_NAMES.contains(&tool.definition().name.as_str()) {
            self.external.push(tool);
        }
        self
    }

    pub fn on_event(mut self, callback: impl Fn(HarnessEvent) + Send + Sync + 'static) -> Self {
        self.events = Some(Arc::new(callback));
        self
    }

    /// Set to `true` to stop the run before its next model turn or tool call.
    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    pub fn skills(&self) -> &[Skill] {
        &self.skills
    }

    /// Runs the agent on `goal` until it answers, hits a limit or is cancelled.
    pub async fn run(&self, goal: &str) -> HarnessOutcome {
        let run_start = std::time::Instant::now();
        let loop_guard = Arc::new(Mutex::new(LoopGuard::default()));
        let session_id = uuid::Uuid::new_v4().to_string();
        let filtered_goal = redact_credentials_str(goal);
        let meta = SessionMeta {
            session_id: session_id.clone(),
            goal: filtered_goal.clone(),
            root: self.root.clone(),
            model: self.config.model.clone(),
        };
        let plugin_ctx = PluginContext {
            session_id: session_id.clone(),
            root: self.root.clone(),
            model: self.config.model.clone(),
        };

        if self.profile.enable_session_trace {
            self.journal.record(SessionEvent::SessionStarted {
                session_id,
                goal: filtered_goal,
                profile: self.profile.name.clone(),
                model: self.config.model.clone(),
                timestamp: SessionJournal::now_iso(),
            });
        }
        self.plugins.dispatch_session_start(&plugin_ctx, &meta);

        let state = Arc::new(Mutex::new(RunState::default()));
        let (final_text, turns, stop) = self
            .run_loop(
                goal.to_owned(),
                0,
                state.clone(),
                plugin_ctx.clone(),
                loop_guard.clone(),
            )
            .await;
        let state = std::mem::take(&mut *state.lock().expect("harness state lock"));
        let outcome = HarnessOutcome {
            final_text,
            changes: state.changes.into_values().collect(),
            plan: state.plan,
            turns,
            stop,
        };

        if self.profile.enable_session_trace {
            self.journal.record(SessionEvent::SessionEnded {
                turns: outcome.turns,
                stop_cause: format!("{:?}", outcome.stop),
                total_changes: outcome.changes.len(),
                duration_ms: run_start.elapsed().as_millis() as u64,
            });
        }
        self.plugins.dispatch_session_end(&plugin_ctx, &outcome);

        outcome
    }

    fn emit(&self, event: HarnessEvent) {
        if let Some(callback) = &self.events {
            callback(event);
        }
    }

    fn tool_definitions(&self, depth: u8) -> Vec<ToolDefinition> {
        let mut tools = tools::builtin_definitions(&ToolSet {
            commands: self.config.allow_commands,
            delegate: depth < self.config.max_delegation_depth,
            skills: !self.skills.is_empty(),
        });
        tools.extend(self.external.iter().map(|tool| tool.definition()));
        tools
    }

    fn system_prompt(&self, depth: u8) -> String {
        let memory = self.memory.load();
        let skills_summary = self
            .skills
            .iter()
            .map(|s| (s.name.clone(), s.description.clone()))
            .collect();

        SystemPromptComposer::new()
            .with_section(Box::new(prompt::PersonaSection))
            .with_section(Box::new(prompt::ProfileSection {
                kind: self.profile.kind,
            }))
            .with_section(Box::new(prompt::SubAgentSection { depth }))
            .with_section(Box::new(prompt::MemorySection { memory }))
            .with_section(Box::new(prompt::SkillsSection {
                skills: skills_summary,
            }))
            .render()
    }

    fn run_loop(
        &self,
        goal: String,
        depth: u8,
        state: Arc<Mutex<RunState>>,
        plugin_ctx: PluginContext,
        loop_guard: Arc<Mutex<LoopGuard>>,
    ) -> Pin<Box<dyn Future<Output = (String, usize, StopCause)> + Send + '_>> {
        Box::pin(async move {
            let mut request = ChatRequest::new(self.config.model.clone())
                .with_system(self.system_prompt(depth))
                .with_cached_system_prefix()
                .with_tools(self.tool_definitions(depth))
                .with_max_tokens(self.config.max_output_tokens)
                .with_user(goal);
            request.scope = self.config.scope.clone();
            let mut last_text = String::new();

            for turn in 1..=self.config.max_turns {
                if self.cancel.load(Ordering::SeqCst) {
                    return (last_text, turn - 1, StopCause::Cancelled);
                }
                let turn_start = std::time::Instant::now();
                self.emit(HarnessEvent::TurnStarted { depth, turn });
                if self.profile.enable_session_trace {
                    self.journal.record(SessionEvent::TurnStarted {
                        turn,
                        timestamp: SessionJournal::now_iso(),
                    });
                }
                self.plugins.dispatch_turn_start(&plugin_ctx, turn);

                self.compact(&mut request);

                let decision =
                    match self
                        .plugins
                        .dispatch_pre_model_request(&plugin_ctx, turn, &mut request)
                    {
                        Ok(d) => d,
                        Err(err) => {
                            return (last_text, turn, StopCause::ModelError(err));
                        }
                    };
                match decision {
                    StepDecision::Continue => {}
                    StepDecision::Halt(reason) => {
                        return (last_text, turn, StopCause::ModelError(reason));
                    }
                    StepDecision::InjectNotice(notice) => {
                        request.messages.push(Message::user(notice));
                    }
                }

                if self.profile.enable_session_trace {
                    let prompt_chars: usize = request
                        .messages
                        .iter()
                        .map(|m| m.text_content().len())
                        .sum();
                    self.journal.record(SessionEvent::ModelRequested {
                        turn,
                        model: request.model.clone(),
                        message_count: request.messages.len(),
                        prompt_chars,
                    });
                }

                let req_start = std::time::Instant::now();
                let completion = match self.models.complete(request.clone()).await {
                    Ok(completion) => completion,
                    Err(error) => {
                        return (last_text, turn, StopCause::ModelError(error.to_string()));
                    }
                };
                let req_duration_ms = req_start.elapsed().as_millis() as u64;

                self.plugins
                    .dispatch_model_response(&plugin_ctx, turn, &completion);

                if self.profile.enable_session_trace {
                    self.journal.record(SessionEvent::ModelResponded {
                        turn,
                        text_length: completion.text.len(),
                        tool_calls: completion
                            .tool_calls
                            .iter()
                            .map(|c| c.name.clone())
                            .collect(),
                        stop_reason: completion.stop_reason.map(|s| format!("{s:?}")),
                        duration_ms: req_duration_ms,
                    });
                }

                if !completion.text.trim().is_empty() {
                    last_text = completion.text.clone();
                    self.emit(HarnessEvent::AssistantText {
                        depth,
                        text: completion.text.clone(),
                    });
                }
                if completion.tool_calls.is_empty() {
                    let turn_dur = turn_start.elapsed().as_millis() as u64;
                    self.plugins.dispatch_turn_end(&plugin_ctx, turn);
                    if self.profile.enable_session_trace {
                        self.journal.record(SessionEvent::TurnEnded {
                            turn,
                            duration_ms: turn_dur,
                        });
                    }
                    return (last_text, turn, StopCause::Completed);
                }

                let mut blocks = Vec::new();
                if let Some(reasoning) = &completion.reasoning {
                    if !reasoning.trim().is_empty() {
                        blocks.push(ContentBlock::Reasoning {
                            text: reasoning.clone(),
                        });
                    }
                }
                if !completion.text.trim().is_empty() {
                    blocks.push(ContentBlock::Text {
                        text: completion.text.clone(),
                    });
                }
                let calls: Vec<_> = completion
                    .tool_calls
                    .iter()
                    .enumerate()
                    .map(|(index, call)| {
                        let id = if call.id.trim().is_empty() {
                            format!("call-{depth}-{turn}-{index}")
                        } else {
                            call.id.clone()
                        };
                        (id, call.name.clone(), call.arguments.clone())
                    })
                    .collect();
                for (id, name, arguments) in &calls {
                    blocks.push(ContentBlock::ToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: arguments.clone(),
                    });
                }
                request.messages.push(Message::new(Role::Assistant, blocks));

                for (index, (id, name, arguments)) in calls.into_iter().enumerate() {
                    let tool_start = std::time::Instant::now();
                    let tool_call_obj = ryoko_models::ToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: arguments.clone(),
                    };

                    let pre_decision = self
                        .plugins
                        .dispatch_pre_tool_call(&plugin_ctx, &tool_call_obj)
                        .unwrap_or_else(|err| {
                            ToolDecision::Deny(format!("plugin pre_tool_call error: {err}"))
                        });

                    let result = if self.cancel.load(Ordering::SeqCst) {
                        Err("cancelled by the user".to_owned())
                    } else if index >= self.config.max_calls_per_turn {
                        Err(format!(
                            "skipped: at most {} tool calls per turn",
                            self.config.max_calls_per_turn
                        ))
                    } else {
                        match pre_decision {
                            ToolDecision::Deny(reason) => Err(reason),
                            ToolDecision::Override(output) => Ok(output),
                            ToolDecision::Allow => {
                                let loop_warning = if self.profile.enable_loop_guard {
                                    let rev = if name == "read_file" {
                                        arguments
                                            .get("path")
                                            .and_then(Value::as_str)
                                            .and_then(|p| {
                                                state
                                                    .lock()
                                                    .ok()
                                                    .and_then(|s| s.file_revisions.get(p).copied())
                                            })
                                            .unwrap_or(0)
                                    } else {
                                        0
                                    };
                                    loop_guard.lock().ok().and_then(|mut g| {
                                        g.check_and_record_with_revision(&name, &arguments, rev)
                                    })
                                } else {
                                    None
                                };

                                if let Some(warning) = loop_warning {
                                    if self.profile.enable_session_trace {
                                        self.journal.record(SessionEvent::LoopIntervention {
                                            turn,
                                            tool_name: name.clone(),
                                            repetitions: 3,
                                        });
                                    }
                                    Err(warning)
                                } else {
                                    if self.profile.enable_session_trace {
                                        self.journal.record(SessionEvent::ToolStarted {
                                            turn,
                                            call_id: id.clone(),
                                            name: name.clone(),
                                            args: redact_credentials_val(&arguments),
                                        });
                                    }
                                    self.emit(HarnessEvent::ToolStarted {
                                        depth,
                                        name: name.clone(),
                                    });
                                    let result = self
                                        .call_tool(
                                            &name,
                                            &arguments,
                                            depth,
                                            &state,
                                            &plugin_ctx,
                                            &loop_guard,
                                        )
                                        .await;
                                    self.emit(HarnessEvent::ToolFinished {
                                        depth,
                                        name: name.clone(),
                                        ok: result.is_ok(),
                                    });
                                    if self.profile.enable_session_trace {
                                        let out_len = match &result {
                                            Ok(txt) => txt.len(),
                                            Err(err) => err.len(),
                                        };
                                        self.journal.record(SessionEvent::ToolFinished {
                                            turn,
                                            call_id: id.clone(),
                                            name: name.clone(),
                                            ok: result.is_ok(),
                                            output_chars: out_len,
                                            duration_ms: tool_start.elapsed().as_millis() as u64,
                                        });
                                    }
                                    result
                                }
                            }
                        }
                    };

                    self.plugins
                        .dispatch_post_tool_call(&plugin_ctx, &tool_call_obj, &result);

                    let (content, is_error) = match result {
                        Ok(text) => (text, false),
                        Err(text) => (format!("error: {text}"), true),
                    };
                    request.messages.push(Message::new(
                        Role::Tool,
                        vec![ContentBlock::ToolResult {
                            tool_call_id: id,
                            content: tools::truncate_middle(
                                &content,
                                self.config.tool_output_limit,
                            ),
                            is_error,
                        }],
                    ));
                }

                let turn_dur = turn_start.elapsed().as_millis() as u64;
                self.plugins.dispatch_turn_end(&plugin_ctx, turn);
                if self.profile.enable_session_trace {
                    self.journal.record(SessionEvent::TurnEnded {
                        turn,
                        duration_ms: turn_dur,
                    });
                }
            }
            (last_text, self.config.max_turns, StopCause::TurnLimit)
        })
    }

    /// Replaces the oldest tool results with a placeholder once the
    /// conversation outgrows its budget. The goal and the latest turns stay.
    fn compact(&self, request: &mut ChatRequest) {
        const KEEP_RECENT: usize = 6;
        const PLACEHOLDER: &str = "[older tool output removed to save context]";
        let size = |request: &ChatRequest| -> usize {
            request
                .messages
                .iter()
                .flat_map(|message| &message.content)
                .map(|block| match block {
                    ContentBlock::Text { text } => text.len(),
                    ContentBlock::Reasoning { text } => text.len(),
                    ContentBlock::ToolResult { content, .. } => content.len(),
                    ContentBlock::ToolCall { arguments, .. } => arguments.to_string().len(),
                    ContentBlock::Image { data_base64, .. } => data_base64.len(),
                })
                .sum()
        };
        if size(request) <= self.config.context_char_budget {
            return;
        }
        let end = request.messages.len().saturating_sub(KEEP_RECENT);
        let mut elided = 0;
        for index in 1..end {
            for block in &mut request.messages[index].content {
                if let ContentBlock::ToolResult { content, .. } = block {
                    if content.len() > PLACEHOLDER.len() {
                        *content = PLACEHOLDER.to_owned();
                        elided += 1;
                    }
                }
            }
            if size(request) <= self.config.context_char_budget {
                break;
            }
        }
        if elided > 0 {
            self.emit(HarnessEvent::ContextCompacted {
                results_elided: elided,
            });
            if self.profile.enable_session_trace {
                self.journal.record(SessionEvent::ContextCompacted {
                    turn: 0,
                    elided_count: elided,
                });
            }
        }
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: &Value,
        depth: u8,
        state: &Arc<Mutex<RunState>>,
        plugin_ctx: &PluginContext,
        loop_guard: &Arc<Mutex<LoopGuard>>,
    ) -> Result<String, String> {
        if !arguments.is_object() {
            return Err("tool arguments must be a JSON object".into());
        }
        let text = |key: &str| -> Result<String, String> {
            arguments
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("missing string argument `{key}`"))
        };
        match name {
            "find_context" => {
                let query = text("query")?;
                if query.trim().is_empty() || query.chars().count() > 500 {
                    return Err("query must be 1..500 characters".into());
                }
                let limit = arguments
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(8)
                    .clamp(1, 20) as usize;
                let index = self
                    .fast_context
                    .get_or_init(|| {
                        FastContext::build(&self.root).map_err(|error| error.to_string())
                    })
                    .as_ref()
                    .map_err(Clone::clone)?;
                let hits = index.search(&query, limit);
                if hits.is_empty() {
                    return Ok("no matching code found; try other words or identifiers".into());
                }
                Ok(context::format_hits(&hits))
            }
            "read_file" => {
                let path = tools::validate_relative_path(&text("path")?)?;
                if let Some(change) = state.lock().expect("state").changes.get(&path) {
                    return Ok(change.content.clone());
                }
                self.read_disk(&path)
            }
            "list_files" => {
                let limit = arguments
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(100) as usize;
                match self.reader.execute(ToolRequest::ListFiles { limit }) {
                    Ok(ToolResponse::Files { paths, truncated }) => {
                        let mut out = paths.join("\n");
                        if truncated {
                            out.push_str("\n[list truncated]");
                        }
                        Ok(out)
                    }
                    Ok(_) => Err("unexpected tool response".into()),
                    Err(error) => Err(error.to_string()),
                }
            }
            "search_text" => {
                let needle = text("query")?;
                match self
                    .reader
                    .execute(ToolRequest::SearchText { needle, limit: 100 })
                {
                    Ok(ToolResponse::Matches { matches, truncated }) => {
                        if matches.is_empty() {
                            return Ok("no matches".into());
                        }
                        let mut out: String = matches
                            .iter()
                            .map(|hit| format!("{}:{}: {}\n", hit.path, hit.line, hit.excerpt))
                            .collect();
                        if truncated {
                            out.push_str("[more matches not shown]");
                        }
                        Ok(out)
                    }
                    Ok(_) => Err("unexpected tool response".into()),
                    Err(error) => Err(error.to_string()),
                }
            }
            "write_file" => {
                let path = tools::validate_relative_path(&text("path")?)?;
                self.stage(state, path, text("content")?, loop_guard)
            }
            "edit_file" => {
                let path = tools::validate_relative_path(&text("path")?)?;
                let (old, new) = (text("old")?, text("new")?);
                if old.is_empty() {
                    return Err("`old` must not be empty; use write_file for new files".into());
                }
                let staged = state
                    .lock()
                    .expect("state")
                    .changes
                    .get(&path)
                    .map(|change| change.content.clone());
                let current = match staged {
                    Some(content) => content,
                    None => self.read_disk(&path)?,
                };
                match current.matches(old.as_str()).count() {
                    0 => Err("`old` text was not found; read the file again".into()),
                    1 => self.stage(state, path, current.replacen(&old, &new, 1), loop_guard),
                    n => Err(format!("`old` appears {n} times; include more context")),
                }
            }
            "update_plan" => {
                let items: Vec<PlanItem> =
                    serde_json::from_value(arguments.get("items").cloned().unwrap_or(Value::Null))
                        .map_err(|error| format!("invalid plan: {error}"))?;
                if items.len() > 30 || items.iter().any(|item| item.task.chars().count() > 300) {
                    return Err("plan is too long: at most 30 short steps".into());
                }
                state.lock().expect("state").plan = items.clone();
                self.emit(HarnessEvent::PlanUpdated {
                    items: items.clone(),
                });
                if self.profile.enable_session_trace {
                    self.journal.record(SessionEvent::PlanUpdated { items });
                }
                Ok("plan updated".into())
            }
            "remember" => {
                let note = text("note")?;
                self.memory
                    .append(&note)
                    .map_err(|error| error.to_string())?;
                self.emit(HarnessEvent::MemorySaved { note: note.clone() });
                if self.profile.enable_session_trace {
                    self.journal.record(SessionEvent::MemorySaved { note });
                }
                Ok("saved to project memory".into())
            }
            "load_skill" => {
                let wanted = text("name")?;
                let skill = self
                    .skills
                    .iter()
                    .find(|skill| skill.name == wanted)
                    .ok_or_else(|| format!("no skill named `{wanted}`"))?;
                skill
                    .load()
                    .map(|body| format!("<skill name=\"{}\">\n{body}\n</skill>", skill.name))
                    .map_err(|error| error.to_string())
            }
            "run_command" if self.config.allow_commands => {
                let request = tools::parse_command(arguments)?;
                if !self.approvals.approve_command(&request).await {
                    self.emit(HarnessEvent::CommandDenied {
                        program: request.program.clone(),
                    });
                    return Err("the user did not approve this command".into());
                }
                Ok(tools::run_approved_command(&self.root, &request).await)
            }
            "delegate" if depth < self.config.max_delegation_depth => {
                let task = text("task")?;
                if task.trim().is_empty() || task.chars().count() > 8000 {
                    return Err("task must be 1..8000 characters".into());
                }
                if self.profile.enable_session_trace {
                    self.journal
                        .record(SessionEvent::SubAgentStarted { task: task.clone() });
                }
                self.emit(HarnessEvent::SubAgentStarted { task: task.clone() });
                let (summary, _, stop) = self
                    .run_loop(
                        task,
                        depth + 1,
                        state.clone(),
                        plugin_ctx.clone(),
                        loop_guard.clone(),
                    )
                    .await;
                if self.profile.enable_session_trace {
                    self.journal.record(SessionEvent::SubAgentFinished {
                        summary_chars: summary.len(),
                    });
                }
                self.emit(HarnessEvent::SubAgentFinished {
                    summary: summary.clone(),
                });
                match stop {
                    StopCause::Completed => Ok(summary),
                    other => Err(format!("sub-agent stopped early ({other:?}): {summary}")),
                }
            }
            other => match self
                .external
                .iter()
                .find(|tool| tool.definition().name == other)
            {
                Some(tool) => tool.call(arguments.clone()).await,
                None => Err(format!("unknown or unavailable tool `{other}`")),
            },
        }
    }

    fn read_disk(&self, path: &str) -> Result<String, String> {
        match self.reader.execute(ToolRequest::ReadFile {
            path: path.to_owned(),
        }) {
            Ok(ToolResponse::File { content, .. }) => Ok(content),
            Ok(_) => Err("unexpected tool response".into()),
            Err(error) => Err(format!("cannot read `{path}`: {error}")),
        }
    }

    fn stage(
        &self,
        state: &Arc<Mutex<RunState>>,
        path: String,
        content: String,
        loop_guard: &Arc<Mutex<LoopGuard>>,
    ) -> Result<String, String> {
        if content.len() > MAX_STAGED_FILE_BYTES {
            return Err(format!("file is larger than {MAX_STAGED_FILE_BYTES} bytes"));
        }
        let mut guard = state.lock().expect("state");
        if !guard.changes.contains_key(&path) && guard.changes.len() >= MAX_STAGED_FILES {
            return Err(format!(
                "at most {MAX_STAGED_FILES} files can be changed in one run"
            ));
        }
        let original = match guard.changes.get(&path) {
            Some(existing) => existing.original.clone(),
            None => self.read_disk(&path).ok(),
        };
        guard.changes.insert(
            path.clone(),
            ProposedChange {
                path: path.clone(),
                original: original.clone(),
                content: content.clone(),
            },
        );
        *guard.file_revisions.entry(path.clone()).or_insert(0) += 1;
        drop(guard);
        if let Ok(mut g) = loop_guard.lock() {
            g.invalidate_file(&path);
        }
        self.emit(HarnessEvent::ChangeStaged { path: path.clone() });
        if self.profile.enable_session_trace {
            self.journal.record(SessionEvent::ChangeStaged {
                path: path.clone(),
                original_bytes: original.as_ref().map(|s| s.len()),
                staged_bytes: content.len(),
            });
        }
        Ok(format!("staged `{path}` for review"))
    }
}

pub fn redact_credentials_str(text: &str) -> String {
    let mut out = text.to_owned();

    // 1. Redact PEM private keys
    if out.contains("BEGIN") && out.contains("PRIVATE KEY") {
        if let Some(start) = out.find("-----BEGIN") {
            if let Some(end) = out[start..].find("-----END") {
                if let Some(final_dash) = out[start + end..].find("-----") {
                    let end_pos = start + end + final_dash + 5;
                    out.replace_range(start..end_pos, "[REDACTED PRIVATE KEY]");
                }
            }
        }
    }

    // 2. Token / key prefixes
    let prefixes = [
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "sk-",
        "sk_live_",
        "rk_live_",
        "xoxb-",
        "xoxp-",
        "xoxa-",
        "xoxs-",
        "glpat-",
    ];

    for prefix in prefixes {
        let mut idx = 0;
        while let Some(pos) = out[idx..].find(prefix) {
            let start = idx + pos;
            let end = out[start..]
                .find(|ch: char| {
                    ch.is_whitespace()
                        || ch == '"'
                        || ch == '\''
                        || ch == ';'
                        || ch == '&'
                        || ch == '|'
                        || ch == ','
                })
                .map(|len| start + len)
                .unwrap_or(out.len());
            let secret_len = end - start;
            if secret_len >= prefix.len() + 8 {
                out.replace_range(start..end, "[REDACTED TOKEN]");
                idx = start + "[REDACTED TOKEN]".len();
            } else {
                idx = start + prefix.len();
            }
        }
    }

    // 3. AWS keys AKIA... (16 uppercase/digits)
    let mut idx = 0;
    while let Some(pos) = out[idx..].find("AKIA") {
        let start = idx + pos;
        let suffix = &out[start + 4..];
        let count = suffix
            .chars()
            .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            .count();
        if count == 16 {
            let end = start + 20;
            out.replace_range(start..end, "[REDACTED AWS KEY]");
            idx = start + "[REDACTED AWS KEY]".len();
        } else {
            idx = start + 4;
        }
    }

    // 4. Common key=value or key: value forms
    let kv_markers = [
        "password=",
        "password:",
        "api_key=",
        "api_key:",
        "apikey=",
        "apikey:",
        "secret=",
        "secret:",
        "token=",
        "token:",
    ];
    for marker in kv_markers {
        let lower = out.to_ascii_lowercase();
        let mut search_idx = 0;
        while let Some(pos) = lower[search_idx..].find(marker) {
            let start = search_idx + pos + marker.len();
            let remainder = &out[start..];
            let trimmed_start = remainder
                .chars()
                .take_while(|c| c.is_whitespace() || *c == '"' || *c == '\'')
                .count();
            let val_start = start + trimmed_start;
            let val_len = out[val_start..]
                .find(|ch: char| {
                    ch.is_whitespace()
                        || ch == '"'
                        || ch == '\''
                        || ch == ';'
                        || ch == '&'
                        || ch == ','
                        || ch == '\n'
                })
                .unwrap_or(out.len() - val_start);
            if val_len > 0 {
                let end = val_start + val_len;
                out.replace_range(val_start..end, "[REDACTED]");
                break;
            } else {
                search_idx = start;
            }
        }
    }

    // 5. Bearer tokens: Bearer <token>
    let mut idx = 0;
    while let Some(pos) = out[idx..].find("Bearer ") {
        let start = idx + pos + 7;
        let end = out[start..]
            .find(|ch: char| ch.is_whitespace() || ch == '"' || ch == '\'' || ch == '\n')
            .map(|l| start + l)
            .unwrap_or(out.len());
        if end > start + 5 {
            out.replace_range(start..end, "[REDACTED]");
            idx = start + "[REDACTED]".len();
        } else {
            idx = start;
        }
    }

    out
}

pub fn redact_credentials_val(val: &Value) -> Value {
    match val {
        Value::String(s) => Value::String(redact_credentials_str(s)),
        Value::Array(arr) => Value::Array(arr.iter().map(redact_credentials_val).collect()),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                let k_lower = k.to_ascii_lowercase();
                if [
                    "token",
                    "api_key",
                    "apikey",
                    "secret",
                    "password",
                    "authorization",
                    "auth",
                ]
                .iter()
                .any(|sensitive| k_lower == *sensitive)
                {
                    out.insert(k.clone(), Value::String("[REDACTED]".to_owned()));
                } else {
                    out.insert(k.clone(), redact_credentials_val(v));
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use ryoko_models::{MockProvider, ScriptedToolCall, ScriptedTurn};
    use serde_json::json;
    use std::fs;

    fn setup() -> (tempfile::TempDir, Arc<MockProvider>, Arc<ModelRouter>) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn answer() -> u32 { 41 }\n",
        )
        .unwrap();
        let mock = Arc::new(MockProvider::new("mock"));
        let mut router = ModelRouter::new("mock");
        router.register(mock.clone());
        (dir, mock, Arc::new(router))
    }

    fn call(id: &str, name: &str, args: Value) -> ScriptedTurn {
        ScriptedTurn::text("").with_tool_call(ScriptedToolCall::new(id, name, args))
    }

    #[tokio::test]
    async fn stages_edits_without_touching_disk() {
        let (dir, mock, router) = setup();
        mock.push_turn(call("1", "read_file", json!({"path": "src/lib.rs"})));
        mock.push_turn(call(
            "2",
            "edit_file",
            json!({"path": "src/lib.rs", "old": "41", "new": "42"}),
        ));
        mock.push_turn(call(
            "3",
            "write_file",
            json!({"path": "src/new.rs", "content": "// new\n"}),
        ));
        mock.push_turn(ScriptedTurn::text("Fixed the answer."));

        let harness = Harness::new(dir.path(), router, HarnessConfig::new("mock/coder")).unwrap();
        let outcome = harness.run("fix the answer").await;

        assert_eq!(outcome.stop, StopCause::Completed);
        assert_eq!(outcome.final_text, "Fixed the answer.");
        assert_eq!(outcome.turns, 4);
        assert_eq!(outcome.changes.len(), 2);
        let edited = &outcome.changes[0];
        assert_eq!(edited.path, "src/lib.rs");
        assert_eq!(
            edited.original.as_deref(),
            Some("pub fn answer() -> u32 { 41 }\n")
        );
        assert_eq!(edited.content, "pub fn answer() -> u32 { 42 }\n");
        assert_eq!(outcome.changes[1].original, None);
        // Disk is untouched.
        assert!(
            fs::read_to_string(dir.path().join("src/lib.rs"))
                .unwrap()
                .contains("41")
        );
        assert!(!dir.path().join("src/new.rs").exists());
        // The tool result was fed back to the model.
        let requests = mock.requests();
        let fed_back = requests[1].messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(block, ContentBlock::ToolResult { content, .. } if content.contains("41"))
            })
        });
        assert!(fed_back);

        // Verify session journal recorded events
        let events = harness.journal().events();
        assert!(!events.is_empty());
        let trace = harness.journal().inspect();
        assert!(trace.contains("SESSION START"));
        assert!(trace.contains("read_file"));
        assert!(trace.contains("edit_file"));
        assert!(trace.contains("write_file"));
    }

    #[tokio::test]
    async fn bad_tool_calls_become_errors_the_model_can_recover_from() {
        let (dir, mock, router) = setup();
        mock.push_turn(call("1", "read_file", json!({"path": "../secret.rs"})));
        mock.push_turn(call("2", "no_such_tool", json!({})));
        mock.push_turn(call("3", "run_command", json!({"program": "ls"})));
        mock.push_turn(ScriptedTurn::text("done"));
        let harness = Harness::new(dir.path(), router, HarnessConfig::new("mock/coder")).unwrap();
        let outcome = harness.run("poke around").await;
        assert_eq!(outcome.stop, StopCause::Completed);
        let errors: Vec<String> = mock
            .requests()
            .last()
            .unwrap()
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|block| match block {
                ContentBlock::ToolResult {
                    content,
                    is_error: true,
                    ..
                } => Some(content.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(errors.len(), 3);
        // run_command is not offered unless the host enables it.
        assert!(errors[2].contains("unknown or unavailable tool"));
    }

    struct Recorder(Mutex<Vec<CommandRequest>>, bool);

    use crate::tools::CommandRequest;

    #[async_trait]
    impl ApprovalGate for Recorder {
        async fn approve_command(&self, request: &CommandRequest) -> bool {
            self.0.lock().unwrap().push(request.clone());
            self.1
        }
    }

    #[tokio::test]
    async fn runs_approved_commands_and_feeds_output_back() {
        let (dir, mock, router) = setup();
        mock.push_turn(call(
            "1",
            "run_command",
            json!({"program": "echo", "args": ["hello"], "reason": "say hello"}),
        ));
        mock.push_turn(ScriptedTurn::text("Saw the greeting."));

        let mut config = HarnessConfig::new("mock/coder");
        config.allow_commands = true;
        let recorder = Arc::new(Recorder(Mutex::new(Vec::new()), true));
        let harness = Harness::new(dir.path(), router, config)
            .unwrap()
            .with_approvals(recorder.clone());
        let outcome = harness.run("say hello").await;
        assert_eq!(outcome.stop, StopCause::Completed);
        assert_eq!(recorder.0.lock().unwrap().len(), 1);
        let fed_back = mock.requests()[1].messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(block, ContentBlock::ToolResult { content, .. } if content.contains("hello"))
            })
        });
        assert!(fed_back);
    }

    #[tokio::test]
    async fn loop_guard_intervenes_on_repeats() {
        let (dir, mock, router) = setup();
        // Model keeps repeating the exact same failing tool call
        mock.push_turn(call("1", "read_file", json!({"path": "src/missing.rs"})));
        mock.push_turn(call("2", "read_file", json!({"path": "src/missing.rs"})));
        mock.push_turn(call("3", "read_file", json!({"path": "src/missing.rs"})));
        mock.push_turn(ScriptedTurn::text("Recognized the loop and stopped."));

        let harness = Harness::new(dir.path(), router, HarnessConfig::new("mock/coder")).unwrap();
        let outcome = harness.run("find file").await;
        assert_eq!(outcome.stop, StopCause::Completed);

        // 3rd call should contain the loop guard intervention message
        let third_result = mock.requests()[3].messages.iter().find_map(|m| {
            m.content.iter().find_map(|b| match b {
                ContentBlock::ToolResult {
                    tool_call_id,
                    content,
                    ..
                } if tool_call_id == "3" => Some(content.clone()),
                _ => None,
            })
        });
        assert!(third_result.is_some());
        assert!(third_result.unwrap().contains("Loop protection triggered"));
    }

    struct MockPlugin {
        name: String,
        started: Arc<Mutex<bool>>,
    }

    impl HarnessPlugin for MockPlugin {
        fn name(&self) -> &str {
            &self.name
        }

        fn on_session_start(&self, _ctx: &PluginContext, _meta: &SessionMeta) {
            *self.started.lock().unwrap() = true;
        }
    }

    #[tokio::test]
    async fn plugin_lifecycle_hooks_are_dispatched() {
        let (dir, mock, router) = setup();
        mock.push_turn(ScriptedTurn::text("Instant completion"));

        let started = Arc::new(Mutex::new(false));
        let plugin = Arc::new(MockPlugin {
            name: "test-plugin".to_owned(),
            started: started.clone(),
        });

        let harness = Harness::new(dir.path(), router, HarnessConfig::new("mock/coder"))
            .unwrap()
            .with_plugin(plugin);

        let outcome = harness.run("quick goal").await;
        assert_eq!(outcome.stop, StopCause::Completed);
        assert!(*started.lock().unwrap());
    }

    #[tokio::test]
    async fn custom_profile_configures_harness() {
        let (dir, _mock, router) = setup();
        let profile = HarnessProfile::minimal("mock/fast");
        let harness = Harness::new(dir.path(), router, HarnessConfig::new("mock/coder"))
            .unwrap()
            .with_profile(profile);

        assert_eq!(harness.profile().name, "minimal");
        assert_eq!(harness.config.max_turns, 15);
        assert_eq!(harness.config.context_char_budget, 60_000);
    }

    struct FailingPlugin;
    impl HarnessPlugin for FailingPlugin {
        fn name(&self) -> &str {
            "failing"
        }
        fn pre_tool_call(
            &self,
            _ctx: &PluginContext,
            _call: &ryoko_models::ToolCall,
        ) -> Result<ToolDecision, String> {
            Err("plugin failure".into())
        }
    }

    #[tokio::test]
    async fn plugin_dispatch_error_denies_tool_call() {
        let (dir, mock, router) = setup();
        mock.push_turn(call("1", "read_file", json!({"path": "src/lib.rs"})));
        mock.push_turn(ScriptedTurn::text("done"));

        let harness = Harness::new(dir.path(), router, HarnessConfig::new("mock/coder"))
            .unwrap()
            .with_plugin(Arc::new(FailingPlugin));

        let outcome = harness.run("test plugin failure").await;
        assert_eq!(outcome.stop, StopCause::Completed);

        // Tool call should be denied with error message from failing plugin
        let res = mock.requests()[1].messages.iter().find_map(|m| {
            m.content.iter().find_map(|b| match b {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
        });
        assert!(res.is_some());
        assert!(res.unwrap().contains("plugin pre_tool_call error"));
    }

    #[tokio::test]
    async fn intervening_edit_allows_rereading_file() {
        let (dir, mock, router) = setup();
        // 1. read_file
        mock.push_turn(call("1", "read_file", json!({"path": "src/lib.rs"})));
        // 2. edit_file
        mock.push_turn(call(
            "2",
            "edit_file",
            json!({"path": "src/lib.rs", "old": "41", "new": "42"}),
        ));
        // 3. read_file again (revision 1)
        mock.push_turn(call("3", "read_file", json!({"path": "src/lib.rs"})));
        // 4. edit_file again
        mock.push_turn(call(
            "4",
            "edit_file",
            json!({"path": "src/lib.rs", "old": "42", "new": "43"}),
        ));
        // 5. read_file again (revision 2)
        mock.push_turn(call("5", "read_file", json!({"path": "src/lib.rs"})));
        mock.push_turn(ScriptedTurn::text("Verified all edits"));

        let harness = Harness::new(dir.path(), router, HarnessConfig::new("mock/coder")).unwrap();
        let outcome = harness.run("edit and verify").await;
        assert_eq!(outcome.stop, StopCause::Completed);

        // None of the tool calls should have been blocked by loop guard
        for req in mock.requests() {
            for msg in &req.messages {
                for block in &msg.content {
                    if let ContentBlock::ToolResult { content, .. } = block {
                        assert!(
                            !content.contains("Loop protection triggered"),
                            "unexpected loop guard hit: {content}"
                        );
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn journal_redacts_credentials_in_goal_and_tool_args() {
        let (dir, mock, router) = setup();
        mock.push_turn(call(
            "1",
            "read_file",
            json!({"path": "src/lib.rs", "token": "ghp_12345678901234567890"}),
        ));
        mock.push_turn(ScriptedTurn::text("done"));

        let harness = Harness::new(dir.path(), router, HarnessConfig::new("mock/coder")).unwrap();
        let _ = harness
            .run("use api token sk-1234567890abcdef1234567890 to check")
            .await;

        let events = harness.journal().events();
        // Check SessionStarted goal
        let started_event = events.iter().find_map(|e| match e {
            SessionEvent::SessionStarted { goal, .. } => Some(goal),
            _ => None,
        });
        assert!(started_event.is_some());
        assert!(
            !started_event
                .unwrap()
                .contains("sk-1234567890abcdef1234567890")
        );
        assert!(started_event.unwrap().contains("[REDACTED TOKEN]"));

        // Check ToolStarted args
        let tool_event = events.iter().find_map(|e| match e {
            SessionEvent::ToolStarted { args, .. } => Some(args),
            _ => None,
        });
        assert!(tool_event.is_some());
        let args_str = serde_json::to_string(tool_event.unwrap()).unwrap();
        assert!(!args_str.contains("ghp_12345678901234567890"));
        assert!(args_str.contains("[REDACTED]"));
    }
}
