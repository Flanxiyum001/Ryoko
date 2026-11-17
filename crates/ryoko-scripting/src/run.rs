//! Agent adapter for a WASI agent plugin.
//!
//! The plugin can only return complete files. Those files still travel through
//! the normal orchestrator workspace, path validation, review and sandbox
//! validation path; a plugin never gains direct filesystem access.

use std::sync::Arc;

use async_trait::async_trait;
use ryoko_core::{
    AgentIdentity, AgentRole, AgentRuntime, CoreResult, ProjectContext, Task, TaskContext,
    TaskResult,
};

use crate::manifest::{PluginError, PluginKind, WasiPlugin};
use crate::types::{PluginOutput, PluginRequest};

pub struct PluginAgent {
    plugin: Arc<WasiPlugin>,
    identity: AgentIdentity,
}

impl PluginAgent {
    pub fn new(
        plugin: Arc<WasiPlugin>,
        role: AgentRole,
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Result<Self, PluginError> {
        if plugin.manifest().kind != PluginKind::Agent {
            return Err(PluginError::Protocol(
                "only an agent plugin can back an AgentRuntime".into(),
            ));
        }
        Ok(Self {
            plugin,
            identity: AgentIdentity::new(role, name, description).with_capability("wasi-plugin"),
        })
    }

    pub fn identity_value(&self) -> AgentIdentity {
        self.identity.clone()
    }
}

#[async_trait]
impl AgentRuntime for PluginAgent {
    fn identity(&self) -> AgentIdentity {
        self.identity.clone()
    }

    async fn execute_task(&self, ctx: TaskContext, task: Task) -> CoreResult<TaskResult> {
        // Shared memory can contain arbitrary project text; it is not part of
        // the plugin boundary. Only the root, branch and bounded task cross it.
        let project = ProjectContext {
            project_root: ctx.project.project_root.chars().take(4096).collect(),
            active_branch: ctx.project.active_branch.chars().take(256).collect(),
            shared_memory: Default::default(),
        };
        let request = PluginRequest::agent(task.clone(), project, self.identity.role.label());
        let response = match self.plugin.invoke(&request).await {
            Ok(response) => response,
            Err(error) => {
                return Ok(TaskResult::failure(
                    task.id,
                    format!("WASI agent plugin failed: {error}"),
                ));
            }
        };
        match response.result {
            PluginOutput::Agent { summary, files } => {
                let summary = if summary.trim().is_empty() {
                    "WASI agent plugin proposed changes".to_owned()
                } else {
                    summary
                };
                Ok(TaskResult::success(task.id, summary).with_changes(files))
            }
            _ => Ok(TaskResult::failure(
                task.id,
                "WASI agent plugin returned a non-agent result",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::tests::fixture;
    use crate::manifest::{PluginHost, PluginLimits, PluginPolicy};
    use crate::types::PLUGIN_SCHEMA;
    use ryoko_core::{AllowAllValidator, EventSink, SharedMemory};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn agent_fixture(output: &str) -> (TempDir, PathBuf, PathBuf, PluginPolicy) {
        let fixture = fixture(PluginKind::Agent, output, PluginLimits::default());
        (fixture.dir, fixture.manifest, fixture.wasm, fixture.policy)
    }

    #[tokio::test]
    async fn plugin_agent_returns_validated_changes() {
        let output = format!(
            r##"{{"schema":{PLUGIN_SCHEMA},"result":{{"type":"agent","summary":"add module","files":[{{"path":"src/plugin_generated.rs","content":"pub fn generated() {{}}"}},{{"path":"tests/plugin_generated.rs","content":"#[test]\nfn generated() {{ assert!(true); }}"}}]}}}}"##
        );
        let (_dir, manifest, wasm, policy) = agent_fixture(&output);
        let host = PluginHost::new(PluginLimits::default()).unwrap();
        let plugin = host.load(&manifest, &wasm, &policy).await.unwrap();
        let agent = PluginAgent::new(
            Arc::new(plugin),
            AgentRole::Custom("wasi-agent".into()),
            "wasi-agent",
            "test plugin",
        )
        .unwrap();
        let ctx = TaskContext {
            project: ProjectContext {
                project_root: "/tmp/project".into(),
                active_branch: "main".into(),
                shared_memory: Default::default(),
            },
            memory: SharedMemory::new(),
            models: Arc::new(ryoko_models::ModelRouter::new("mock")),
            validator: Arc::new(AllowAllValidator),
            workspace: None,
            mcp: None,
            events: EventSink::default(),
        };
        let task = Task::new(
            "add generated module",
            AgentRole::Custom("wasi-agent".into()),
        );
        let result = agent.execute_task(ctx, task).await.unwrap();
        assert!(result.success, "{:?}", result.error);
        assert_eq!(result.changes.len(), 2);
        assert_eq!(result.changes[0].path, "src/plugin_generated.rs");
    }

    #[tokio::test]
    async fn plugin_agent_rejects_unsafe_paths_before_returning() {
        let output = format!(
            r#"{{"schema":{PLUGIN_SCHEMA},"result":{{"type":"agent","summary":"bad","files":[{{"path":"../escape.rs","content":"x"}}]}}}}"#
        );
        let (_dir, manifest, wasm, policy) = agent_fixture(&output);
        let host = PluginHost::new(PluginLimits::default()).unwrap();
        let plugin = host.load(&manifest, &wasm, &policy).await.unwrap();
        let agent = PluginAgent::new(
            Arc::new(plugin),
            AgentRole::Custom("wasi-agent".into()),
            "wasi-agent",
            "test plugin",
        )
        .unwrap();
        let ctx = TaskContext {
            project: ProjectContext::default(),
            memory: SharedMemory::new(),
            models: Arc::new(ryoko_models::ModelRouter::new("mock")),
            validator: Arc::new(AllowAllValidator),
            workspace: None,
            mcp: None,
            events: EventSink::default(),
        };
        let task = Task::new("bad", AgentRole::Custom("wasi-agent".into()));
        let result = agent.execute_task(ctx, task).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("traversal"));
    }

    #[test]
    fn non_agent_plugin_cannot_become_an_agent() {
        let output = r#"{"schema":1,"result":{"type":"lint","diagnostics":[]}}"#;
        let fixture = fixture(PluginKind::Linter, output, PluginLimits::default());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = PluginHost::new(PluginLimits::default()).unwrap();
        let plugin = runtime
            .block_on(host.load(&fixture.manifest, &fixture.wasm, &fixture.policy))
            .unwrap();
        assert!(
            PluginAgent::new(
                Arc::new(plugin),
                AgentRole::Custom("wasi".into()),
                "wasi",
                "test"
            )
            .is_err()
        );
    }
}
