//! Incremental symbol and call graph index.
//!
//! This module provides persistent SQLite-backed indexing of source file symbols,
//! imports, and call edges. It supports:
//! - Incremental updates when files change
//! - Content-hash-based detection of missed events
//! - Filesystem watching for real-time change detection
//! - Cross-file symbol lookup and call relationship discovery
//!
//! The index lives at `.ryoko/state/symbols.sqlite3` alongside worktree state.

mod hash;
mod watcher;

pub mod index;
pub mod error;

pub use error::IndexError;
pub use index::{SymbolHit, CallerHit, SymbolIndex, IndexedSymbol, CallEdge, FileContextSource, goal_terms};
pub use hash::ContentHash;
pub use watcher::{FileWatcher, WatchEvent};

/// Default maximum signature length stored in the index.
pub const MAX_SIGNATURE_CHARS: usize = 500;

/// Default maximum bytes per file to parse for indexing.
pub const MAX_FILE_BYTES: u64 = 1_000_000;