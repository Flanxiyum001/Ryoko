//! Capability-based permission system for tool execution.
//!
//! This module defines the capability model that governs what tools can do.
//! Capabilities are granted by the operator (human) and cannot be self-granted
//! by agents through prompts, tool arguments, or generated files.

use std::collections::BTreeSet;
use serde::{Deserialize, Serialize};

/// A single capability that a tool may require.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Read files from the workspace (includes listing, searching, reading).
    Read(ReadScope),
    /// Write files to the workspace (includes create, edit, delete).
    Write(WriteScope),
    /// Execute commands in the sandboxed environment.
    Execute,
    /// Access network resources (external APIs, web requests).
    Network(NetworkScope),
    /// Access MCP tools from connected servers.
    Mcp,
    /// Read Git history, status, and diffs.
    GitRead,
    /// Write Git operations (commit, branch, push).
    GitWrite,
    /// Run tests, builds, and linters in the sandbox.
    TestExecute,
}

/// Scope for read operations.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadScope {
    /// Read only files explicitly listed in the task context.
    TaskFiles,
    /// Read any file in the workspace (respecting .gitignore).
    Workspace,
    /// Read files outside the workspace (e.g., system configs).
    System,
}

/// Scope for write operations.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteScope {
    /// Create new files only (no modification or deletion).
    CreateOnly,
    /// Modify existing files (create, edit).
    Modify,
    /// Delete files.
    Delete,
    /// Full write access (create, edit, delete).
    Full,
}

/// Scope for network operations.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkScope {
    /// Access only loopback/localhost endpoints.
    Loopback,
    /// Access internal/private networks (RFC 1918).
    Internal,
    /// Access any external network.
    External,
}

/// A set of capabilities with efficient membership testing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySet {
    capabilities: BTreeSet<Capability>,
}

impl CapabilitySet {
    /// Create an empty capability set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a capability set from a list of capabilities.
    pub fn from(capabilities: impl IntoIterator<Item = Capability>) -> Self {
        Self {
            capabilities: capabilities.into_iter().collect(),
        }
    }

    /// Create a capability set with all capabilities.
    pub fn all() -> Self {
        Self {
            capabilities: [
                Capability::Read(ReadScope::Workspace),
                Capability::Write(WriteScope::Full),
                Capability::Execute,
                Capability::Network(NetworkScope::External),
                Capability::Mcp,
                Capability::GitRead,
                Capability::GitWrite,
                Capability::TestExecute,
            ]
            .into_iter()
            .collect(),
        }
    }

    /// Create a minimal read-only capability set.
    pub fn read_only() -> Self {
        Self {
            capabilities: [
                Capability::Read(ReadScope::TaskFiles),
                Capability::GitRead,
            ]
            .into_iter()
            .collect(),
        }
    }

    /// Create a capability set for code generation tasks.
    pub fn code_generation() -> Self {
        Self {
            capabilities: [
                Capability::Read(ReadScope::Workspace),
                Capability::Write(WriteScope::Modify),
                Capability::Execute,
                Capability::GitRead,
                Capability::TestExecute,
            ]
            .into_iter()
            .collect(),
        }
    }

    /// Check if this set contains a capability.
    pub fn contains(&self, capability: &Capability) -> bool {
        self.capabilities.contains(capability)
    }

    /// Check if this set contains all required capabilities.
    pub fn contains_all(&self, required: &CapabilitySet) -> bool {
        required.capabilities.is_subset(&self.capabilities)
    }

    /// Add a capability.
    pub fn insert(&mut self, capability: Capability) {
        self.capabilities.insert(capability);
    }

    /// Remove a capability.
    pub fn remove(&mut self, capability: &Capability) -> bool {
        self.capabilities.remove(capability)
    }

    /// Get all capabilities as a slice.
    pub fn as_slice(&self) -> &[Capability] {
        // This is a bit awkward since BTreeSet doesn't have as_slice,
        // but we can collect into a Vec for iteration
        // For now, return empty and use iter() instead
        &[]
    }

    /// Get an iterator over capabilities.
    pub fn iter(&self) -> impl Iterator<Item = &Capability> {
        self.capabilities.iter()
    }

    /// Number of capabilities in the set.
    pub fn len(&self) -> usize {
        self.capabilities.len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.capabilities.is_empty()
    }
}

impl From<Vec<Capability>> for CapabilitySet {
    fn from(capabilities: Vec<Capability>) -> Self {
        Self::from(capabilities)
    }
}

impl std::ops::BitOr for CapabilitySet {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self {
            capabilities: self.capabilities | other.capabilities,
        }
    }
}

impl std::ops::BitAnd for CapabilitySet {
    type Output = Self;

    fn bitand(self, other: Self) -> Self {
        Self {
            capabilities: self.capabilities & other.capabilities,
        }
    }
}

impl std::ops::Sub for CapabilitySet {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self {
            capabilities: self.capabilities - other.capabilities,
        }
    }
}

/// Permission scope combines capabilities with the approval requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionScope {
    /// The capabilities this scope grants.
    pub capabilities: CapabilitySet,
    /// Whether tools requiring these capabilities need explicit approval per call.
    pub requires_approval: bool,
}

impl PermissionScope {
    /// Create a new permission scope.
    pub fn new(capabilities: CapabilitySet, requires_approval: bool) -> Self {
        Self {
            capabilities,
            requires_approval,
        }
    }

    /// A scope that requires approval for all operations.
    pub fn with_approval(capabilities: CapabilitySet) -> Self {
        Self::new(capabilities, true)
    }

    /// A scope that does not require approval (pre-approved).
    pub fn pre_approved(capabilities: CapabilitySet) -> Self {
        Self::new(capabilities, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_set_operations() {
        let mut set = CapabilitySet::new();
        set.insert(Capability::Read(ReadScope::Workspace));
        set.insert(Capability::Write(WriteScope::Modify));

        assert!(set.contains(&Capability::Read(ReadScope::Workspace)));
        assert!(!set.contains(&Capability::Execute));

        let required = CapabilitySet::from([
            Capability::Read(ReadScope::Workspace),
        ]);
        assert!(set.contains_all(&required));

        let required2 = CapabilitySet::from([
            Capability::Read(ReadScope::Workspace),
            Capability::Execute,
        ]);
        assert!(!set.contains_all(&required2));
    }

    #[test]
    fn preset_capability_sets() {
        let read_only = CapabilitySet::read_only();
        assert!(read_only.contains(&Capability::Read(ReadScope::TaskFiles)));
        assert!(read_only.contains(&Capability::GitRead));
        assert!(!read_only.contains(&Capability::Write(WriteScope::CreateOnly)));

        let code_gen = CapabilitySet::code_generation();
        assert!(code_gen.contains(&Capability::Read(ReadScope::Workspace)));
        assert!(code_gen.contains(&Capability::Write(WriteScope::Modify)));
        assert!(code_gen.contains(&Capability::Execute));
        assert!(code_gen.contains(&Capability::TestExecute));
    }
}