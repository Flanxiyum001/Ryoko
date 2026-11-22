//! Repository and connection management for persistent storage.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use rusqlite::{Connection, OpenFlags, params};
use thiserror::Error;
use tracing::{debug, info};

use crate::{DatabaseSchema, SCHEMA_VERSION, StorageError};

/// Repository managing a SQLite database for durable state.
#[derive(Debug)]
pub struct Repository {
    state_dir: std::path::PathBuf,
    db_name: String,
    connection: Arc<Mutex<Option<Connection>>>,
}

impl Repository {
    /// Opens a repository at the given state directory with the given database name.
    pub fn open_at(state_dir: &Path, db_name: &str) -> Result<Self, StorageError> {
        let repo = Self {
            state_dir: state_dir.to_path_buf(),
            db_name: db_name.to_string(),
            connection: Arc::new(Mutex::new(None)),
        };

        // Initialize the connection and schema
        repo.ensure_connection()?;

        Ok(repo)
    }

    /// Gets a connection, creating it if necessary.
    fn ensure_connection(&self) -> Result<(), StorageError> {
        let mut guard = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            let db_path = self.state_dir.join(format!("{}.sqlite3", self.db_name));
            debug!("Opening database at: {:?}", db_path);

            let connection = Connection::open_with_flags(
                &db_path,
                OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_CREATE
                    | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            ).map_err(StorageError::Sqlite)?;

            // Enable WAL mode for better concurrency
            connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
                .map_err(StorageError::Sqlite)?;

            // Apply schema
            DatabaseSchema::apply_task_schema(&connection)?;
            DatabaseSchema::apply_event_journal_schema(&connection)?;

            // Run migrations
            let current_version = self.get_schema_version(&connection)?;
            DatabaseSchema::migrate(&connection, &current_version)?;

            *guard = Some(connection);
            info!("Repository initialized at {:?}", db_path);
        }
        Ok(())
    }

    /// Gets the current schema version from the database.
    fn get_schema_version(&self, conn: &Connection) -> Result<String, StorageError> {
        let version: Option<String> = conn.query_row(
            "SELECT version FROM schema_version ORDER BY applied_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        ).ok();

        Ok(version.unwrap_or_else(|| "0.0.0".to_string()))
    }

    /// Gets a connection for read operations.
    pub fn read_connection(&self) -> Result<RepositoryConnection, StorageError> {
        self.ensure_connection()?;
        let conn = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        Ok(RepositoryConnection {
            connection: Arc::new(conn.clone().unwrap()),
        })
    }

    /// Gets a connection for write operations.
    pub fn write_connection(&self) -> Result<RepositoryConnection, StorageError> {
        self.ensure_connection()?;
        let conn = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        Ok(RepositoryConnection {
            connection: Arc::new(conn.clone().unwrap()),
        })
    }

    /// Executes a read-only operation with a connection.
    pub fn with_read<T>(&self, f: impl FnOnce(&Connection) -> Result<T, StorageError>) -> Result<T, StorageError> {
        let conn = self.read_connection()?;
        f(&conn.connection)
    }

    /// Executes a write operation with a transaction.
    pub fn with_write<T>(&self, f: impl FnOnce(&Connection) -> Result<T, StorageError>) -> Result<T, StorageError> {
        let conn = self.write_connection()?;
        let result = f(&conn.connection);
        result
    }

    /// Executes a transaction with automatic rollback on error.
    pub fn with_transaction<T>(&self, f: impl FnOnce(&rusqlite::Transaction) -> Result<T, StorageError>) -> Result<T, StorageError> {
        let conn = self.write_connection()?;
        let tx = conn.connection.transaction().map_err(StorageError::Sqlite)?;
        let result = f(&tx);
        match result {
            Ok(value) => {
                tx.commit().map_err(StorageError::Sqlite)?;
                Ok(value)
            }
            Err(e) => {
                let _ = tx.rollback();
                Err(e)
            }
        }
    }
}

/// Wrapper around a database connection for the repository.
#[derive(Debug, Clone)]
pub struct RepositoryConnection {
    connection: Arc<Connection>,
}

impl RepositoryConnection {
    /// Gets the underlying connection.
    pub fn connection(&self) -> &Connection {
        &self.connection
    }
}