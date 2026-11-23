//! GitHub account integration for the Ryoko workbench.
//!
//! Scope: turning a user's GitHub account into the three capabilities the IDE
//! uses directly — an authenticated account session, access to that account's
//! repositories, and a fetch/pull/push sync for the open checkout. Copilot
//! request shapes live in `ryoko-models`, which owns provider payloads.
//!
//! No token is ever logged, returned to the frontend or written into a
//! workspace file. The long-lived credential lives in the OS credential store
//! (or comes from the process environment when an operator supplies one) and
//! only a redacted status leaves this crate.

mod client;
mod transport;

pub mod auth;
pub mod store;
pub mod sync;

pub use auth::{DeviceFlow, DeviceFlowStart, PollOutcome};
pub use client::{GitHubClient, GitHubRepository, GitHubUser};
pub use store::{
    CredentialSource, CredentialStore, GitCredentialStore, MemoryStore, StoredCredential,
};
pub use sync::{SyncAction, SyncOutcome, SyncStatus};
pub use transport::{HttpMethod, HttpReply, HttpRequest, ReqwestTransport, Transport};

use std::fmt;

/// A credential that must never reach a log line, an error, a serialized IPC
/// payload or a `Debug` rendering.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GitHubError {
    #[error("GitHub sign-in is not configured: {0}")]
    NotConfigured(&'static str),
    #[error("could not reach GitHub")]
    Transport(&'static str),
    #[error("GitHub returned HTTP {status} while {context}")]
    Http { status: u16, context: &'static str },
    #[error("GitHub denied this request; sign in again")]
    Unauthorized,
    #[error("this GitHub account is not allowed to do that")]
    Forbidden,
    #[error("GitHub response was not usable: {0}")]
    InvalidResponse(&'static str),
    #[error("OS credential store error: {0}")]
    CredentialStore(String),
    #[error("{0}")]
    Git(String),
    #[error("refused for safety: {0}")]
    Unsafe(&'static str),
}

impl GitHubError {
    /// Whether the caller should ask the user to sign in again.
    pub fn needs_sign_in(&self) -> bool {
        matches!(self, Self::Unauthorized)
    }
}
