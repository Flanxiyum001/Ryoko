//! End-to-end test: a task that passes validation is published to an LSP
//! client over the same byte-stream framing `ryoko-lsp` uses on stdio.
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
use ryoko_core::{AllowAllValidator, InMemoryWorkspaceProvider, Orchestrator};
use ryoko_models::ModelRouter;
use ryoko_native::{Url, diagnostic_channel, lsp_service};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream};
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

#[tokio::test]
async fn validated_task_diagnostic_reaches_lsp_client() {
    let project = tempfile::tempdir().expect("temp project");
    let uri = Url::from_file_path(project.path().join("src/generated.rs")).expect("file uri");

    // One channel instance is shared by the orchestrator (producer) and the
    // language server (consumer), exactly like a host embedding both.
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

    let orchestrator = Orchestrator::new(
        Arc::new(InMemoryWorkspaceProvider::new("main")),
        Arc::new(AllowAllValidator),
        Arc::new(ModelRouter::new("mock")),
    )
    .with_diagnostic_publisher(publisher.clone());

    let identity = AgentIdentity::new(AgentRole::SeniorDeveloper, "validated-agent", "test agent");
    let task = Task::new("generate a module", AgentRole::SeniorDeveloper);
    let report = orchestrator
        .run(
            vec![task],
            vec![Arc::new(ValidatedAgent { identity })],
            ProjectContext {
                project_root: project.path().to_string_lossy().into_owned(),
                ..ProjectContext::default()
            },
        )
        .await
        .expect("orchestrator run");
    assert!(report.all_succeeded(), "task should pass validation");

    let published = receive_until(&mut client, |message| {
        message["method"] == "textDocument/publishDiagnostics"
            && message["params"]["uri"] == uri.as_str()
            && message["params"]["diagnostics"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
    })
    .await;

    let diagnostics = published["params"]["diagnostics"]
        .as_array()
        .expect("diagnostics array");
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
