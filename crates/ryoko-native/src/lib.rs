//! Native editor integration for Ryoko.
//!
//! The server speaks standard LSP over stdio. Agent workers publish findings
//! through [`DiagnosticPublisher`]; the server forwards them to the editor and
//! also exposes code lenses, quick fixes and a small completion entry.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ryoko_vfs::{DocumentChange, DocumentStore, Position as VfsPosition, Range as VfsRange};
use serde_json::json;
use tokio::sync::mpsc;
use tower_lsp::jsonrpc::{Error, Result};
use tower_lsp::lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, CodeActionResponse, CodeLens, CodeLensParams, Command,
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, ExecuteCommandOptions, ExecuteCommandParams, InitializeParams,
    InitializeResult, InitializedParams, MessageType, Position, Range, ServerCapabilities,
    ServerInfo, TextDocumentSyncCapability, TextDocumentSyncKind, WorkDoneProgressOptions,
};
use tower_lsp::{Client, ClientSocket, LanguageServer, LspService, Server};

pub use tower_lsp::lsp_types::Url;

/// A finding produced by a Ryoko agent and addressed to an open editor document.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentDiagnostic {
    pub range: Range,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub code: Option<String>,
    pub source: Option<String>,
    pub fix_title: Option<String>,
}

impl AgentDiagnostic {
    pub fn error(range: Range, message: impl Into<String>) -> Self {
        Self {
            range,
            severity: DiagnosticSeverity::ERROR,
            message: message.into(),
            code: None,
            source: Some("ryoko".to_owned()),
            fix_title: None,
        }
    }

    pub fn warning(range: Range, message: impl Into<String>) -> Self {
        Self {
            range,
            severity: DiagnosticSeverity::WARNING,
            message: message.into(),
            code: None,
            source: Some("ryoko".to_owned()),
            fix_title: None,
        }
    }

    pub fn info(range: Range, message: impl Into<String>) -> Self {
        Self {
            range,
            severity: DiagnosticSeverity::INFORMATION,
            message: message.into(),
            code: None,
            source: Some("ryoko".to_owned()),
            fix_title: None,
        }
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    pub fn with_fix(mut self, title: impl Into<String>) -> Self {
        self.fix_title = Some(title.into());
        self
    }

    fn to_lsp(&self) -> Diagnostic {
        Diagnostic {
            range: self.range,
            severity: Some(self.severity),
            code: self
                .code
                .clone()
                .map(tower_lsp::lsp_types::NumberOrString::String),
            code_description: None,
            source: self.source.clone(),
            message: self.message.clone(),
            related_information: None,
            tags: None,
            data: None,
        }
    }
}

#[derive(Clone)]
struct SharedFindings {
    by_uri: Arc<Mutex<HashMap<String, Vec<Diagnostic>>>>,
}

/// A cloneable channel for agent workers to push diagnostics into the editor.
#[derive(Clone)]
pub struct DiagnosticPublisher {
    findings: SharedFindings,
    tx: mpsc::UnboundedSender<PublishRequest>,
}

#[derive(Debug)]
pub struct PublishRequest {
    pub uri: Url,
    pub version: Option<i32>,
    pub diagnostics: Vec<Diagnostic>,
}

impl DiagnosticPublisher {
    /// Replaces the diagnostics for `uri` and forwards them to every connected
    /// editor. Returns an error only when the language server is no longer
    /// accepting messages.
    pub fn publish(
        &self,
        uri: Url,
        version: Option<i32>,
        findings: Vec<AgentDiagnostic>,
    ) -> std::result::Result<(), mpsc::error::SendError<PublishRequest>> {
        let diagnostics: Vec<Diagnostic> = findings.iter().map(AgentDiagnostic::to_lsp).collect();
        self.findings
            .by_uri
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(uri.as_str().to_owned(), diagnostics.clone());
        self.tx.send(PublishRequest {
            uri,
            version,
            diagnostics,
        })
    }

    pub fn clear(
        &self,
        uri: Url,
    ) -> std::result::Result<(), mpsc::error::SendError<PublishRequest>> {
        self.publish(uri, None, Vec::new())
    }

    /// Publishes one position-less finding for a whole document. The orchestrator
    /// uses this after a task passes validation; the message starts at 0:0 and
    /// leaves any language-server diagnostics for the same file untouched.
    pub fn publish_finding(
        &self,
        uri: Url,
        level: FindingLevel,
        message: impl Into<String>,
        code: Option<&str>,
    ) -> std::result::Result<(), mpsc::error::SendError<PublishRequest>> {
        let message = message.into();
        let mut finding = AgentDiagnostic {
            range: Range::new(Position::new(0, 0), Position::new(0, 0)),
            severity: level.to_lsp(),
            message,
            code: None,
            source: Some("ryoko".to_owned()),
            fix_title: None,
        };
        if let Some(code) = code {
            finding = finding.with_code(code);
        }
        self.publish(uri, None, vec![finding])
    }
}

/// Severity for a task-level finding published without an editor position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingLevel {
    Info,
    Warning,
    Error,
}

impl FindingLevel {
    fn to_lsp(self) -> DiagnosticSeverity {
        match self {
            Self::Info => DiagnosticSeverity::INFORMATION,
            Self::Warning => DiagnosticSeverity::WARNING,
            Self::Error => DiagnosticSeverity::ERROR,
        }
    }
}

/// Creates the channel a multi-agent backend uses to publish editor findings.
pub fn diagnostic_channel() -> (DiagnosticPublisher, mpsc::UnboundedReceiver<PublishRequest>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (
        DiagnosticPublisher {
            findings: SharedFindings {
                by_uri: Arc::new(Mutex::new(HashMap::new())),
            },
            tx,
        },
        rx,
    )
}

pub struct Backend {
    client: Client,
    documents: Arc<DocumentStore>,
    findings: SharedFindings,
    inbox: Mutex<Option<mpsc::UnboundedReceiver<PublishRequest>>>,
}

impl Backend {
    pub fn new(
        client: Client,
        publisher: DiagnosticPublisher,
        inbox: mpsc::UnboundedReceiver<PublishRequest>,
    ) -> Self {
        Self {
            client,
            documents: Arc::new(DocumentStore::new()),
            findings: publisher.findings,
            inbox: Mutex::new(Some(inbox)),
        }
    }

    async fn publish_saved(&self, uri: &Url, version: Option<i32>) {
        let diagnostics = self
            .findings
            .by_uri
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(uri.as_str())
            .cloned()
            .unwrap_or_default();
        self.client
            .publish_diagnostics(uri.clone(), diagnostics, version)
            .await;
    }

    async fn forward_agent_findings(&self) {
        let receiver = self
            .inbox
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        let Some(mut receiver) = receiver else {
            return;
        };
        let client = self.client.clone();
        tokio::spawn(async move {
            while let Some(request) = receiver.recv().await {
                client
                    .publish_diagnostics(request.uri, request.diagnostics, request.version)
                    .await;
            }
        });
    }
}

fn to_vfs_range(range: Range) -> VfsRange {
    VfsRange::new(
        VfsPosition::new(range.start.line, range.start.character),
        VfsPosition::new(range.end.line, range.end.character),
    )
}

fn document_change(change: tower_lsp::lsp_types::TextDocumentContentChangeEvent) -> DocumentChange {
    match change.range {
        Some(range) => DocumentChange::incremental(to_vfs_range(range), change.text),
        None => DocumentChange::full(change.text),
    }
}

fn build_code_lenses(uri: &Url, findings: &[Diagnostic]) -> Vec<CodeLens> {
    let mut lenses = vec![CodeLens {
        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
        command: Some(Command {
            title: "Run Ryoko review".to_owned(),
            command: "ryoko.review".to_owned(),
            arguments: Some(vec![json!(uri)]),
        }),
        data: None,
    }];

    for finding in findings {
        let title = finding
            .data
            .as_ref()
            .and_then(|data| data.get("fix_title"))
            .and_then(|title| title.as_str())
            .unwrap_or("Ask Ryoko to fix this");
        lenses.push(CodeLens {
            range: finding.range,
            command: Some(Command {
                title: title.to_owned(),
                command: "ryoko.fix".to_owned(),
                arguments: Some(vec![
                    json!(uri),
                    json!(finding.range),
                    json!(finding.message),
                ]),
            }),
            data: None,
        });
    }
    lenses
}

fn build_code_actions(uri: &Url, params: &CodeActionParams) -> Vec<CodeActionOrCommand> {
    let mut actions = vec![CodeActionOrCommand::CodeAction(CodeAction {
        title: "Ask Ryoko to fix this".to_owned(),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(params.context.diagnostics.clone()),
        edit: None,
        command: Some(Command {
            title: "Ask Ryoko to fix this".to_owned(),
            command: "ryoko.fix".to_owned(),
            arguments: Some(vec![json!(uri), json!(params.range)]),
        }),
        is_preferred: Some(true),
        disabled: None,
        data: None,
    })];
    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
        title: "Explain this with Ryoko".to_owned(),
        kind: None,
        diagnostics: Some(params.context.diagnostics.clone()),
        edit: None,
        command: Some(Command {
            title: "Explain this with Ryoko".to_owned(),
            command: "ryoko.explain".to_owned(),
            arguments: Some(vec![json!(uri), json!(params.range)]),
        }),
        is_preferred: Some(false),
        disabled: None,
        data: None,
    }));
    actions
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _params: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                code_lens_provider: Some(tower_lsp::lsp_types::CodeLensOptions {
                    resolve_provider: Some(false),
                }),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions::default()),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![
                        "ryoko.fix".to_owned(),
                        "ryoko.explain".to_owned(),
                        "ryoko.review".to_owned(),
                    ],
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                }),
                ..ServerCapabilities::default()
            },
            server_info: Some(ServerInfo {
                name: "ryoko-lsp".to_owned(),
                version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            }),
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        self.forward_agent_findings().await;
        self.client
            .log_message(MessageType::INFO, "Ryoko language server ready")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let document = params.text_document;
        self.documents.open(
            document.uri.as_str(),
            document.language_id,
            document.version,
            document.text,
        );
        self.publish_saved(&document.uri, Some(document.version))
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let changes: Vec<DocumentChange> = params
            .content_changes
            .into_iter()
            .map(document_change)
            .collect();
        match self
            .documents
            .change(uri.as_str(), params.text_document.version, &changes)
        {
            Ok(document) => {
                self.publish_saved(&uri, Some(document.version)).await;
            }
            Err(error) => {
                self.client
                    .log_message(MessageType::ERROR, format!("Ryoko document error: {error}"))
                    .await;
            }
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.documents.close(uri.as_str());
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn code_lens(&self, params: CodeLensParams) -> Result<Option<Vec<CodeLens>>> {
        let uri = params.text_document.uri;
        let findings = self
            .findings
            .by_uri
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(uri.as_str())
            .cloned()
            .unwrap_or_default();
        Ok(Some(build_code_lenses(&uri, &findings)))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri.clone();
        Ok(Some(build_code_actions(&uri, &params)))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let item = CompletionItem {
            label: "Ryoko: explain selection".to_owned(),
            kind: Some(CompletionItemKind::TEXT),
            detail: Some("Ask the Ryoko agent about the current file".to_owned()),
            command: Some(Command {
                title: "Explain with Ryoko".to_owned(),
                command: "ryoko.explain".to_owned(),
                arguments: Some(vec![json!(uri)]),
            }),
            ..CompletionItem::default()
        };
        Ok(Some(CompletionResponse::Array(vec![item])))
    }

    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> Result<Option<serde_json::Value>> {
        if matches!(
            params.command.as_str(),
            "ryoko.fix" | "ryoko.explain" | "ryoko.review"
        ) {
            self.client
                .log_message(
                    MessageType::INFO,
                    format!("Ryoko accepted editor command {}", params.command),
                )
                .await;
            Ok(Some(json!({ "accepted": true, "command": params.command })))
        } else {
            Err(Error::invalid_params(format!(
                "unsupported Ryoko command: {}",
                params.command
            )))
        }
    }
}

/// Wires a diagnostic channel into a fresh LSP service. The returned publisher
/// remains connected for as long as the caller keeps it alive.
pub fn lsp_service(
    publisher: DiagnosticPublisher,
    inbox: mpsc::UnboundedReceiver<PublishRequest>,
) -> (DiagnosticPublisher, LspService<Backend>, ClientSocket) {
    let retained = publisher.clone();
    let (service, socket) =
        LspService::new(move |client| Backend::new(client, publisher.clone(), inbox));
    (retained, service, socket)
}

/// Runs the language server over stdin/stdout.
pub async fn run_stdio() -> std::io::Result<()> {
    let (publisher, inbox) = diagnostic_channel();
    let (_retained, service, socket) = lsp_service(publisher, inbox);
    Server::new(tokio::io::stdin(), tokio::io::stdout(), socket)
        .serve(service)
        .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_diagnostics_convert_to_lsp_and_drive_lenses() {
        let finding = AgentDiagnostic::error(
            Range::new(Position::new(2, 4), Position::new(2, 10)),
            "unused variable",
        )
        .with_code("unused_variable")
        .with_fix("Remove the unused variable");
        let diagnostic = finding.to_lsp();
        assert_eq!(diagnostic.message, "unused variable");
        assert_eq!(diagnostic.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(
            diagnostic.code,
            Some(tower_lsp::lsp_types::NumberOrString::String(
                "unused_variable".to_owned()
            ))
        );

        let uri = Url::parse("file:///demo/src/lib.rs").unwrap();
        let lenses = build_code_lenses(&uri, &[diagnostic]);
        assert_eq!(lenses.len(), 2);
        assert_eq!(lenses[0].command.as_ref().unwrap().command, "ryoko.review");
        assert_eq!(lenses[1].command.as_ref().unwrap().command, "ryoko.fix");
    }

    #[test]
    fn code_actions_offer_fix_and_explanation() {
        let uri = Url::parse("file:///demo/src/lib.rs").unwrap();
        let params = CodeActionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            range: Range::new(Position::new(1, 0), Position::new(1, 4)),
            context: tower_lsp::lsp_types::CodeActionContext {
                diagnostics: Vec::new(),
                only: None,
                trigger_kind: None,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let actions = build_code_actions(&uri, &params);
        assert_eq!(actions.len(), 2);
        let commands: Vec<&str> = actions
            .iter()
            .filter_map(|action| match action {
                CodeActionOrCommand::CodeAction(action) => action
                    .command
                    .as_ref()
                    .map(|command| command.command.as_str()),
                CodeActionOrCommand::Command(_) => None,
            })
            .collect();
        assert_eq!(commands, vec!["ryoko.fix", "ryoko.explain"]);
    }
}
