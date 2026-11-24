use crate::util::now_ms;
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionRow {
    pub directory: String,
    pub title: Option<String>,
    pub updated: Option<i64>,
}

/// The agent-session provider. gc asks for directories, the JSON board asks
/// for the conversation title and last update. When the trait is absent the
/// board falls back to the process and touch checks.
pub trait SessionProvider: Send + Sync {
    fn recent_dirs(&self, age_seconds: i64) -> Vec<String>;
    fn recent_meta(&self, age_seconds: i64) -> Vec<SessionRow>;
}

#[derive(Clone, Debug)]
pub struct SqliteSessions {
    db: PathBuf,
}

impl SqliteSessions {
    pub fn new(db: impl Into<PathBuf>) -> Self {
        Self { db: db.into() }
    }

    fn open(&self) -> Option<Connection> {
        // A missing database is a normal state on a host without OpenCode, and
        // a schema change is not the board's failure: both are silent misses.
        if !self.db.is_file() {
            return None;
        }
        let connection = Connection::open_with_flags(
            &self.db,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ok()?;
        let _ = connection.busy_timeout(Duration::from_millis(2000));
        Some(connection)
    }

    fn since(age_seconds: i64) -> i64 {
        now_ms() - (age_seconds.max(0) * 1000)
    }
}

impl SessionProvider for SqliteSessions {
    fn recent_dirs(&self, age_seconds: i64) -> Vec<String> {
        let Some(connection) = self.open() else {
            return Vec::new();
        };
        let mut statement = match connection.prepare(
            "select distinct directory from session_v2 \
             where time_archived is null and time_updated >= ?1",
        ) {
            Ok(statement) => statement,
            Err(_) => return Vec::new(),
        };
        let rows = statement.query_map([Self::since(age_seconds)], |row| row.get::<_, String>(0));
        let Ok(rows) = rows else {
            return Vec::new();
        };
        rows.flatten()
            .filter(|directory| !directory.is_empty())
            .collect()
    }

    fn recent_meta(&self, age_seconds: i64) -> Vec<SessionRow> {
        let Some(connection) = self.open() else {
            return Vec::new();
        };
        // SQLite's bare-column rule picks the title from the row that carries
        // max(time_updated), which is the conversation a person last touched.
        let mut statement = match connection.prepare(
            "select directory, title, max(time_updated) as updated from session_v2 \
             where time_archived is null and time_updated >= ?1 group by directory",
        ) {
            Ok(statement) => statement,
            Err(_) => return Vec::new(),
        };
        let rows = statement.query_map([Self::since(age_seconds)], |row| {
            Ok(SessionRow {
                directory: row.get::<_, String>(0)?,
                title: row.get::<_, Option<String>>(1)?,
                updated: row.get::<_, Option<i64>>(2)?,
            })
        });
        let Ok(rows) = rows else {
            return Vec::new();
        };
        rows.flatten()
            .filter(|row| !row.directory.is_empty())
            .collect()
    }
}

/// Reads nothing; the guard-silent fallback the shell gets without sqlite3.
pub struct NoSessions;

impl SessionProvider for NoSessions {
    fn recent_dirs(&self, _age_seconds: i64) -> Vec<String> {
        Vec::new()
    }

    fn recent_meta(&self, _age_seconds: i64) -> Vec<SessionRow> {
        Vec::new()
    }
}

/// The fixture opencode database the tests create has this schema; core only
/// depends on it through the provider, never on a concrete database layout.
#[allow(dead_code)]
pub fn db_path_from(config: &crate::config::Config) -> &Path {
    &config.session_db
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_database_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let provider = SqliteSessions::new(dir.path().join("absent.db"));
        assert!(provider.recent_dirs(604800).is_empty());
        assert!(provider.recent_meta(604800).is_empty());
    }

    #[test]
    fn broken_schema_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let connection = Connection::open(&db).unwrap();
        connection
            .execute("create table unrelated (x text)", [])
            .unwrap();
        drop(connection);
        let provider = SqliteSessions::new(db);
        assert!(provider.recent_dirs(604800).is_empty());
        assert!(provider.recent_meta(604800).is_empty());
    }
}
