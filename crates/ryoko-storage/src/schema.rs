//! Database schema definitions for durable state persistence.

use rusqlite::{Connection, params};
use std::time::SystemTime;

/// Database schema for the task persistence store.
pub struct DatabaseSchema;

impl DatabaseSchema {
    /// Applies the schema for task persistence.
    pub fn apply_task_schema(conn: &Connection) -> rusqlite::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                description TEXT NOT NULL,
                assigned_to TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                metadata TEXT NOT NULL DEFAULT '{}',
                status TEXT NOT NULL DEFAULT 'pending',
                dependencies TEXT NOT NULL DEFAULT '[]',
                source_snapshot TEXT,
                validated_changes TEXT,
                remaining_budget TEXT,
                attempt_count INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS attempts (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL REFERENCES tasks(id),
                attempt_number INTEGER NOT NULL,
                started_at INTEGER NOT NULL,
                completed_at INTEGER,
                status TEXT NOT NULL,
                execution_context TEXT,
                tool_outcomes TEXT,
                checkpoint TEXT,
                UNIQUE(task_id, attempt_number)
            );

            CREATE TABLE IF NOT EXISTS events (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL REFERENCES tasks(id),
                version INTEGER NOT NULL,
                event_type TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                payload TEXT NOT NULL,
                replay_context TEXT,
                validated BOOLEAN NOT NULL DEFAULT 1
            );

            CREATE INDEX IF NOT EXISTS idx_events_task ON events(task_id);
            CREATE INDEX IF NOT EXISTS idx_events_type ON events(event_type);
            CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp);

            CREATE TABLE IF NOT EXISTS checkpoints (
                task_id TEXT PRIMARY KEY REFERENCES tasks(id),
                state TEXT NOT NULL,
                context_hash TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                snapshot TEXT,
                changes TEXT,
                tool_outcomes TEXT,
                UNIQUE(task_id)
            );

            CREATE TABLE IF NOT EXISTS approvals (
                id TEXT PRIMARY KEY,
                tool TEXT NOT NULL,
                scope TEXT NOT NULL,
                allowed BOOLEAN NOT NULL,
                decision TEXT NOT NULL,
                granted_at INTEGER NOT NULL,
                expires_at INTEGER,
                task_context TEXT,
                audit_metadata TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_approvals_tool ON approvals(tool);
            CREATE INDEX IF NOT EXISTS idx_approvals_scoped ON approvals(scope, allowed);"
        )?;

        self.apply_memory_schema(conn)?;
        Ok(())
    }

    /// Applies the schema for project memory persistence.
    pub fn apply_memory_schema(conn: &Connection) -> rusqlite::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memory_entries (
                id TEXT PRIMARY KEY,
                category TEXT NOT NULL,
                title TEXT NOT NULL,
                body TEXT NOT NULL,
                source TEXT NOT NULL,
                author TEXT NOT NULL,
                task_id TEXT,
                timestamp INTEGER NOT NULL,
                confidence REAL NOT NULL DEFAULT 1.0,
                evidence TEXT,
                deprecated BOOLEAN NOT NULL DEFAULT 0,
                deleted BOOLEAN NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_memory_category ON memory_entries(category);
            CREATE INDEX IF NOT EXISTS idx_memory_source ON memory_entries(source);
            CREATE INDEX IF NOT EXISTS idx_memory_not_deleted ON memory_entries(deleted, timestamp);"
        )?;
        Ok(())
    }

    /// Applies the schema for the event journal.
    pub fn apply_event_journal_schema(conn: &Connection) -> rusqlite::Result<()> {
        // Main event journal table
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS event_journal (
                event_id TEXT PRIMARY KEY,
                version INTEGER NOT NULL,
                event_type TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                payload TEXT NOT NULL,
                source TEXT NOT NULL,
                validated BOOLEAN NOT NULL DEFAULT 1,
                replayable BOOLEAN NOT NULL DEFAULT 1
            );

            CREATE INDEX IF NOT EXISTS idx_event_journal_type ON event_journal(event_type);
            CREATE INDEX IF NOT EXISTS idx_event_journal_timestamp ON event_journal(timestamp);

            CREATE TABLE IF NOT EXISTS event_checkpoints (
                checkpoint_id TEXT PRIMARY KEY,
                last_event_id TEXT NOT NULL,
                position INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                schema_version TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS replay_sessions (
                session_id TEXT PRIMARY KEY,
                started_at INTEGER NOT NULL,
                last_event TEXT NOT NULL,
                status TEXT NOT NULL,
                context TEXT
            );"
        )?;
        Ok(())
    }

    /// Checks and applies any needed migrations.
    pub fn migrate(conn: &Connection, current_version: &str) -> rusqlite::Result<bool> {
        let migrated = current_version != super::SCHEMA_VERSION;
        if migrated {
            // Record the migration in the schema version table
            conn.execute(
                "CREATE TABLE IF NOT EXISTS schema_version (
                    version TEXT PRIMARY KEY,
                    applied_at INTEGER NOT NULL
                )",
                params![],
            )?;

            conn.execute(
                "INSERT OR REPLACE INTO schema_version (version, applied_at) VALUES (?1, ?2)",
                params![super::SCHEMA_VERSION, SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64],
            )?;
        }
        Ok(migrated)
    }
}