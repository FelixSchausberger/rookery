use crate::error::{Error, Result};
use crate::util::now_secs;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

fn default_version() -> u32 {
    2
}

/// The task lifecycle. `open` groups workspaces and holds them out of every
/// reclamation path; `done` is the lead's decision that the work is finished
/// and the marked workspaces may be swept; `archived` is hidden from the board.
pub const OPEN: &str = "open";
pub const DONE: &str = "done";
pub const ARCHIVED: &str = "archived";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub status: String,
    /// The task this one belongs to, or None for a root. One level is allowed:
    /// a child may not have children and a parent may not have a parent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub workspaces: Vec<String>,
    pub notes: String,
    pub created: i64,
    pub updated: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskStore {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub tasks: Vec<Task>,
}

impl Default for TaskStore {
    fn default() -> Self {
        Self {
            version: default_version(),
            tasks: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Store {
    base: PathBuf,
}

impl Store {
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    pub fn path(&self) -> PathBuf {
        self.base.join("tasks.json")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.base.join("tasks.json.lock")
    }

    /// A missing store is an empty document. Read paths take no lock: the only
    /// writer replaces the document by rename, so a reader sees old or new.
    pub fn read(&self) -> Result<TaskStore> {
        let path = self.path();
        match std::fs::read_to_string(&path) {
            Ok(body) => serde_json::from_str(&body).map_err(|_| {
                Error::message(format!(
                    "rookery: task store is unreadable: {}",
                    path.display()
                ))
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(TaskStore::default()),
            Err(err) => Err(Error::message(format!(
                "rookery: task store is unreadable: {}: {err}",
                path.display()
            ))),
        }
    }

    /// The board tolerates a corrupt store and shows unassigned rows.
    pub fn read_tolerant(&self) -> TaskStore {
        self.read().unwrap_or_default()
    }

    /// Serialize writers on the sidecar lock, re-read inside it, then rename a
    /// temp file in the same directory over the store. A failed filter is a
    /// refusal (exit 2) and leaves the document untouched.
    pub fn mutate<T>(&self, update: impl FnOnce(&mut TaskStore) -> Result<T>) -> Result<T> {
        std::fs::create_dir_all(&self.base).map_err(|err| {
            Error::message(format!(
                "rookery: cannot create the task store directory '{}': {err}",
                self.base.display()
            ))
        })?;
        let lock = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(self.lock_path())
            .map_err(|_| Error::message("rookery: cannot open the task store lock"))?;
        if !flock_timeout(&lock, 10) {
            return Err(Error::message("rookery: task store is busy; retry"));
        }
        let path = self.path();
        let mut store = match std::fs::read_to_string(&path) {
            Ok(body) => serde_json::from_str::<TaskStore>(&body)
                .map_err(|err| Error::message(format!("rookery: task store: {err}")))?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => TaskStore::default(),
            Err(err) => return Err(Error::message(format!("rookery: task store: {err}"))),
        };
        let result = update(&mut store)?;
        // A write always lands the current document shape, even when the file
        // was still a version-1 store with no parent fields.
        store.version = default_version();
        let mut temp = tempfile::NamedTempFile::new_in(&self.base)
            .map_err(|_| Error::message("rookery: cannot write the task store"))?;
        serde_json::to_writer(temp.as_file_mut(), &store)
            .map_err(|_| Error::message("rookery: cannot write the task store"))?;
        temp.as_file_mut()
            .write_all(b"\n")
            .map_err(|_| Error::message("rookery: cannot write the task store"))?;
        temp.persist(&path)
            .map_err(|_| Error::message("rookery: cannot replace the task store"))?;
        let _ = unlock(&lock);
        Ok(result)
    }

    pub fn exists(&self, id: &str) -> Result<bool> {
        Ok(self.read()?.tasks.iter().any(|task| task.id == id))
    }

    pub fn is_open(&self, id: &str) -> Result<bool> {
        Ok(self
            .read()?
            .tasks
            .iter()
            .any(|task| task.id == id && task.status == "open"))
    }

    pub fn task(&self, id: &str) -> Result<Option<Task>> {
        Ok(self.read()?.tasks.into_iter().find(|task| task.id == id))
    }

    /// Workspaces grouped under an open task. GC keeps every one of them,
    /// whatever jj says about the workspace, until the lead finishes the task.
    pub fn open_workspaces(&self) -> Result<HashMap<String, String>> {
        let mut open = HashMap::new();
        for task in self.read()?.tasks {
            if task.status != OPEN {
                continue;
            }
            let id = task.id;
            for workspace in task.workspaces {
                open.insert(workspace, id.clone());
            }
        }
        Ok(open)
    }

    pub fn ws_task(&self, workspace: &str) -> Option<String> {
        self.read_tolerant().tasks.into_iter().find_map(|task| {
            if task.workspaces.iter().any(|entry| entry == workspace) {
                Some(task.id)
            } else {
                None
            }
        })
    }

    pub fn ws_map(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for task in self.read_tolerant().tasks {
            for workspace in task.workspaces {
                map.insert(workspace, task.id.clone());
            }
        }
        map
    }

    pub fn add(
        &self,
        id: &str,
        title: &str,
        notes: &str,
        parent: Option<&str>,
    ) -> Result<()> {
        self.mutate(|store| {
            if store.tasks.iter().any(|task| task.id == id) {
                return Err(Error::refused(format!("rookery: task {id} already exists")));
            }
            validate_parent(store, id, parent)?;
            let now = now_secs();
            store.tasks.push(Task {
                id: id.to_string(),
                title: title.to_string(),
                status: OPEN.to_string(),
                parent: parent.map(str::to_string),
                workspaces: Vec::new(),
                notes: notes.to_string(),
                created: now,
                updated: now,
            });
            Ok(())
        })
    }

    /// Set or clear a task's parent. The store enforces one level: the parent
    /// must be an open root, and the child must not have children of its own.
    pub fn set_parent(&self, id: &str, parent: Option<&str>) -> Result<()> {
        self.mutate(|store| {
            let Some(index) = store.tasks.iter().position(|task| task.id == id) else {
                return Err(Error::refused(format!("rookery: no task named {id}")));
            };
            if store.tasks[index].status == ARCHIVED {
                return Err(Error::refused(format!("rookery: task {id} is archived")));
            }
            validate_parent(store, id, parent)?;
            let now = now_secs();
            store.tasks[index].parent = parent.map(str::to_string);
            store.tasks[index].updated = now;
            Ok(())
        })
    }

    pub fn assign(&self, id: &str, workspace: &str, move_workspace: bool) -> Result<()> {
        self.mutate(|store| {
            let Some(target) = store.tasks.iter().find(|task| task.id == id) else {
                return Err(Error::refused(format!("rookery: no task named {id}")));
            };
            if target.status != OPEN {
                return Err(Error::refused(format!(
                    "rookery: task {id} is {}; only an open task accepts workspaces",
                    target.status
                )));
            }
            let owner = store
                .tasks
                .iter()
                .find(|task| task.workspaces.iter().any(|entry| entry == workspace))
                .map(|task| task.id.clone());
            if let Some(owner) = owner {
                if owner != id && !move_workspace {
                    return Err(Error::refused(format!(
                        "rookery: workspace {workspace} belongs to task {owner}; use --move"
                    )));
                }
            }
            let now = now_secs();
            for task in &mut store.tasks {
                if task.id == id {
                    task.workspaces.retain(|entry| entry != workspace);
                    task.workspaces.push(workspace.to_string());
                    task.updated = now;
                } else if task.workspaces.iter().any(|entry| entry == workspace) {
                    task.workspaces.retain(|entry| entry != workspace);
                    task.updated = now;
                }
            }
            Ok(())
        })
    }

    pub fn rename_ws(&self, old: &str, new: &str) -> Result<()> {
        self.mutate(|store| {
            let now = now_secs();
            for task in &mut store.tasks {
                if task.workspaces.iter().any(|entry| entry == old) {
                    task.workspaces.retain(|entry| entry != old && entry != new);
                    task.workspaces.push(new.to_string());
                    task.updated = now;
                }
            }
            Ok(())
        })
    }

    pub fn drop_ws(&self, workspace: &str) -> Result<()> {
        self.mutate(|store| {
            let now = now_secs();
            for task in &mut store.tasks {
                if task.workspaces.iter().any(|entry| entry == workspace) {
                    task.workspaces.retain(|entry| entry != workspace);
                    task.updated = now;
                }
            }
            Ok(())
        })
    }

    pub fn status(&self, id: &str) -> Result<Option<String>> {
        Ok(self.task(id)?.map(|task| task.status))
    }

    /// Set the lifecycle state. Callers own the transition; the store only
    /// refuses a missing task and a corrupt document.
    pub fn set_status(&self, id: &str, status: &str) -> Result<()> {
        self.mutate(|store| {
            let Some(target) = store.tasks.iter_mut().find(|task| task.id == id) else {
                return Err(Error::refused(format!("rookery: no task named {id}")));
            };
            target.status = status.to_string();
            target.updated = now_secs();
            Ok(())
        })
    }

    /// Append a decision or follow-up to the task's durable notes. The task's
    /// `updated` stamp is the per-note timestamp; no calendar formatting is
    /// duplicated in the core.
    pub fn note(&self, id: &str, text: &str) -> Result<()> {
        self.mutate(|store| {
            let Some(target) = store.tasks.iter_mut().find(|task| task.id == id) else {
                return Err(Error::refused(format!("rookery: no task named {id}")));
            };
            if !target.notes.is_empty() && !target.notes.ends_with('\n') {
                target.notes.push('\n');
            }
            target.updated = now_secs();
            target.notes.push_str(&format!("- {text}"));
            Ok(())
        })
    }

    /// A done task whose last workspace has been removed leaves the board.
    /// Open tasks stay visible even with zero workspaces: an unstarted task is
    /// a backlog item, not an archive entry.
    pub fn archive_if_empty_done(&self, id: &str) -> Result<bool> {
        self.mutate(|store| {
            let Some(index) = store.tasks.iter().position(|task| task.id == id) else {
                return Ok(false);
            };
            if store.tasks[index].status != DONE || !store.tasks[index].workspaces.is_empty() {
                return Ok(false);
            }
            if store
                .tasks
                .iter()
                .any(|task| task.parent.as_deref() == Some(id) && task.status != ARCHIVED)
            {
                return Ok(false);
            }
            store.tasks[index].status = ARCHIVED.to_string();
            store.tasks[index].updated = now_secs();
            Ok(true)
        })
    }

    pub fn archive(&self, id: &str) -> Result<()> {
        self.mutate(|store| {
            let Some(target) = store.tasks.iter().find(|task| task.id == id) else {
                return Err(Error::refused(format!("rookery: no task named {id}")));
            };
            if target.status == ARCHIVED {
                return Err(Error::refused(format!("rookery: task {id} is archived")));
            }
            if store
                .tasks
                .iter()
                .any(|task| task.parent.as_deref() == Some(id) && task.status != ARCHIVED)
            {
                return Err(Error::refused(format!(
                    "rookery: task {id} still has child tasks; archive them first"
                )));
            }
            if !target.workspaces.is_empty() {
                return Err(Error::refused(format!(
                    "rookery: task {id} still has workspaces; run task done and clean --apply first"
                )));
            }
            let now = now_secs();
            for task in &mut store.tasks {
                if task.id == id {
                    task.status = ARCHIVED.to_string();
                    task.updated = now;
                }
            }
            Ok(())
        })
    }

    /// The seat `open` focuses: a live workspace when there is one, otherwise
    /// the first grouped workspace jj still lists.
    pub fn pick_workspace(
        &self,
        id: &str,
        exists: impl Fn(&str) -> bool,
        live: impl Fn(&str) -> bool,
    ) -> Option<String> {
        let store = self.read_tolerant();
        let task = store.tasks.into_iter().find(|task| task.id == id)?;
        let mut chosen = None;
        let mut live_workspace = None;
        for workspace in task.workspaces {
            if !exists(&workspace) {
                continue;
            }
            if chosen.is_none() {
                chosen = Some(workspace.clone());
            }
            if live_workspace.is_none() && live(&workspace) {
                live_workspace = Some(workspace);
            }
        }
        live_workspace.or(chosen)
    }
}

/// One level of nesting keeps the board tree, the cleanup rules, and the
/// fan-out cap of the lead protocol simple. A child may not have children and
/// a parent may not have a parent; both checks also make cycles impossible.
fn validate_parent(store: &TaskStore, child: &str, parent: Option<&str>) -> Result<()> {
    let Some(parent_id) = parent else {
        return Ok(());
    };
    if parent_id == child {
        return Err(Error::invalid(format!(
            "rookery: task {child} cannot be its own parent"
        )));
    }
    let Some(parent_task) = store.tasks.iter().find(|task| task.id == parent_id) else {
        return Err(Error::invalid(format!(
            "rookery: no task named {parent_id}"
        )));
    };
    if parent_task.status != OPEN {
        return Err(Error::invalid(format!(
            "rookery: task {parent_id} is {}; only an open task can be a parent",
            parent_task.status
        )));
    }
    if parent_task.parent.is_some() {
        return Err(Error::invalid(format!(
            "rookery: task {parent_id} is already a child; only one level is allowed"
        )));
    }
    if store
        .tasks
        .iter()
        .any(|task| task.parent.as_deref() == Some(child))
    {
        return Err(Error::invalid(format!(
            "rookery: task {child} already has children; only one level is allowed"
        )));
    }
    Ok(())
}

fn flock_timeout(file: &File, seconds: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn unlock(file: &File) -> std::io::Result<()> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[allow(dead_code)]
pub fn store_path(base: &Path) -> PathBuf {
    base.join("tasks.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn missing_store_is_empty_and_read_does_not_create_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        assert!(store.read().unwrap().tasks.is_empty());
        assert!(!store.path().exists());
    }

    #[test]
    fn add_assign_move_and_archive() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        store.add("one", "One", "", None).unwrap();
        store.add("two", "Two", "", None).unwrap();
        store.assign("one", "ws", false).unwrap();
        assert!(store.assign("two", "ws", false).is_err());
        store.assign("two", "ws", true).unwrap();
        assert_eq!(store.ws_task("ws").as_deref(), Some("two"));
        store.drop_ws("ws").unwrap();
        assert!(store.ws_task("ws").is_none());
        store.archive("one").unwrap();
        assert!(!store.is_open("one").unwrap());
        store.archive("two").unwrap();
        store.add("with-ws", "With a workspace", "", None).unwrap();
        store.assign("with-ws", "kept", false).unwrap();
        assert!(store.archive("with-ws").is_err());
    }

    #[test]
    fn done_gate_notes_and_auto_archive() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        store.add("t", "T", "", None).unwrap();
        store.note("t", "first").unwrap();
        store.note("t", "second").unwrap();
        assert_eq!(store.task("t").unwrap().unwrap().notes, "- first\n- second");
        store.assign("t", "ws", false).unwrap();
        store.set_status("t", DONE).unwrap();
        assert!(store.assign("t", "other", false).is_err());
        assert!(!store.archive_if_empty_done("t").unwrap());
        store.drop_ws("ws").unwrap();
        assert!(store.archive_if_empty_done("t").unwrap());
        assert_eq!(store.status("t").unwrap().as_deref(), Some(ARCHIVED));
        assert!(!store.archive_if_empty_done("t").unwrap());
    }

    #[test]
    fn parent_links_are_one_level_and_archive_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        store.add("lead", "Lead", "", None).unwrap();
        store.add("work", "Work", "", Some("lead")).unwrap();
        assert_eq!(
            store.task("work").unwrap().unwrap().parent.as_deref(),
            Some("lead")
        );

        assert!(store.add("deep", "Deep", "", Some("work")).is_err());
        assert!(store.set_parent("lead", Some("work")).is_err());
        assert!(store.set_parent("lead", Some("lead")).is_err());
        assert!(store.set_parent("work", Some("missing")).is_err());

        store.set_status("lead", DONE).unwrap();
        assert!(store.archive("lead").is_err());
        store.archive("work").unwrap();
        assert!(store.archive_if_empty_done("lead").unwrap());
        assert_eq!(store.status("lead").unwrap().as_deref(), Some(ARCHIVED));
    }

    #[test]
    fn a_version_one_store_reads_as_roots_and_upgrades_on_write() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        std::fs::write(
            store.path(),
            r#"{"version":1,"tasks":[{"id":"old","title":"Old","status":"open","workspaces":[],"notes":"","created":1,"updated":1}]}"#,
        )
        .unwrap();
        let read = store.read().unwrap();
        assert_eq!(read.version, 1);
        assert_eq!(read.tasks[0].parent, None);

        store.add("child", "Child", "", Some("old")).unwrap();
        let read = store.read().unwrap();
        assert_eq!(read.version, 2);
        assert_eq!(read.tasks[0].parent, None);
        assert_eq!(read.tasks[1].parent.as_deref(), Some("old"));
    }

    #[test]
    fn open_workspaces_excludes_done_and_archived() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        store.add("open", "Open", "", None).unwrap();
        store.add("done", "Done", "", None).unwrap();
        store.add("archived", "Archived", "", None).unwrap();
        store.assign("open", "ws-open", false).unwrap();
        store.assign("done", "ws-done", false).unwrap();
        store.assign("archived", "ws-archived", false).unwrap();
        store.set_status("done", DONE).unwrap();
        store.set_status("archived", ARCHIVED).unwrap();
        let open = store.open_workspaces().unwrap();
        assert_eq!(open.get("ws-open").map(String::as_str), Some("open"));
        assert!(!open.contains_key("ws-done"));
        assert!(!open.contains_key("ws-archived"));
    }

    #[test]
    fn concurrent_adds_serialize_on_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(dir.path()));
        let mut handles = Vec::new();
        for index in 0..8 {
            let store = Arc::clone(&store);
            handles.push(std::thread::spawn(move || {
                store
                    .add(&format!("parallel-{index}"), "Parallel", "", None)
                    .unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        let body = std::fs::read_to_string(store.path()).unwrap();
        let parsed: TaskStore = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed.tasks.len(), 8);
        assert_eq!(parsed.version, 2);
    }

    #[test]
    fn corrupt_store_is_reported_and_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        std::fs::write(store.path(), "garbage\n").unwrap();
        assert!(store.read().is_err());
        assert!(store.read_tolerant().tasks.is_empty());
        assert!(store.add("x", "X", "", None).is_err());
        assert_eq!(std::fs::read_to_string(store.path()).unwrap(), "garbage\n");
    }
}
