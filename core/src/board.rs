use crate::config::Config;
use crate::error::{Error, Result};
use crate::jj::{self, State, WsEntry};
use crate::session::{SessionProvider, SessionRow, SqliteSessions};
use crate::task::{Store, Task};
use crate::util::{is_writable, now_secs, realpath_m, sha256_hex};
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Hold {
    Live,
    Session,
    #[default]
    Idle,
}

impl Hold {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Session => "session",
            Self::Idle => "-",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SessionInfo {
    pub title: Option<String>,
    pub updated: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WaitInfo {
    pub reason: String,
    pub age: i64,
    pub days: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CleanInfo {
    pub age: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceRow {
    pub task: Option<String>,
    pub workspace: String,
    pub path: PathBuf,
    pub state: State,
    pub hold: Hold,
    pub description: String,
    pub session: Option<SessionInfo>,
    pub wait: Option<WaitInfo>,
    pub clean: Option<CleanInfo>,
    pub age: i64,
}

#[derive(Clone, Debug, Default)]
pub struct Board {
    pub workspaces: Vec<WorkspaceRow>,
    pub tasks: Vec<Task>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub total: i64,
    pub empty: i64,
    pub merged: i64,
    pub unpushed: i64,
    pub inflight: i64,
    pub conflict: i64,
    pub stale: i64,
    pub waiting: i64,
    pub waitold: i64,
}

impl Board {
    /// One classification pass shared by every renderer. `want_meta` selects
    /// the JSON board's session titles and task join; the text modes never read
    /// the task store and query only the session directories for the hold.
    pub fn load(config: &Config, refresh: bool, want_meta: bool) -> Result<Self> {
        let workspaces = jj::workspace_list()?;
        let roots: Vec<PathBuf> = workspaces
            .iter()
            .map(|entry| realpath_m(entry.root.as_deref().unwrap_or(&config.ws_dir(&entry.name))))
            .collect();
        let board_roots: HashSet<PathBuf> = roots.iter().cloned().collect();
        let board_root_strings: HashSet<String> = roots
            .iter()
            .map(|root| root.to_string_lossy().into_owned())
            .collect();
        let main_cid = jj::trunk_commit(&config.trunk)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                Error::message(format!(
                    "rookery ls: {} not found; run 'jj git fetch' first",
                    config.trunk
                ))
            })?;
        let op_id = jj::op_id().filter(|value| !value.is_empty());
        let repo_id = config
            .repo_id
            .get_or_init(|| jj::repo_id(&config.root))
            .clone();
        let cache = BoardCache::new(&config.cache_dir, &repo_id);
        let key = cache_key(&op_id, &main_cid, &workspaces, &roots);
        let provider = SqliteSessions::new(config.session_db.clone());
        let age = config.gc_session_age;

        let (sessions, procs, states) = std::thread::scope(|scope| {
            let sessions =
                scope.spawn(|| load_sessions(&provider, want_meta, age, &board_root_strings));
            let procs = scope.spawn(|| ProcSnapshot::scan(Some(&board_roots)));
            let states =
                classify_cached(config, refresh, &workspaces, op_id.as_deref(), &key, &cache);
            (
                sessions.join().unwrap_or_default(),
                procs.join().unwrap_or_default(),
                states,
            )
        });
        let states = states?;

        let store = Store::new(&config.base);
        let task_map = if want_meta {
            store.ws_map()
        } else {
            HashMap::new()
        };
        let mut rows = Vec::new();
        let now = now_secs();
        for (index, entry) in workspaces.iter().enumerate() {
            let root = &roots[index];
            let state = states.get(&entry.name).copied().ok_or_else(|| {
                Error::message(format!(
                    "rookery: no classification for workspace '{}'",
                    entry.name
                ))
            })?;
            let live = procs.live(root, &config.live_exclude);
            let session_dir = root.to_string_lossy().into_owned();
            let hold = if live {
                Hold::Live
            } else if matches!(state, State::Empty | State::Merged)
                && sessions.dirs.contains(&session_dir)
            {
                Hold::Session
            } else {
                Hold::Idle
            };
            let session = if want_meta {
                sessions.meta.get(&session_dir).map(|row| SessionInfo {
                    title: row.title.clone(),
                    updated: row.updated,
                })
            } else {
                None
            };
            rows.push(WorkspaceRow {
                task: task_map.get(&entry.name).cloned(),
                workspace: entry.name.clone(),
                path: root.clone(),
                state,
                hold,
                description: entry.description.clone(),
                session,
                wait: read_wait(config, &entry.name, now),
                clean: read_clean(config, &entry.name, now),
                age: (now - entry.stamp).max(0),
            });
        }
        let tasks = if want_meta {
            store.read_tolerant().tasks
        } else {
            Vec::new()
        };
        Ok(Self {
            workspaces: rows,
            tasks,
        })
    }

    pub fn counts(&self, stale_days: i64) -> Counts {
        let mut counts = Counts::default();
        for row in &self.workspaces {
            if row.workspace == "default" {
                continue;
            }
            counts.total += 1;
            match row.state {
                State::Empty => counts.empty += 1,
                State::Merged => counts.merged += 1,
                State::Unpushed => counts.unpushed += 1,
                State::InFlight => counts.inflight += 1,
                State::Conflict => counts.conflict += 1,
            }
            if let Some(wait) = &row.wait {
                counts.waiting += 1;
                if wait.days >= stale_days {
                    counts.waitold += 1;
                }
            }
            if row.hold == Hold::Idle
                && row.wait.is_none()
                && matches!(row.state, State::Empty | State::Merged)
            {
                counts.stale += 1;
            }
        }
        counts
    }

    pub fn render_counts(&self, stale_days: i64) -> String {
        let counts = self.counts(stale_days);
        format!(
            "{} {} {} {} {} {} {} {}\n",
            counts.total,
            counts.empty,
            counts.merged,
            counts.unpushed,
            counts.inflight,
            counts.conflict,
            counts.stale,
            counts.waitold
        )
    }

    pub fn render_board(&self) -> String {
        let mut parked = Vec::new();
        let mut progress = Vec::new();
        let mut review = Vec::new();
        let mut conflict = Vec::new();
        let mut claimed = Vec::new();
        let mut reclaimable = Vec::new();
        for row in &self.workspaces {
            if row.workspace == "default" {
                continue;
            }
            let (_, description) = display(row);
            let line = format!("  {:<16} {}", row.workspace, description);
            if row.wait.is_some() {
                parked.push(line);
                continue;
            }
            match row.state {
                State::Unpushed => progress.push(line),
                State::InFlight => review.push(line),
                State::Conflict => conflict.push(line),
                State::Empty | State::Merged => {
                    if row.hold == Hold::Idle {
                        reclaimable.push(line);
                    } else {
                        claimed.push(line);
                    }
                }
            }
        }
        let mut out = String::from(
            "rookery board - state derived from jj plus the wait marker and recent sessions; cleanup happens only under RECLAIMABLE\n",
        );
        append_section(&mut out, "PARKED (waiting on the outside world)", &parked);
        append_section(&mut out, "IN PROGRESS (unpushed)", &progress);
        append_section(&mut out, "IN REVIEW (open PR)", &review);
        append_section(&mut out, "NEEDS YOU (conflict)", &conflict);
        append_section(&mut out, "CLAIMED (session active)", &claimed);
        append_section(
            &mut out,
            "RECLAIMABLE (gc removes once idle and unoccupied)",
            &reclaimable,
        );
        out
    }

    pub fn render_table(&self, trunk: &str, stale_days: i64) -> String {
        let counts = self.counts(stale_days);
        let mut out = String::from("Workspace states:\n");
        out.push_str(&format!(
            "  {:<18} {:<10} {:<8} {}\n",
            "NAME", "STATE", "HOLD", "DESCRIPTION"
        ));
        for row in &self.workspaces {
            let (state_display, description) = display(row);
            out.push_str(&format!(
                "  {:<18} {:<10} {:<8} {}\n",
                row.workspace,
                state_display,
                row.hold.as_str(),
                description
            ));
        }
        out.push('\n');
        out.push_str("  empty/merged: reclaimable by 'rookery gc' once idle and unoccupied\n");
        out.push_str("  HOLD:         live = process rooted here, session = recent opencode session, - = idle\n");
        out.push_str(&format!(
            "  unpushed:     commits outside {trunk} no remote bookmark tracks\n"
        ));
        out.push_str("  in-flight:    commits outside the trunk with an open PR\n");
        out.push_str("  conflict:     needs 'jj resolve'\n");
        out.push_str("  waiting:      parked with 'rookery wait'; gc keeps until 'rookery go'\n");
        out.push('\n');
        out.push_str(&format!(
            "Summary: {} empty, {} merged, {} unpushed, {} in-flight, {} conflict, {} stale, {} waiting ({} reminder)\n",
            counts.empty,
            counts.merged,
            counts.unpushed,
            counts.inflight,
            counts.conflict,
            counts.stale,
            counts.waiting,
            counts.waitold
        ));
        out
    }

    pub fn render_json(&self) -> String {
        let mut out = String::new();
        for row in &self.workspaces {
            let view = WorkspaceJson {
                task: row.task.as_deref(),
                workspace: &row.workspace,
                path: row.path.to_string_lossy().into_owned(),
                state: row.state.as_str(),
                hold: row.hold.as_str(),
                description: &row.description,
                session: row.session.as_ref(),
                wait: row.wait.as_ref().map(|wait| WaitJson {
                    reason: &wait.reason,
                    age: wait.age,
                }),
                clean: row.clean.as_ref(),
                age: row.age,
            };
            if let Ok(line) = serde_json::to_string(&view) {
                out.push_str(&line);
                out.push('\n');
            }
        }
        out
    }

    /// The TUI's five derived lanes. Roots are tasks with no parent (or a
    /// parent the store no longer has); their children travel with them and
    /// never occupy a lane of their own. A workspace with no open task is its
    /// own row; archived tasks are hidden.
    pub fn lanes(&self) -> Vec<LaneView> {
        let open: BTreeMap<&str, &Task> = self
            .tasks
            .iter()
            .filter(|task| task.status == "open")
            .map(|task| (task.id.as_str(), task))
            .collect();
        let mut grouped: BTreeMap<String, Vec<WorkspaceRow>> = BTreeMap::new();
        let mut standalone: Vec<WorkspaceRow> = Vec::new();
        for workspace in &self.workspaces {
            if workspace.workspace == "default" {
                continue;
            }
            match workspace.task.as_deref() {
                Some(id) if open.contains_key(id) => {
                    grouped
                        .entry(id.to_string())
                        .or_default()
                        .push(workspace.clone());
                }
                _ => standalone.push(workspace.clone()),
            }
        }
        let mut views: BTreeMap<String, TaskView> = open
            .iter()
            .map(|(id, task)| {
                let mut workspaces = grouped.remove(*id).unwrap_or_default();
                workspaces.sort_by(|a, b| a.workspace.cmp(&b.workspace));
                (
                    (*id).to_string(),
                    TaskView {
                        id: Some((*id).to_string()),
                        title: task.title.clone(),
                        parent: task.parent.clone(),
                        workspaces,
                        children: Vec::new(),
                    },
                )
            })
            .collect();
        let mut roots: Vec<TaskView> = Vec::new();
        let root_ids: Vec<String> = views
            .keys()
            .filter(|id| is_root(&open, id))
            .cloned()
            .collect();
        for id in root_ids {
            let Some(mut view) = views.remove(&id) else {
                continue;
            };
            let child_ids: Vec<String> = views
                .iter()
                .filter(|(_, child)| child.parent.as_deref() == Some(id.as_str()))
                .map(|(id, _)| id.clone())
                .collect();
            for child_id in child_ids {
                if let Some(child) = views.remove(&child_id) {
                    view.children.push(child);
                }
            }
            roots.push(view);
        }
        // Defensive: a hand-edited deeper or cyclic link must not hide a task.
        while let Some((_, view)) = views.pop_first() {
            roots.push(view);
        }
        for workspace in standalone {
            let title = if workspace.description.trim().is_empty() {
                workspace.workspace.clone()
            } else {
                workspace.description.clone()
            };
            roots.push(TaskView {
                id: None,
                title,
                parent: None,
                workspaces: vec![workspace],
                children: Vec::new(),
            });
        }
        for view in &mut roots {
            view.children.sort_by(compare_views);
        }
        roots.sort_by(compare_views);
        let mut lanes: Vec<LaneView> = Lane::ALL
            .iter()
            .map(|lane| LaneView {
                lane: *lane,
                tasks: Vec::new(),
            })
            .collect();
        for view in roots {
            let lane = derive_lane(&subtree_rows(&view));
            lanes[lane.index()].tasks.push(view);
        }
        lanes
    }
}

/// A task is a root when it has no parent, a missing or archived parent, a
/// self-link, or a parent that is itself a child. The last case turns a
/// hand-edited two-level chain into roots rather than losing either task.
fn is_root(open: &BTreeMap<&str, &Task>, id: &str) -> bool {
    let Some(task) = open.get(id) else {
        return false;
    };
    match task.parent.as_deref() {
        None => true,
        Some(parent) if parent == id => true,
        Some(parent) => match open.get(parent) {
            None => true,
            Some(parent_task) => parent_task.parent.is_some(),
        },
    }
}

fn compare_views(a: &TaskView, b: &TaskView) -> std::cmp::Ordering {
    a.title
        .to_lowercase()
        .cmp(&b.title.to_lowercase())
        .then_with(|| a.title.cmp(&b.title))
        .then_with(|| a.id.cmp(&b.id))
}

fn subtree_rows(view: &TaskView) -> Vec<WorkspaceRow> {
    let mut rows = view.workspaces.clone();
    for child in &view.children {
        rows.extend(subtree_rows(child));
    }
    rows
}

fn append_section(out: &mut String, heading: &str, rows: &[String]) {
    if rows.is_empty() {
        return;
    }
    out.push('\n');
    out.push_str(&format!("  {heading}\n"));
    for row in rows {
        out.push_str(row);
        out.push('\n');
    }
}

/// The display description and state: a park overrides the state text but the
/// jj state stays visible when it is not empty.
fn display(row: &WorkspaceRow) -> (String, String) {
    if let Some(wait) = &row.wait {
        let reason = if wait.reason.is_empty() {
            "(no reason given)".to_string()
        } else {
            wait.reason.clone()
        };
        let suffix = if wait.days > 0 {
            format!(" ({}d)", wait.days)
        } else {
            String::new()
        };
        let mut description = format!("parked: {reason}{suffix}");
        if row.state != State::Empty {
            description.push_str(&format!(" [jj: {}]", row.state.as_str()));
        }
        return ("waiting".to_string(), description);
    }
    let description = if row.description.is_empty() {
        "(no description set)".to_string()
    } else {
        row.description.clone()
    };
    (row.state.as_str().to_string(), description)
}

fn read_wait(config: &Config, workspace: &str, now: i64) -> Option<WaitInfo> {
    let body = std::fs::read_to_string(config.wait_marker(workspace)).ok()?;
    let mut lines = body.lines();
    let reason = lines.next().unwrap_or_default().to_string();
    let created = lines
        .next()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(0);
    let age = now - created;
    Some(WaitInfo {
        reason,
        age,
        days: age / 86400,
    })
}

fn read_clean(config: &Config, workspace: &str, now: i64) -> Option<CleanInfo> {
    let body = std::fs::read_to_string(config.clean_marker(workspace)).ok()?;
    let created = body
        .lines()
        .next()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(now);
    Some(CleanInfo { age: now - created })
}

#[derive(Default)]
struct SessionSet {
    dirs: HashSet<String>,
    meta: HashMap<String, SessionRow>,
}

fn load_sessions(
    provider: &dyn SessionProvider,
    want_meta: bool,
    age: i64,
    roots: &HashSet<String>,
) -> SessionSet {
    let mut set = SessionSet::default();
    if want_meta {
        for row in provider.recent_meta(age) {
            if roots.contains(&row.directory) {
                set.dirs.insert(row.directory.clone());
                set.meta.insert(row.directory.clone(), row);
            }
        }
    } else {
        for directory in provider.recent_dirs(age) {
            if roots.contains(&directory) {
                set.dirs.insert(directory);
            }
        }
    }
    set
}

#[derive(Default)]
pub struct ProcSnapshot {
    pids_by_dir: HashMap<PathBuf, Vec<u32>>,
}

impl ProcSnapshot {
    /// One /proc pass, indexed under every ancestor for gc or under the
    /// deepest workspace root for the board. An unreadable cwd is invisible,
    /// exactly as find's silent empty target is.
    pub fn scan(filter: Option<&HashSet<PathBuf>>) -> Self {
        let mut snapshot = Self::default();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return snapshot;
        };
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(cwd) = std::fs::read_link(entry.path().join("cwd")) else {
                continue;
            };
            match filter {
                Some(roots) => {
                    let mut deepest: Option<&PathBuf> = None;
                    for root in roots {
                        if cwd.starts_with(root)
                            && deepest.is_none_or(|current| {
                                root.as_os_str().len() > current.as_os_str().len()
                            })
                        {
                            deepest = Some(root);
                        }
                    }
                    if let Some(root) = deepest {
                        snapshot
                            .pids_by_dir
                            .entry(root.clone())
                            .or_default()
                            .push(pid);
                    }
                }
                None => {
                    let mut dir = Some(cwd.as_path());
                    while let Some(current) = dir {
                        if current != Path::new("/") {
                            snapshot
                                .pids_by_dir
                                .entry(current.to_path_buf())
                                .or_default()
                                .push(pid);
                        }
                        dir = current.parent();
                    }
                }
            }
        }
        snapshot
    }

    pub fn live(&self, dir: &Path, exclude: &Regex) -> bool {
        let Some(pids) = self.pids_by_dir.get(dir) else {
            return false;
        };
        for pid in pids {
            let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
                // The command line cannot be read; rookery never kills, so the
                // workspace counts as occupied.
                return true;
            };
            let command = raw
                .split(|byte| *byte == 0)
                .map(|part| String::from_utf8_lossy(part))
                .collect::<Vec<_>>()
                .join(" ");
            if exclude.is_match(&command) {
                continue;
            }
            return true;
        }
        false
    }
}

struct BoardCache {
    file: PathBuf,
    lock: PathBuf,
}

impl BoardCache {
    fn new(dir: &Path, repo_id: &str) -> Self {
        Self {
            file: dir.join(format!("board-{repo_id}")),
            lock: dir.join(format!("board-{repo_id}.lock")),
        }
    }

    fn load(&self, key: &str, names: &[&str]) -> Option<HashMap<String, State>> {
        let body = std::fs::read_to_string(&self.file).ok()?;
        let mut lines = body.lines();
        if lines.next()? != format!("v1 {key}") {
            return None;
        }
        let mut states = HashMap::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let (name, state) = line.split_once('\t')?;
            states.insert(name.to_string(), State::parse(state)?);
        }
        for name in names {
            states.get(*name)?;
        }
        Some(states)
    }

    fn store(&self, key: &str, names: &[&str], states: &HashMap<String, State>) {
        let Some(dir) = self.file.parent() else {
            return;
        };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let mut body = format!("v1 {key}\n");
        for name in names {
            let Some(state) = states.get(*name) else {
                return;
            };
            body.push_str(&format!("{name}\t{}\n", state.as_str()));
        }
        let Ok(mut temp) = tempfile::NamedTempFile::new_in(dir) else {
            return;
        };
        use std::io::Write;
        if temp.write_all(body.as_bytes()).is_err() {
            return;
        }
        let _ = temp.persist(&self.file);
    }

    /// Two popups classify at once at most; after two seconds the caller
    /// classifies without the lock and the atomic rename makes the duplicate
    /// write harmless.
    fn lock(&self) -> Option<File> {
        let dir = self.file.parent()?;
        if std::fs::create_dir_all(dir).is_err() || !is_writable(dir) {
            return None;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&self.lock)
            .ok()?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result == 0 {
                return Some(file);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn cache_key(
    op_id: &Option<String>,
    main_cid: &str,
    workspaces: &[WsEntry],
    roots: &[PathBuf],
) -> String {
    let mut body = format!(
        "op {}\nmain {}\n",
        op_id.as_deref().unwrap_or_default(),
        main_cid
    );
    for (index, entry) in workspaces.iter().enumerate() {
        body.push_str(&format!(
            "ws {}\t{}\t{}\n",
            entry.name,
            roots[index].to_string_lossy(),
            entry.target
        ));
    }
    sha256_hex(&body)
}

fn classify_cached(
    config: &Config,
    refresh: bool,
    workspaces: &[WsEntry],
    op_id: Option<&str>,
    key: &str,
    cache: &BoardCache,
) -> Result<HashMap<String, State>> {
    let names: Vec<&str> = workspaces.iter().map(|entry| entry.name.as_str()).collect();
    if !refresh && op_id.is_some() {
        if let Some(states) = cache.load(key, &names) {
            return Ok(states);
        }
    }
    if op_id.is_some() {
        if let Some(_lock) = cache.lock() {
            if !refresh {
                if let Some(states) = cache.load(key, &names) {
                    return Ok(states);
                }
            }
            let (states, ok) = classify(config, workspaces)?;
            if ok {
                cache.store(key, &names, &states);
            }
            return Ok(states);
        }
    }
    Ok(classify(config, workspaces)?.0)
}

fn classify(config: &Config, workspaces: &[WsEntry]) -> Result<(HashMap<String, State>, bool)> {
    let fleet = jj::board_scan(&config.trunk, config.board_jobs)?;
    let mut states = HashMap::new();
    let mut ok = true;
    for entry in workspaces {
        let target = if entry.target.is_empty() {
            None
        } else {
            Some(entry.target.as_str())
        };
        let Some((state, good)) = fleet.state_for(&entry.name, target) else {
            return Err(Error::message(format!(
                "rookery: cannot classify workspace '{}'",
                entry.name
            )));
        };
        states.insert(entry.name.clone(), state);
        ok = ok && good;
    }
    Ok((states, ok))
}

#[derive(Serialize)]
struct WorkspaceJson<'a> {
    task: Option<&'a str>,
    workspace: &'a str,
    path: String,
    state: &'static str,
    hold: &'static str,
    description: &'a str,
    session: Option<&'a SessionInfo>,
    wait: Option<WaitJson<'a>>,
    clean: Option<&'a CleanInfo>,
    age: i64,
}

#[derive(Serialize)]
struct WaitJson<'a> {
    reason: &'a str,
    age: i64,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lane {
    #[default]
    Backlog,
    Active,
    Review,
    Blocked,
    ToClean,
}

impl Lane {
    pub const ALL: [Lane; 5] = [
        Lane::Backlog,
        Lane::Active,
        Lane::Review,
        Lane::Blocked,
        Lane::ToClean,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Lane::Backlog => "Backlog",
            Lane::Active => "Active",
            Lane::Review => "Review",
            Lane::Blocked => "Blocked",
            Lane::ToClean => "To Clean",
        }
    }

    fn index(self) -> usize {
        match self {
            Lane::Backlog => 0,
            Lane::Active => 1,
            Lane::Review => 2,
            Lane::Blocked => 3,
            Lane::ToClean => 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskView {
    pub id: Option<String>,
    pub title: String,
    /// Set on a child card; a root has no parent (or a link the store dropped).
    pub parent: Option<String>,
    pub workspaces: Vec<WorkspaceRow>,
    /// One level of child task cards. Lanes are derived from the whole subtree.
    pub children: Vec<TaskView>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaneView {
    pub lane: Lane,
    pub tasks: Vec<TaskView>,
}

pub fn derive_lane(workspaces: &[WorkspaceRow]) -> Lane {
    if workspaces.is_empty() {
        return Lane::Backlog;
    }
    if workspaces.iter().all(|row| row.clean.is_some()) {
        return Lane::ToClean;
    }
    if workspaces
        .iter()
        .any(|row| row.wait.is_some() || row.state == State::Conflict)
    {
        return Lane::Blocked;
    }
    if workspaces
        .iter()
        .any(|row| row.hold == Hold::Live || row.state == State::Unpushed)
    {
        return Lane::Active;
    }
    Lane::Review
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, state: State) -> WorkspaceRow {
        WorkspaceRow {
            workspace: name.to_string(),
            state,
            ..WorkspaceRow::default()
        }
    }

    fn task(id: &str, title: &str, workspaces: Vec<&str>) -> Task {
        Task {
            id: id.to_string(),
            title: title.to_string(),
            status: "open".to_string(),
            workspaces: workspaces.into_iter().map(str::to_string).collect(),
            ..Task::default()
        }
    }

    #[test]
    fn lane_precedence() {
        assert_eq!(derive_lane(&[]), Lane::Backlog);
        let mut live = row("a", State::Empty);
        live.hold = Hold::Live;
        assert_eq!(derive_lane(&[live]), Lane::Active);
        assert_eq!(derive_lane(&[row("a", State::Unpushed)]), Lane::Active);
        let mut parked = row("a", State::Empty);
        parked.wait = Some(WaitInfo::default());
        assert_eq!(derive_lane(&[parked]), Lane::Blocked);
        assert_eq!(derive_lane(&[row("a", State::Conflict)]), Lane::Blocked);
        assert_eq!(derive_lane(&[row("a", State::Merged)]), Lane::Review);
        let mut clean = row("a", State::Merged);
        clean.clean = Some(CleanInfo::default());
        assert_eq!(derive_lane(&[clean]), Lane::ToClean);
        let mut both = row("a", State::Merged);
        both.clean = Some(CleanInfo::default());
        both.wait = Some(WaitInfo::default());
        assert_eq!(derive_lane(&[both]), Lane::ToClean);
    }

    #[test]
    fn a_child_travels_with_its_root_and_moves_the_root_lane() {
        let mut lead_ws = row("lead", State::Merged);
        lead_ws.task = Some("lead".to_string());
        let mut child_ws = row("child", State::Conflict);
        child_ws.task = Some("child".to_string());
        let child = Task {
            parent: Some("lead".to_string()),
            ..task("child", "The child", vec!["child"])
        };
        let board = Board {
            workspaces: vec![lead_ws, child_ws],
            tasks: vec![task("lead", "The lead", vec!["lead"]), child],
        };
        let lanes = board.lanes();
        assert_eq!(lanes.iter().map(|lane| lane.tasks.len()).sum::<usize>(), 1);
        let blocked = lanes
            .iter()
            .find(|lane| lane.lane == Lane::Blocked)
            .unwrap();
        assert_eq!(blocked.tasks.len(), 1);
        assert_eq!(blocked.tasks[0].id.as_deref(), Some("lead"));
        assert_eq!(blocked.tasks[0].children.len(), 1);
        assert_eq!(blocked.tasks[0].children[0].id.as_deref(), Some("child"));
    }

    #[test]
    fn a_child_of_a_missing_parent_becomes_a_root() {
        let mut child_ws = row("orphan", State::Unpushed);
        child_ws.task = Some("orphan".to_string());
        let board = Board {
            workspaces: vec![child_ws],
            tasks: vec![Task {
                parent: Some("gone".to_string()),
                ..task("orphan", "The orphan", vec!["orphan"])
            }],
        };
        let lanes = board.lanes();
        let active = lanes.iter().find(|lane| lane.lane == Lane::Active).unwrap();
        assert_eq!(active.tasks.len(), 1);
        assert_eq!(active.tasks[0].id.as_deref(), Some("orphan"));
    }

    #[test]
    fn lanes_group_tasks_and_keep_standalone_rows() {
        let mut grouped = row("ws-1", State::Unpushed);
        grouped.task = Some("task".to_string());
        let mut grouped2 = row("ws-2", State::Merged);
        grouped2.task = Some("task".to_string());
        let mut loose = row("loose", State::Unpushed);
        loose.description = "A loose seat".to_string();
        let board = Board {
            workspaces: vec![grouped, grouped2, loose],
            tasks: vec![task("task", "The task", vec!["ws-1", "ws-2"])],
        };
        let lanes = board.lanes();
        let active = lanes.iter().find(|lane| lane.lane == Lane::Active).unwrap();
        assert_eq!(active.tasks.len(), 2);
        let grouped = active.tasks.iter().find(|view| view.id.is_some()).unwrap();
        assert_eq!(grouped.workspaces.len(), 2);
        let loose = active.tasks.iter().find(|view| view.id.is_none()).unwrap();
        assert_eq!(loose.title, "A loose seat");
    }

    #[test]
    fn archived_tasks_are_hidden_and_primary_is_not_a_seat() {
        let board = Board {
            workspaces: vec![row("default", State::Empty)],
            tasks: vec![
                task("backlog", "Backlog task", vec![]),
                Task {
                    status: "archived".to_string(),
                    ..task("done", "Done task", vec![])
                },
            ],
        };
        let lanes = board.lanes();
        let backlog = lanes
            .iter()
            .find(|lane| lane.lane == Lane::Backlog)
            .unwrap();
        assert_eq!(backlog.tasks.len(), 1);
        assert!(
            lanes
                .iter()
                .all(|lane| lane.tasks.iter().all(|view| view.title != "Done task"))
        );
        assert!(lanes.iter().all(|lane| {
            lane.tasks
                .iter()
                .all(|view| view.workspaces.iter().all(|row| row.workspace != "default"))
        }));
    }

    #[test]
    fn counts_match_the_shell_semantics() {
        let mut alpha = row("alpha", State::Empty);
        alpha.hold = Hold::Session;
        let beta = row("beta", State::Empty);
        let mut parked = row("parked", State::Unpushed);
        parked.wait = Some(WaitInfo {
            reason: "blocked".to_string(),
            age: 86400 * 15,
            days: 15,
        });
        let board = Board {
            workspaces: vec![row("default", State::Empty), alpha, beta, parked],
            tasks: Vec::new(),
        };
        let counts = board.counts(14);
        assert_eq!(counts.total, 3);
        assert_eq!(counts.empty, 2);
        assert_eq!(counts.unpushed, 1);
        assert_eq!(counts.stale, 1);
        assert_eq!(counts.waiting, 1);
        assert_eq!(counts.waitold, 1);
        assert_eq!(board.render_counts(14), "3 2 0 1 0 0 1 1\n");
    }
}
