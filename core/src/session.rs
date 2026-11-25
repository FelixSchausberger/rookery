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

/// A top-level conversation and the directory it was started in. Subagent
/// sessions are children of a root and are enumerated by `tree_members`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRoot {
    pub id: String,
    pub directory: String,
    pub title: Option<String>,
    pub updated: Option<i64>,
}

/// A direct child of a top-level conversation, with the newest activity in
/// its whole subtree. A direct child whose subtree has a recent member is not
/// a prune candidate: a resumed grandchild keeps its parent alive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildSession {
    pub id: String,
    pub root: String,
    pub title: Option<String>,
    pub slug: String,
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

    /// Every top-level conversation, oldest first. Archived roots are not
    /// candidates: archiving is an explicit keep. A schema that cannot answer
    /// the query yields an empty list, so the reaper never deletes on a guess.
    pub fn roots(&self) -> Vec<SessionRoot> {
        let Some(connection) = self.open() else {
            return Vec::new();
        };
        let mut statement = match connection.prepare(
            "select id, directory, title, time_updated from session_v2 \
             where parent_id is null and time_archived is null \
             and directory is not null and directory <> '' \
             order by time_updated asc",
        ) {
            Ok(statement) => statement,
            Err(_) => return Vec::new(),
        };
        let rows = statement.query_map([], |row| {
            Ok(SessionRoot {
                id: row.get::<_, String>(0)?,
                directory: row.get::<_, String>(1)?,
                title: row.get::<_, Option<String>>(2)?,
                updated: row.get::<_, Option<i64>>(3)?,
            })
        });
        let Ok(rows) = rows else {
            return Vec::new();
        };
        rows.flatten().collect()
    }

    /// Direct children of live top-level conversations whose whole subtree
    /// has been idle before `cutoff_ms`, and whose root is equally old. A
    /// child with any recent descendant is kept, and an archived root or child
    /// is never a candidate.
    pub fn prune_candidates(&self, cutoff_ms: i64) -> Vec<ChildSession> {
        let Some(connection) = self.open() else {
            return Vec::new();
        };
        let mut statement = match connection.prepare(
            "with recursive tree(top, id, updated) as (
               select child.id, child.id, child.time_updated
               from session_v2 child join session_v2 root on root.id = child.parent_id
               where root.parent_id is null
                 and root.time_archived is null
                 and child.time_archived is null
               union all
               select tree.top, session.id, session.time_updated
               from session_v2 session join tree on session.parent_id = tree.id
             )
             select tree.top, root.id, child.title, coalesce(child.slug, 'session')
             from tree
             join session_v2 child on child.id = tree.top
             join session_v2 root on root.id = child.parent_id
             group by tree.top
             having max(tree.updated) < ?1 and coalesce(root.time_updated, 0) < ?1
             order by max(tree.updated) asc",
        ) {
            Ok(statement) => statement,
            Err(_) => return Vec::new(),
        };
        let rows = statement.query_map([cutoff_ms], |row| {
            Ok(ChildSession {
                id: row.get::<_, String>(0)?,
                root: row.get::<_, String>(1)?,
                title: row.get::<_, Option<String>>(2)?,
                slug: row.get::<_, String>(3)?,
            })
        });
        let Ok(rows) = rows else {
            return Vec::new();
        };
        rows.flatten().collect()
    }

    /// The root and every descendant: `session export` covers one session and
    /// `session delete` removes the whole tree, so every member is exported
    /// before the root is deleted. The slug is a file-name component.
    pub fn tree_members(&self, root: &str) -> Vec<(String, String)> {
        let Some(connection) = self.open() else {
            return Vec::new();
        };
        let mut statement = match connection.prepare(
            "with recursive tree(id) as (
               select id from session_v2 where id = ?1
               union all
               select child.id from session_v2 child join tree parent on child.parent_id = parent.id
             )
             select tree.id, coalesce(session.slug, 'session')
             from tree join session_v2 session on session.id = tree.id",
        ) {
            Ok(statement) => statement,
            Err(_) => return Vec::new(),
        };
        let rows = statement.query_map([root], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        });
        let Ok(rows) = rows else {
            return Vec::new();
        };
        rows.flatten().collect()
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
    fn roots_and_tree_members_follow_the_parent_links() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let connection = Connection::open(&db).unwrap();
        connection
            .execute_batch(
                "create table session_v2 (
                   id text primary key,
                   parent_id text,
                   directory text,
                   title text,
                   slug text,
                   time_updated integer,
                   time_archived integer
                 );
                 insert into session_v2 values
                   ('ses_root', null, '/ws/alpha', 'Root', 'root', 1, null),
                   ('ses_child', 'ses_root', '/ws/alpha', null, 'child', 2, null),
                   ('ses_other', null, '/ws/beta', 'Other', 'other', 3, null);",
            )
            .unwrap();
        drop(connection);
        let provider = SqliteSessions::new(db);
        let roots = provider.roots();
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].id, "ses_root");
        assert_eq!(roots[0].directory, "/ws/alpha");
        assert_eq!(
            provider.tree_members("ses_root"),
            vec![
                ("ses_root".to_string(), "root".to_string()),
                ("ses_child".to_string(), "child".to_string())
            ]
        );
        assert!(provider.tree_members("ses_missing").is_empty());
    }

    #[test]
    fn prune_candidates_keep_roots_recent_subtrees_and_archived_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        let connection = Connection::open(&db).unwrap();
        connection
            .execute_batch(
                "create table session_v2 (
                   id text primary key,
                   parent_id text,
                   directory text,
                   title text,
                   slug text,
                   time_updated integer,
                   time_archived integer
                 );
                 insert into session_v2 values
                   ('ses_old_root', null, '/ws/alpha', 'Old root', 'old-root', 1000, null),
                   ('ses_old_child', 'ses_old_root', '/ws/alpha', 'Old child', 'old-child', 2000, null),
                   ('ses_old_grandchild', 'ses_old_child', '/ws/alpha', null, 'old-grandchild', 3000, null),
                   ('ses_new_root', null, '/ws/alpha', 'New root', 'new-root', 500000, null),
                   ('ses_new_child', 'ses_new_root', '/ws/alpha', null, 'new-child', 4000, null),
                   ('ses_resumed_root', null, '/ws/alpha', 'Resumed root', 'resumed-root', 1500, null),
                   ('ses_resumed_child', 'ses_resumed_root', '/ws/alpha', null, 'resumed-child', 2000, null),
                   ('ses_resumed_grandchild', 'ses_resumed_child', '/ws/alpha', null, 'resumed-grandchild', 900000, null),
                   ('ses_archived_root', null, '/ws/alpha', 'Archived root', 'archived-root', 1000, 1),
                   ('ses_archived_child', 'ses_archived_root', '/ws/alpha', null, 'archived-child', 2000, null);",
            )
            .unwrap();
        drop(connection);
        let provider = SqliteSessions::new(db);
        assert_eq!(provider.roots().len(), 3);
        let candidates = provider.prune_candidates(5000);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].id, "ses_old_child");
        assert_eq!(candidates[0].root, "ses_old_root");
        assert_eq!(candidates[0].title.as_deref(), Some("Old child"));
        assert_eq!(candidates[0].slug, "old-child");
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
