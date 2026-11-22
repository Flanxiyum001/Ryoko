//! File system tools for reading, writing, and managing files in the workspace.

use async_trait::async_trait;
use ryoko_core::model::ProposedChange;
use ryoko_core::tools::{
    Capability, CapabilitySet, ReadScope, WriteScope, Tool, ToolContext, ToolOutput,
    ToolRegistry, ToolError, ToolResult,
};
use ryoko_core::workspace::validate_relative_path;
use ryoko_worktree::WorkspaceLease;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;

/// Input for file system tools.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum FileToolInput {
    /// Read a file.
    Read {
        path: String,
    },
    /// Write a file (create or overwrite).
    Write {
        path: String,
        content: String,
    },
    /// Create a new file (fails if exists).
    Create {
        path: String,
        content: String,
    },
    /// Delete a file.
    Delete {
        path: String,
    },
    /// List files in a directory.
    List {
        path: String,
        #[serde(default)]
        recursive: bool,
    },
    /// Check if a path exists.
    Exists {
        path: String,
    },
    /// Get file metadata.
    Metadata {
        path: String,
    },
}

/// Output from file system tools.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileToolOutput {
    FileContent {
        path: String,
        content: String,
        size: usize,
    },
    FileWritten {
        path: String,
        bytes: usize,
    },
    FileCreated {
        path: String,
        bytes: usize,
    },
    FileDeleted {
        path: String,
    },
    FileList {
        path: String,
        files: Vec<FileEntry>,
    },
    FileExists {
        path: String,
        exists: bool,
    },
    FileMetadata {
        path: String,
        size: u64,
        is_file: bool,
        is_dir: bool,
        modified: Option<String>,
    },
}

/// File entry in a directory listing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_file: bool,
    pub is_dir: bool,
    pub size: u64,
}

/// File system tool that operates within an isolated workspace.
pub struct FileTool {
    workspace: Option<Arc<dyn WorkspaceLease>>,
}

impl FileTool {
    /// Create a new file tool.
    pub fn new(workspace: Option<Arc<dyn WorkspaceLease>>) -> Self {
        Self { workspace }
    }

    /// Resolve a path relative to the workspace root.
    fn resolve_path(&self, path: &str) -> ToolResult<PathBuf> {
        let relative = validate_relative_path(path)
            .map_err(|e| ToolError::InvalidArguments {
                tool: "fs".to_owned(),
                message: e.to_string(),
            })?;

        let root = self.workspace.as_ref()
            .map(|w| w.root().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));

        let full_path = root.join(&relative);

        // Ensure the path stays within the workspace
        let canonical_root = root.canonicalize()
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let canonical_path = full_path.canonicalize()
            .map_err(|_| ToolError::PathTraversal { path: relative.into() })?;

        if !canonical_path.starts_with(&canonical_root) {
            return Err(ToolError::PathTraversal { path: relative.into() });
        }

        // Check for symlinks
        if let Ok(metadata) = fs::symlink_metadata(&full_path).await {
            if metadata.file_type().is_symlink() {
                return Err(ToolError::SymlinkEscape { path: relative.into() });
            }
        }

        Ok(full_path)
    }

    /// Read a file.
    async fn read(&self, path: &str) -> ToolResult<FileToolOutput> {
        let full_path = self.resolve_path(path)?;
        let content = fs::read_to_string(&full_path).await
            .map_err(|e| ToolError::ExecutionFailed {
                tool: "fs.read".to_owned(),
                message: e.to_string(),
            })?;
        let size = content.len();
        Ok(FileToolOutput::FileContent {
            path: path.to_owned(),
            content,
            size,
        })
    }

    /// Write a file (create or overwrite).
    async fn write(&self, path: &str, content: &str) -> ToolResult<FileToolOutput> {
        let full_path = self.resolve_path(path)?;

        // Create parent directories if needed
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).await
                .map_err(|e| ToolError::ExecutionFailed {
                    tool: "fs.write".to_owned(),
                    message: e.to_string(),
                })?;
        }

        fs::write(&full_path, content).await
            .map_err(|e| ToolError::ExecutionFailed {
                tool: "fs.write".to_owned(),
                message: e.to_string(),
            })?;

        Ok(FileToolOutput::FileWritten {
            path: path.to_owned(),
            bytes: content.len(),
        })
    }

    /// Create a new file (fail if exists).
    async fn create(&self, path: &str, content: &str) -> ToolResult<FileToolOutput> {
        let full_path = self.resolve_path(path)?;

        if full_path.exists() {
            return Err(ToolError::ExecutionFailed {
                tool: "fs.create".to_owned(),
                message: format!("file already exists: {}", path),
            });
        }

        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).await
                .map_err(|e| ToolError::ExecutionFailed {
                    tool: "fs.create".to_owned(),
                    message: e.to_string(),
                })?;
        }

        fs::write(&full_path, content).await
            .map_err(|e| ToolError::ExecutionFailed {
                tool: "fs.create".to_owned(),
                message: e.to_string(),
            })?;

        Ok(FileToolOutput::FileCreated {
            path: path.to_owned(),
            bytes: content.len(),
        })
    }

    /// Delete a file.
    async fn delete(&self, path: &str) -> ToolResult<FileToolOutput> {
        let full_path = self.resolve_path(path)?;

        if !full_path.exists() {
            return Err(ToolError::ExecutionFailed {
                tool: "fs.delete".to_owned(),
                message: format!("file not found: {}", path),
            });
        }

        fs::remove_file(&full_path).await
            .map_err(|e| ToolError::ExecutionFailed {
                tool: "fs.delete".to_owned(),
                message: e.to_string(),
            })?;

        Ok(FileToolOutput::FileDeleted {
            path: path.to_owned(),
        })
    }

    /// List files in a directory.
    async fn list(&self, path: &str, recursive: bool) -> ToolResult<FileToolOutput> {
        let full_path = self.resolve_path(path)?;

        if !full_path.exists() {
            return Err(ToolError::ExecutionFailed {
                tool: "fs.list".to_owned(),
                message: format!("path not found: {}", path),
            });
        }

        let mut files = Vec::new();

        if recursive {
            let mut stack = vec![full_path.clone()];
            while let Some(current) = stack.pop() {
                let mut entries = fs::read_dir(&current).await
                    .map_err(|e| ToolError::ExecutionFailed {
                        tool: "fs.list".to_owned(),
                        message: e.to_string(),
                    })?;

                while let Some(entry) = entries.next_entry().await
                    .map_err(|e| ToolError::ExecutionFailed {
                        tool: "fs.list".to_owned(),
                        message: e.to_string(),
                    })? {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();
                    let relative = path.strip_prefix(self.workspace.as_ref().map(|w| w.root()).unwrap_or(&PathBuf::from(".")))
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .to_string();

                    let metadata = entry.metadata().await
                        .map_err(|e| ToolError::ExecutionFailed {
                            tool: "fs.list".to_owned(),
                            message: e.to_string(),
                        })?;

                    files.push(FileEntry {
                        name,
                        path: relative,
                        is_file: metadata.is_file(),
                        is_dir: metadata.is_dir(),
                        size: metadata.len(),
                    });

                    if metadata.is_dir() {
                        stack.push(path);
                    }
                }
            }
        } else {
            let mut entries = fs::read_dir(&full_path).await
                .map_err(|e| ToolError::ExecutionFailed {
                    tool: "fs.list".to_owned(),
                    message: e.to_string(),
                })?;

            while let Some(entry) = entries.next_entry().await
                .map_err(|e| ToolError::ExecutionFailed {
                    tool: "fs.list".to_owned(),
                    message: e.to_string(),
                })? {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                let relative = path.strip_prefix(self.workspace.as_ref().map(|w| w.root()).unwrap_or(&PathBuf::from(".")))
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();

                let metadata = entry.metadata().await
                    .map_err(|e| ToolError::ExecutionFailed {
                        tool: "fs.list".to_owned(),
                        message: e.to_string(),
                    })?;

                files.push(FileEntry {
                    name,
                    path: relative,
                    is_file: metadata.is_file(),
                    is_dir: metadata.is_dir(),
                    size: metadata.len(),
                });
            }
        }

        Ok(FileToolOutput::FileList {
            path: path.to_owned(),
            files,
        })
    }

    /// Check if a path exists.
    async fn exists(&self, path: &str) -> ToolResult<FileToolOutput> {
        let full_path = self.resolve_path(path)?;
        Ok(FileToolOutput::FileExists {
            path: path.to_owned(),
            exists: full_path.exists(),
        })
    }

    /// Get file metadata.
    async fn metadata(&self, path: &str) -> ToolResult<FileToolOutput> {
        let full_path = self.resolve_path(path)?;

        let metadata = fs::metadata(&full_path).await
            .map_err(|e| ToolError::ExecutionFailed {
                tool: "fs.metadata".to_owned(),
                message: e.to_string(),
            })?;

        Ok(FileToolOutput::FileMetadata {
            path: path.to_owned(),
            size: metadata.len(),
            is_file: metadata.is_file(),
            is_dir: metadata.is_dir(),
            modified: metadata.modified().ok().map(|t| {
                humantime::format_rfc3339(t).to_string()
            }),
        })
    }
}

#[async_trait]
impl Tool for FileTool {
    fn name(&self) -> &str {
        "fs"
    }

    fn description(&self) -> &str {
        "File system operations: read, write, create, delete, list files in the workspace"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "oneOf": [
                {
                    "properties": {
                        "operation": { "const": "read" },
                        "path": { "type": "string" }
                    },
                    "required": ["operation", "path"]
                },
                {
                    "properties": {
                        "operation": { "const": "write" },
                        "path": { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["operation", "path", "content"]
                },
                {
                    "properties": {
                        "operation": { "const": "create" },
                        "path": { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["operation", "path", "content"]
                },
                {
                    "properties": {
                        "operation": { "const": "delete" },
                        "path": { "type": "string" }
                    },
                    "required": ["operation", "path"]
                },
                {
                    "properties": {
                        "operation": { "const": "list" },
                        "path": { "type": "string" },
                        "recursive": { "type": "boolean", "default": false }
                    },
                    "required": ["operation", "path"]
                },
                {
                    "properties": {
                        "operation": { "const": "exists" },
                        "path": { "type": "string" }
                    },
                    "required": ["operation", "path"]
                },
                {
                    "properties": {
                        "operation": { "const": "metadata" },
                        "path": { "type": "string" }
                    },
                    "required": ["operation", "path"]
                }
            ]
        })
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::from([
            Capability::Read(ReadScope::Workspace),
            Capability::Write(WriteScope::Full),
        ])
    }

    fn is_mutating(&self) -> bool {
        true // Some operations mutate
    }

    async fn execute(&self, _context: &ToolContext, input: serde_json::Value) -> ToolResult<ToolOutput> {
        let operation: FileToolInput = serde_json::from_value(input)
            .map_err(|e| ToolError::InvalidArguments {
                tool: "fs".to_owned(),
                message: e.to_string(),
            })?;

        let output = match operation {
            FileToolInput::Read { path } => self.read(&path).await?,
            FileToolInput::Write { path, content } => self.write(&path, &content).await?,
            FileToolInput::Create { path, content } => self.create(&path, &content).await?,
            FileToolInput::Delete { path } => self.delete(&path).await?,
            FileToolInput::List { path, recursive } => self.list(&path, recursive).await?,
            FileToolInput::Exists { path } => self.exists(&path).await?,
            FileToolInput::Metadata { path } => self.metadata(&path).await?,
        };

        Ok(ToolOutput::data(serde_json::to_value(output)?))
    }
}

/// Register file tools in the registry.
pub fn register_file_tools(registry: &mut ToolRegistry, workspace: Option<Arc<dyn WorkspaceLease>>) {
    registry.register(Box::new(FileTool::new(workspace)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryoko_core::model::{AgentRole, Task};
    use ryoko_core::tools::{ToolContext, CapabilitySet};
    use ryoko_models::ModelRouter;
    use tempfile::TempDir;

    #[tokio::test]
    async fn file_tool_read_write() {
        let dir = TempDir::new().unwrap();
        let workspace = ryoko_core::workspace::InMemoryWorkspaceProvider::new("main");
        let lease = workspace.allocate(&Task::new("test", AgentRole::SeniorDeveloper), &Default::default()).await.unwrap();

        let tool = FileTool::new(Some(lease));
        let context = ToolContext::new(
            Task::new("test", AgentRole::SeniorDeveloper),
            None,
            Arc::new(ModelRouter::new("mock")),
            Arc::new(ryoko_core::AllowAllValidator),
            Arc::new(ryoko_core::SharedMemory::new()),
            CapabilitySet::all(),
        );

        // Write a file
        let input = serde_json::json!({
            "operation": "write",
            "path": "test.txt",
            "content": "Hello, World!"
        });
        let result = tool.execute(&context, input).await.unwrap();
        assert!(result.data["type"] == "file_written");

        // Read it back
        let input = serde_json::json!({
            "operation": "read",
            "path": "test.txt"
        });
        let result = tool.execute(&context, input).await.unwrap();
        assert_eq!(result.data["content"], "Hello, World!");
    }

    #[tokio::test]
    async fn file_tool_rejects_path_traversal() {
        let tool = FileTool::new(None);
        let context = ToolContext::new(
            Task::new("test", AgentRole::SeniorDeveloper),
            None,
            Arc::new(ModelRouter::new("mock")),
            Arc::new(ryoko_core::AllowAllValidator),
            Arc::new(ryoko_core::SharedMemory::new()),
            CapabilitySet::all(),
        );

        let input = serde_json::json!({
            "operation": "read",
            "path": "../etc/passwd"
        });
        let result = tool.execute(&context, input).await;
        assert!(matches!(result, Err(ToolError::PathTraversal { .. })));
    }

    #[tokio::test]
    async fn file_tool_list_recursive() {
        let dir = TempDir::new().unwrap();
        let workspace = ryoko_core::workspace::InMemoryWorkspaceProvider::new("main");
        let lease = workspace.allocate(&Task::new("test", AgentRole::SeniorDeveloper), &Default::default()).await.unwrap();

        // Create test files
        fs::create_dir_all(lease.root().join("src")).await.unwrap();
        fs::write(lease.root().join("src/main.rs"), "fn main() {}").await.unwrap();
        fs::write(lease.root().join("README.md"), "# Test").await.unwrap();

        let tool = FileTool::new(Some(lease));
        let context = ToolContext::new(
            Task::new("test", AgentRole::SeniorDeveloper),
            None,
            Arc::new(ModelRouter::new("mock")),
            Arc::new(ryoko_core::AllowAllValidator),
            Arc::new(ryoko_core::SharedMemory::new()),
            CapabilitySet::all(),
        );

        let input = serde_json::json!({
            "operation": "list",
            "path": ".",
            "recursive": true
        });
        let result = tool.execute(&context, input).await.unwrap();
        let files = result.data["files"].as_array().unwrap();
        assert!(files.iter().any(|f| f["path"] == "src/main.rs"));
        assert!(files.iter().any(|f| f["path"] == "README.md"));
    }
}