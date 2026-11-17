//! End-to-end tests: a task that passes validation, a task whose validation
//! never passes, and a task that fails before validation are all reported to an
//! LSP client over the same byte-stream framing `ryoko-lsp` uses on stdio.
//!
//! The server side is `ryoko_native::lsp_service` driven by `tower_lsp::Server`
//! on a duplex stream pair, so the assertions are made against real
//! `Content-Length` framed JSON-RPC, not against an internal function.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ryoko_core::agent::{AgentRuntime, TaskContext};
use ryoko_core::model::{
    AgentIdentity, AgentRole, ProjectContext, ProposedChange, Task, TaskResult,
};
use ryoko_core::{
    AllowAllValidator, ChangeValidator, CoreResult, InMemoryWorkspaceProvider, Orchestrator,
    OrchestratorConfig, StagedChanges, ValidationReport,
};
use ryoko_models::ModelRouter;
use ryoko_native::{DiagnosticPublisher, Url, diagnostic_channel, lsp_service};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::task::JoinHandle;
use tower_lsp::Server;

struct ValidatedAgent {
    identity: AgentIdentity,
}

#[async_trait]
impl AgentRuntime for ValidatedAgent {
    fn identity(&self) -> AgentIdentity {
        self.identity.clone()
    }

    async fn execute_task(
        &self,
        _ctx: TaskContext,
        task: Task,
    ) -> ryoko_core::CoreResult<TaskResult> {
        Ok(
            TaskResult::success(task.id, "generated the module").with_changes(vec![
                ProposedChange::new("src/generated.rs", "pub fn generated() {}\n"),
            ]),
        )
    }
}

struct FailingAgent {
    identity: AgentIdentity,
}

#[async_trait]
impl AgentRuntime for FailingAgent {
    fn identity(&self) -> AgentIdentity {
        self.identity.clone()
    }

    async fn execute_task(
        &self,
        _ctx: TaskContext,
        task: Task,
    ) -> ryoko_core::CoreResult<TaskResult> {
        Ok(
            TaskResult::failure(task.id, "model stopped before writing a module").with_changes(
                vec![ProposedChange::new(
                    "src/generated.rs",
                    "pub fn generated() {}\n",
                )],
            ),
        )
    }
}

struct RejectingValidator;

#[async_trait]
impl ChangeValidator for RejectingValidator {
    async fn validate(&self, _changes: &StagedChanges) -> CoreResult<ValidationReport> {
        Ok(ValidationReport::failed(
            "cargo test failed",
            vec!["assertion failed".to_owned()],
            Duration::from_millis(1),
        ))
    }
}

async fn send(writer: &mut DuplexStream, message: Value) {
    let body = message.to_string();
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n{}", body.len(), body).as_bytes())
        .await
        .expect("write LSP frame");
    writer.flush().await.expect("flush LSP frame");
}

async fn receive(reader: &mut BufReader<DuplexStream>) -> Value {
    let frame = tokio::time::timeout(Duration::from_secs(10), async {
        let mut content_length = None;
        loop {
            let mut line = String::new();
            let read = reader.read_line(&mut line).await.expect("read LSP header");
            assert!(read > 0, "language server closed the stream");
            let header = line.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some(value) = header.strip_prefix("Content-Length:") {
                content_length = Some(value.trim().parse::<usize>().expect("content length"));
            }
        }
        let length = content_length.expect("frame had no Content-Length");
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body).await.expect("read LSP body");
        serde_json::from_slice::<Value>(&body).expect("decode LSP body")
    })
    .await;
    frame.expect("timed out waiting for a language server message")
}

/// Drains messages until one satisfies `accept`.
async fn receive_until(
    reader: &mut BufReader<DuplexStream>,
    accept: impl Fn(&Value) -> bool,
) -> Value {
    loop {
        let message = receive(reader).await;
        if accept(&message) {
            return message;
        }
    }
}

struct Session {
    publisher: DiagnosticPublisher,
    client_write: DuplexStream,
    client: BufReader<DuplexStream>,
    server: JoinHandle<()>,
}

/// Starts a language server on a duplex stream pair and completes the
/// `initialize` / `initialized` handshake. The publisher is the same instance
/// the server consumes from, ready to be wired into a host as a sink.
async fn start_session() -> Session {
    let (publisher, inbox) = diagnostic_channel();
    let (_retained, service, socket) = lsp_service(publisher.clone(), inbox);

    let (mut client_write, server_read) = tokio::io::duplex(64 * 1024);
    let (server_write, client_read) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        Server::new(BufReader::new(server_read), server_write, socket)
            .serve(service)
            .await;
    });
    let mut client = BufReader::new(client_read);

    send(
        &mut client_write,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "capabilities": {}, "processId": null, "rootUri": null }
        }),
    )
    .await;
    let initialize = receive(&mut client).await;
    assert_eq!(initialize["result"]["serverInfo"]["name"], "ryoko-lsp");

    send(
        &mut client_write,
        json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    )
    .await;

    Session {
        publisher,
        client_write,
        client,
        server,
    }
}

async fn shutdown(
    mut client_write: DuplexStream,
    mut client: BufReader<DuplexStream>,
    server: JoinHandle<()>,
) {
    send(
        &mut client_write,
        json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown" }),
    )
    .await;
    let shutdown = receive(&mut client).await;
    assert_eq!(shutdown["id"], json!(2));
    send(
        &mut client_write,
        json!({ "jsonrpc": "2.0", "method": "exit" }),
    )
    .await;
    drop(client_write);
    let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
}

fn project_context(project: &tempfile::TempDir) -> ProjectContext {
    ProjectContext {
        project_root: project.path().to_string_lossy().into_owned(),
        ..ProjectContext::default()
    }
}

fn diagnostics_for(published: &Value) -> &Vec<Value> {
    published["params"]["diagnostics"]
        .as_array()
        .expect("diagnostics array")
}

fn published_for_uri(message: &Value, uri: &Url) -> bool {
    message["method"] == "textDocument/publishDiagnostics"
        && message["params"]["uri"] == uri.as_str()
        && message["params"]["diagnostics"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
}

#[tokio::test]
async fn validated_task_diagnostic_reaches_lsp_client() {
    let project = tempfile::tempdir().expect("temp project");
    let uri = Url::from_file_path(project.path().join("src/generated.rs")).expect("file uri");

    // One channel instance is shared by the orchestrator (producer) and the
    // language server (consumer), exactly like a host embedding both.
    let Session {
        publisher,
        client_write,
        mut client,
        server,
    } = start_session().await;

    // The host wires the LSP publisher into the orchestrator as a sink; core
    // itself never names the language-server crate.
    let orchestrator = Orchestrator::new(
        Arc::new(InMemoryWorkspaceProvider::new("main")),
        Arc::new(AllowAllValidator),
        Arc::new(ModelRouter::new("mock")),
    )
    .with_diagnostic_sink(Arc::new(publisher));

    let identity = AgentIdentity::new(AgentRole::SeniorDeveloper, "validated-agent", "test agent");
    let task = Task::new("generate a module", AgentRole::SeniorDeveloper);
    let report = orchestrator
        .run(
            vec![task],
            vec![Arc::new(ValidatedAgent { identity })],
            project_context(&project),
        )
        .await
        .expect("orchestrator run");
    assert!(report.all_succeeded(), "task should pass validation");

    let published = receive_until(&mut client, |message| published_for_uri(message, &uri)).await;

    let diagnostics = diagnostics_for(&published);
    assert_eq!(diagnostics.len(), 1, "one finding per changed file");
    let finding = &diagnostics[0];
    assert_eq!(finding["source"], "ryoko");
    assert_eq!(finding["code"], "ryoko.validation");
    assert_eq!(finding["severity"], json!(3), "informational");
    let message = finding["message"].as_str().expect("message text");
    assert!(
        message.contains("validation passed"),
        "unexpected message: {message}"
    );
    assert_eq!(
        finding["range"]["start"],
        json!({ "line": 0, "character": 0 })
    );

    shutdown(client_write, client, server).await;
}

#[tokio::test]
async fn exhausted_validation_reaches_lsp_client_as_an_error() {
    let project = tempfile::tempdir().expect("temp project");
    let uri = Url::from_file_path(project.path().join("src/generated.rs")).expect("file uri");

    let Session {
        publisher,
        client_write,
        mut client,
        server,
    } = start_session().await;

    let orchestrator = Orchestrator::new(
        Arc::new(InMemoryWorkspaceProvider::new("main")),
        Arc::new(RejectingValidator),
        Arc::new(ModelRouter::new("mock")),
    )
    .with_diagnostic_sink(Arc::new(publisher))
    .with_config(OrchestratorConfig {
        max_concurrency: 1,
        max_attempts: 2,
        max_repair_cycles: 1,
    });

    let identity = AgentIdentity::new(AgentRole::SeniorDeveloper, "rejected-agent", "test agent");
    let report = orchestrator
        .run(
            vec![Task::new("generate a module", AgentRole::SeniorDeveloper)],
            vec![Arc::new(ValidatedAgent { identity })],
            project_context(&project),
        )
        .await
        .expect("orchestrator run");
    assert_eq!(report.failed.len(), 1, "every attempt is rejected");
    assert_eq!(report.failed[0].attempts, 2);

    let published = receive_until(&mut client, |message| published_for_uri(message, &uri)).await;

    let diagnostics = diagnostics_for(&published);
    assert_eq!(diagnostics.len(), 1, "one error per rejected file");
    let finding = &diagnostics[0];
    assert_eq!(finding["severity"], json!(1), "LSP error severity");
    assert_eq!(finding["code"], "ryoko.validation.failed");
    assert_eq!(finding["source"], "ryoko");
    let message = finding["message"].as_str().expect("message text");
    assert!(
        message.contains("validation never passed"),
        "unexpected message: {message}"
    );
    assert!(
        message.contains("cargo test failed"),
        "unexpected message: {message}"
    );

    shutdown(client_write, client, server).await;
}

#[tokio::test]
async fn agent_failure_reaches_lsp_client_as_an_error() {
    let project = tempfile::tempdir().expect("temp project");
    let uri = Url::from_file_path(project.path().join("src/generated.rs")).expect("file uri");

    let Session {
        publisher,
        client_write,
        mut client,
        server,
    } = start_session().await;

    let orchestrator = Orchestrator::new(
        Arc::new(InMemoryWorkspaceProvider::new("main")),
        Arc::new(AllowAllValidator),
        Arc::new(ModelRouter::new("mock")),
    )
    .with_diagnostic_sink(Arc::new(publisher))
    .with_config(OrchestratorConfig {
        max_concurrency: 1,
        max_attempts: 2,
        max_repair_cycles: 1,
    });

    let identity = AgentIdentity::new(AgentRole::SeniorDeveloper, "failing-agent", "test agent");
    let report = orchestrator
        .run(
            vec![Task::new("generate a module", AgentRole::SeniorDeveloper)],
            vec![Arc::new(FailingAgent { identity })],
            project_context(&project),
        )
        .await
        .expect("orchestrator run");
    assert_eq!(report.failed.len(), 1, "the agent fails on every attempt");
    assert_eq!(report.failed[0].attempts, 2);

    let published = receive_until(&mut client, |message| published_for_uri(message, &uri)).await;

    let diagnostics = diagnostics_for(&published);
    assert_eq!(diagnostics.len(), 1, "one error for the attempted file");
    let finding = &diagnostics[0];
    assert_eq!(finding["severity"], json!(1), "LSP error severity");
    assert_eq!(finding["code"], "ryoko.agent.failed");
    assert_eq!(finding["source"], "ryoko");
    let message = finding["message"].as_str().expect("message text");
    assert!(
        message.contains("the agent failed"),
        "unexpected message: {message}"
    );
    assert!(
        message.contains("model stopped before writing a module"),
        "unexpected message: {message}"
    );

    shutdown(client_write, client, server).await;
}
