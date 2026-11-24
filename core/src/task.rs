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
    1
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub status: String,
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

    pub fn add(&self, id: &str, title: &str, notes: &str) -> Result<()> {
        self.mutate(|store| {
            if store.tasks.iter().any(|task| task.id == id) {
                return Err(Error::refused(format!("rookery: task {id} already exists")));
            }
            let now = now_secs();
            store.tasks.push(Task {
                id: id.to_string(),
                title: title.to_string(),
                status: "open".to_string(),
                workspaces: Vec::new(),
                notes: notes.to_string(),
                created: now,
                updated: now,
            });
            Ok(())
        })
    }

    pub fn assign(&self, id: &str, workspace: &str, move_workspace: bool) -> Result<()> {
        self.mutate(|store| {
            let Some(target) = store.tasks.iter().find(|task| task.id == id) else {
                return Err(Error::refused(format!("rookery: no task named {id}")));
            };
            if target.status == "archived" {
                return Err(Error::refused(format!("rookery: task {id} is archived")));
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

    pub fn archive(&self, id: &str) -> Result<()> {
        self.mutate(|store| {
            let Some(target) = store.tasks.iter().find(|task| task.id == id) else {
                return Err(Error::refused(format!("rookery: no task named {id}")));
            };
            if !target.workspaces.is_empty() {
                return Err(Error::refused(format!(
                    "rookery: task {id} still has workspaces; run task done and clean --apply first"
                )));
            }
            let now = now_secs();
            for task in &mut store.tasks {
                if task.id == id {
                    task.status = "archived".to_string();
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
        store.add("one", "One", "").unwrap();
        store.add("two", "Two", "").unwrap();
        store.assign("one", "ws", false).unwrap();
        assert!(store.assign("two", "ws", false).is_err());
        store.assign("two", "ws", true).unwrap();
        assert_eq!(store.ws_task("ws").as_deref(), Some("two"));
        store.drop_ws("ws").unwrap();
        assert!(store.ws_task("ws").is_none());
        store.archive("one").unwrap();
        assert!(!store.is_open("one").unwrap());
        store.archive("two").unwrap();
        store.add("with-ws", "With a workspace", "").unwrap();
        store.assign("with-ws", "kept", false).unwrap();
        assert!(store.archive("with-ws").is_err());
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
                    .add(&format!("parallel-{index}"), "Parallel", "")
                    .unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        let body = std::fs::read_to_string(store.path()).unwrap();
        let parsed: TaskStore = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed.tasks.len(), 8);
        assert_eq!(parsed.version, 1);
    }

    #[test]
    fn corrupt_store_is_reported_and_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        std::fs::write(store.path(), "garbage\n").unwrap();
        assert!(store.read().is_err());
        assert!(store.read_tolerant().tasks.is_empty());
        assert!(store.add("x", "X", "").is_err());
        assert_eq!(std::fs::read_to_string(store.path()).unwrap(), "garbage\n");
    }
}
