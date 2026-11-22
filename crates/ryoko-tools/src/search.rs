//! Code and symbol search tools.

use async_trait::async_trait;
use ryoko_core::context::{extract_files, FileContext, SourceFile};
use ryoko_core::index::{WorkspaceIndex, SymbolHit, CallerHit};
use ryoko_core::tools::{
    Capability, CapabilitySet, ReadScope, Tool, ToolContext, ToolOutput,
    ToolRegistry, ToolError, ToolResult,
};
use ryoko_core::workspace::WorkspaceLease;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Input for search tools.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum SearchToolInput {
    /// Search for text in files (grep-like).
    Text {
        pattern: String,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        file_pattern: Option<String>,
        #[serde(default = "default_limit")]
        limit: usize,
    },
    /// Search for a symbol by name.
    Symbol {
        name: String,
        #[serde(default = "default_limit")]
        limit: usize,
    },
    /// Find callers of a symbol.
    Callers {
        name: String,
        #[serde(default = "default_limit")]
        limit: usize,
    },
    /// Get file outline (imports, symbols, calls).
    Outline {
        path: String,
    },
    /// Get focused context for a symbol (imports + body).
    SymbolContext {
        path: String,
        symbol: String,
    },
}

fn default_limit() -> usize {
    50
}

/// Output from search tools.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SearchToolOutput {
    TextMatches {
        pattern: String,
        matches: Vec<TextMatch>,
    },
    SymbolHits {
        name: String,
        hits: Vec<SymbolHit>,
    },
    Callers {
        name: String,
        callers: Vec<CallerHit>,
    },
    Outline {
        path: String,
        outline: String,
    },
    SymbolContext {
        path: String,
        symbol: String,
        context: String,
    },
}

/// A text match in a file.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TextMatch {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub text: String,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}

/// Search tool for finding code, symbols, and patterns.
pub struct SearchTool {
    workspace: Option<Arc<dyn WorkspaceLease>>,
    index: Arc<tokio::sync::Mutex<Option<WorkspaceIndex>>>,
}

impl SearchTool {
    /// Create a new search tool.
    pub fn new(workspace: Option<Arc<dyn WorkspaceLease>>) -> Self {
        Self {
            workspace,
            index: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// Get or build the workspace index.
    async fn get_index(&self, context: &ToolContext) -> ToolResult<WorkspaceIndex> {
        let mut index_guard = self.index.lock().await;
        if let Some(index) = index_guard.as_ref() {
            return Ok(WorkspaceIndex::in_memory().unwrap()); // Simplified - would reuse
        }

        // Build index from workspace
        let root = context.workspace_root().unwrap_or_else(|| PathBuf::from("."));
        let files = Self::collect_source_files(&root).await?;

        let mut index = WorkspaceIndex::in_memory()
            .map_err(|e| ToolError::Internal(e.to_string()))?;

        let file_contexts = extract_files(files);
        index.rebuild(&file_contexts)
            .map_err(|e| ToolError::Internal(e.to_string()))?;

        *index_guard = Some(index);
        Ok(WorkspaceIndex::in_memory().unwrap())
    }

    /// Collect source files from the workspace.
    async fn collect_source_files(root: &Path) -> ToolResult<Vec<SourceFile>> {
        let mut files = Vec::new();
        let mut stack = vec![root.to_path_buf()];

        while let Some(dir) = stack.pop() {
            let mut entries = tokio::fs::read_dir(&dir).await
                .map_err(|e| ToolError::ExecutionFailed {
                    tool: "search".to_owned(),
                    message: e.to_string(),
                })?;

            while let Some(entry) = entries.next_entry().await
                .map_err(|e| ToolError::ExecutionFailed {
                    tool: "search".to_owned(),
                    message: e.to_string(),
                })? {
                let path = entry.path();
                if path.is_dir() {
                    // Skip hidden directories and common build dirs
                    let name = entry.file_name().to_string_lossy();
                    if !name.starts_with('.') && !matches!(name.as_ref(), "target" | "node_modules" | "dist" | "build") {
                        stack.push(path);
                    }
                } else if let Some(ext) = path.extension() {
                    let ext = ext.to_string_lossy().to_lowercase();
                    if matches!(ext.as_str(),
                        "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" |
                        "c" | "h" | "cpp" | "cc" | "hpp" | "java" | "kt" |
                        "cs" | "swift" | "sql"
                    ) {
                        let relative = path.strip_prefix(root)
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .replace('\\', "/");
                        files.push((relative, path));
                    }
                }
            }
        }

        Ok(files)
    }

    /// Search for text pattern in files.
    async fn search_text(
        &self,
        context: &ToolContext,
        pattern: &str,
        path: Option<&str>,
        file_pattern: Option<&str>,
        limit: usize,
    ) -> ToolResult<SearchToolOutput> {
        let regex = regex::Regex::new(pattern)
            .map_err(|e| ToolError::InvalidArguments {
                tool: "search".to_owned(),
                message: format!("invalid regex: {}", e),
            })?;

        let root = context.workspace_root().unwrap_or_else(|| PathBuf::from("."));
        let search_root = if let Some(p) = path {
            root.join(p)
        } else {
            root
        };

        let files = Self::collect_source_files(&search_root).await?;
        let mut matches = Vec::new();

        for (rel_path, abs_path) in files {
            // Check file pattern
            if let Some(fp) = file_pattern {
                let glob = glob::Pattern::new(fp).ok();
                if let Some(g) = glob {
                    if !g.matches(&rel_path) {
                        continue;
                    }
                }
            }

            let content = tokio::fs::read_to_string(&abs_path).await
                .map_err(|e| ToolError::ExecutionFailed {
                    tool: "search".to_owned(),
                    message: e.to_string(),
                })?;

            for (line_num, line) in content.lines().enumerate() {
                if regex.is_match(line) {
                    let mat = regex.find(line).unwrap();
                    let column = mat.start();

                    // Get context lines
                    let lines: Vec<&str> = content.lines().collect();
                    let context_before = lines[..line_num].iter().rev().take(2).cloned().collect::<Vec<_>>().into_iter().rev().collect();
                    let context_after = lines[line_num + 1..].iter().take(2).cloned().collect();

                    matches.push(TextMatch {
                        file: rel_path,
                        line: line_num + 1,
                        column,
                        text: line.to_owned(),
                        context_before,
                        context_after,
                    });

                    if matches.len() >= limit {
                        break;
                    }
                }

                if matches.len() >= limit {
                    break;
                }
            }
        }

        Ok(SearchToolOutput::TextMatches {
            pattern: pattern.to_owned(),
            matches,
        })
    }

    /// Search for a symbol.
    async fn search_symbol(
        &self,
        _context: &ToolContext,
        name: &str,
        limit: usize,
    ) -> ToolResult<SearchToolOutput> {
        // This would use the workspace index
        // For now, return empty
        Ok(SearchToolOutput::SymbolHits {
            name: name.to_owned(),
            hits: Vec::new(),
        })
    }

    /// Find callers of a symbol.
    async fn find_callers(
        &self,
        _context: &ToolContext,
        name: &str,
        limit: usize,
    ) -> ToolResult<SearchToolOutput> {
        Ok(SearchToolOutput::Callers {
            name: name.to_owned(),
            callers: Vec::new(),
        })
    }

    /// Get file outline.
    async fn get_outline(&self, context: &ToolContext, path: &str) -> ToolResult<SearchToolOutput> {
        let full_path = context.workspace_root()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(path);

        let content = tokio::fs::read_to_string(&full_path).await
            .map_err(|e| ToolError::ExecutionFailed {
                tool: "search".to_owned(),
                message: e.to_string(),
            })?;

        let files = vec![(path.to_owned(), full_path)];
        let file_contexts = extract_files(files);

        if let Some(ctx) = file_contexts.first() {
            Ok(SearchToolOutput::Outline {
                path: path.to_owned(),
                outline: ctx.render_outline(),
            })
        } else {
            Err(ToolError::ExecutionFailed {
                tool: "search".to_owned(),
                message: "could not parse file".to_owned(),
            })
        }
    }

    /// Get symbol context.
    async fn get_symbol_context(&self, context: &ToolContext, path: &str, symbol: &str) -> ToolResult<SearchToolOutput> {
        let full_path = context.workspace_root()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(path);

        let content = tokio::fs::read_to_string(&full_path).await
            .map_err(|e| ToolError::ExecutionFailed {
                tool: "search".to_owned(),
                message: e.to_string(),
            })?;

        let files = vec![(path.to_owned(), full_path)];
        let mut extractor = ryoko_core::context::ContextExtractor::new()
            .map_err(|e| ToolError::Internal(e.to_string()))?;

        let file_context = extractor.extract_symbols(path, &content, &[symbol])
            .map_err(|e| ToolError::Internal(e.to_string()))?;

        let context_text = file_context.snippets.iter()
            .map(|s| s.text.clone())
            .collect::<Vec<_>>()
            .join("\n\n");

        Ok(SearchToolOutput::SymbolContext {
            path: path.to_owned(),
            symbol: symbol.to_owned(),
            context: context_text,
        })
    }
}

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &str {
        "search"
    }

    fn description(&self) -> &str {
        "Search for code patterns, symbols, and get file outlines"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "oneOf": [
                {
                    "properties": {
                        "operation": { "const": "text" },
                        "pattern": { "type": "string" },
                        "path": { "type": "string" },
                        "file_pattern": { "type": "string" },
                        "limit": { "type": "integer", "default": 50, "minimum": 1, "maximum": 200 }
                    },
                    "required": ["operation", "pattern"]
                },
                {
                    "properties": {
                        "operation": { "const": "symbol" },
                        "name": { "type": "string" },
                        "limit": { "type": "integer", "default": 50, "minimum": 1, "maximum": 200 }
                    },
                    "required": ["operation", "name"]
                },
                {
                    "properties": {
                        "operation": { "const": "callers" },
                        "name": { "type": "string" },
                        "limit": { "type": "integer", "default": 50, "minimum": 1, "maximum": 200 }
                    },
                    "required": ["operation", "name"]
                },
                {
                    "properties": {
                        "operation": { "const": "outline" },
                        "path": { "type": "string" }
                    },
                    "required": ["operation", "path"]
                },
                {
                    "properties": {
                        "operation": { "const": "symbol_context" },
                        "path": { "type": "string" },
                        "symbol": { "type": "string" }
                    },
                    "required": ["operation", "path", "symbol"]
                }
            ]
        })
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::from([
            Capability::Read(ReadScope::Workspace),
        ])
    }

    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(&self, context: &ToolContext, input: serde_json::Value) -> ToolResult<ToolOutput> {
        let operation: SearchToolInput = serde_json::from_value(input)
            .map_err(|e| ToolError::InvalidArguments {
                tool: "search".to_owned(),
                message: e.to_string(),
            })?;

        let output = match operation {
            SearchToolInput::Text { pattern, path, file_pattern, limit } => {
                self.search_text(context, &pattern, path.as_deref(), file_pattern.as_deref(), limit).await?
            }
            SearchToolInput::Symbol { name, limit } => {
                self.search_symbol(context, &name, limit).await?
            }
            SearchToolInput::Callers { name, limit } => {
                self.find_callers(context, &name, limit).await?
            }
            SearchToolInput::Outline { path } => {
                self.get_outline(context, &path).await?
            }
            SearchToolInput::SymbolContext { path, symbol } => {
                self.get_symbol_context(context, &path, &symbol).await?
            }
        };

        Ok(ToolOutput::data(serde_json::to_value(output)?))
    }
}

/// Register search tools in the registry.
pub fn register_search_tools(registry: &mut ToolRegistry, workspace: Option<Arc<dyn WorkspaceLease>>) {
    registry.register(Box::new(SearchTool::new(workspace)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryoko_core::model::{AgentRole, Task};
    use ryoko_models::ModelRouter;
    use tempfile::TempDir;

    #[tokio::test]
    async fn search_text_basic() {
        let dir = TempDir::new().unwrap();
        tokio::fs::write(dir.path().join("test.rs"), "fn hello() {\n    println!(\"world\");\n}").await.unwrap();

        let workspace = ryoko_core::workspace::InMemoryWorkspaceProvider::new("main");
        let lease = workspace.allocate(&Task::new("test", AgentRole::SeniorDeveloper), &Default::default()).await.unwrap();

        let tool = SearchTool::new(Some(lease));
        let context = ToolContext::new(
            Task::new("test", AgentRole::SeniorDeveloper),
            None,
            Arc::new(ModelRouter::new("mock")),
            Arc::new(ryoko_core::AllowAllValidator),
            Arc::new(ryoko_core::SharedMemory::new()),
            CapabilitySet::read_only(),
        );

        let input = serde_json::json!({
            "operation": "text",
            "pattern": "hello",
            "limit": 10
        });
        let result = tool.execute(&context, input).await.unwrap();
        let matches = result.data["matches"].as_array().unwrap();
        assert!(!matches.is_empty());
        assert!(matches[0]["text"].as_str().unwrap().contains("hello"));
    }

    #[tokio::test]
    async fn search_outline() {
        let dir = TempDir::new().unwrap();
        tokio::fs::write(dir.path().join("test.rs"), "use std::collections::HashMap;\n\npub struct Engine {\n    tasks: HashMap<String, Task>,\n}\n\nimpl Engine {\n    pub fn new() -> Self {\n        Self { tasks: HashMap::new() }\n    }\n}").await.unwrap();

        let workspace = ryoko_core::workspace::InMemoryWorkspaceProvider::new("main");
        let lease = workspace.allocate(&Task::new("test", AgentRole::SeniorDeveloper), &Default::default()).await.unwrap();

        let tool = SearchTool::new(Some(lease));
        let context = ToolContext::new(
            Task::new("test", AgentRole::SeniorDeveloper),
            None,
            Arc::new(ModelRouter::new("mock")),
            Arc::new(ryoko_core::AllowAllValidator),
            Arc::new(ryoko_core::SharedMemory::new()),
            CapabilitySet::read_only(),
        );

        let input = serde_json::json!({
            "operation": "outline",
            "path": "test.rs"
        });
        let result = tool.execute(&context, input).await.unwrap();
        let outline = result.data["outline"].as_str().unwrap();
        assert!(outline.contains("Engine"));
        assert!(outline.contains("new"));
        assert!(outline.contains("HashMap"));
    }
}