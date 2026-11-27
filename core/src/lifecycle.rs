use crate::board::ProcSnapshot;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::jj::{self, State};
use crate::session::{SessionProvider, SqliteSessions};
use crate::task::{self, Store};
use crate::util::{now_ms, now_secs, realpath_m, remove_with_rip, touched_since, valid_task};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug)]
pub struct Line {
    pub stderr: bool,
    pub text: String,
}

impl Line {
    fn out(text: impl Into<String>) -> Self {
        Self {
            stderr: false,
            text: text.into(),
        }
    }

    fn err(text: impl Into<String>) -> Self {
        Self {
            stderr: true,
            text: text.into(),
        }
    }
}

/// The terminal step of a create: the CLI execs OpenCode or starts a zellij
/// tab. Core never replaces the process itself, so the TUI can call it.
#[derive(Clone, Debug)]
pub struct Launch {
    pub dir: std::path::PathBuf,
    pub program: String,
    pub args: Vec<String>,
    pub zellij: bool,
    pub name: String,
    pub prelude: Vec<Line>,
}

fn require_valid(task: &str) -> Result<()> {
    if valid_task(task) {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "rookery: invalid task '{task}' (use [A-Za-z0-9._-])"
        )))
    }
}

fn require_workspace(name: &str) -> Result<()> {
    if jj::exists(name)? {
        Ok(())
    } else {
        Err(Error::message(format!(
            "rookery: no workspace named '{name}'"
        )))
    }
}

fn runner(config: &Config) -> String {
    config.agent_bin.clone()
}

pub fn create(
    config: &Config,
    task: &str,
    extra: &[String],
    new_tab: bool,
    warnings: &mut Vec<String>,
) -> Result<Launch> {
    require_valid(task)?;
    let dir = config.ws_dir(task);
    if jj::exists(task)? {
        return Err(Error::message(format!(
            "rookery: workspace '{task}' already exists - 'rookery rm {task}' to recreate it"
        )));
    }
    if dir.exists() {
        return Err(Error::message(format!(
            "rookery: path already exists: {}",
            dir.display()
        )));
    }

    let mut prelude = Vec::new();
    if !config.no_maint {
        if let Ok(lines) = gc(config, false, warnings) {
            prelude.extend(lines);
        }
    }
    run_hook(
        config,
        "pre_create",
        task,
        &dir,
        &config.hooks.pre_create,
        warnings,
    );
    if !config.no_maint {
        jj::fetch();
    }
    jj::add(&dir, task, &config.trunk)?;

    if let Some(assign) = &config.task_assign {
        let store = Store::new(&config.base);
        if store.assign(assign, task, false).is_err() {
            warnings.push(format!(
                "rookery: could not assign '{task}' to task '{assign}'"
            ));
        }
    }

    run_hook(
        config,
        "post_create",
        task,
        &dir,
        &config.hooks.post_create,
        warnings,
    );
    approve_envrc(config, &dir);
    tab_rename(task);

    Ok(Launch {
        dir,
        program: runner(config),
        args: extra.to_vec(),
        zellij: new_tab,
        name: task.to_string(),
        prelude,
    })
}

pub fn wait(config: &Config, task: &str, reason: &str) -> Result<String> {
    require_valid(task)?;
    require_workspace(task)?;
    if task == "default" {
        return Err(Error::message(
            "rookery wait: the primary checkout is the user's, not an agent workspace to park",
        ));
    }
    let marker = config.wait_marker(task);
    fs::write(&marker, format!("{reason}\n{}\n", now_secs()))
        .map_err(|err| Error::message(format!("rookery: cannot park '{task}': {err}")))?;
    let shown = if reason.is_empty() {
        "(no reason given)"
    } else {
        reason
    };
    Ok(format!(
        "rookery: '{task}' parked ({shown}) - gc keeps it; resume with 'rookery go {task}'"
    ))
}

pub fn go(config: &Config, task: &str) -> Result<String> {
    require_valid(task)?;
    require_workspace(task)?;
    if task == "default" {
        return Err(Error::message(
            "rookery go: the primary checkout is the user's, not an agent workspace",
        ));
    }
    let marker = config.wait_marker(task);
    if !marker.exists() {
        return Err(Error::message(format!("rookery: '{task}' is not parked")));
    }
    remove_with_rip(&marker)?;
    Ok(format!(
        "rookery: '{task}' resumed; gc reclassifies it on the next pass"
    ))
}

fn has_work(config: &Config, task: &str) -> bool {
    match jj::state(config, task, None) {
        Ok((state, _)) => state.has_work(),
        Err(_) => true,
    }
}

fn live_in(config: &Config, dir: &Path) -> bool {
    ProcSnapshot::scan(None).live(dir, &config.live_exclude)
}

pub fn done(
    config: &Config,
    task: &str,
    force: bool,
    warnings: &mut Vec<String>,
) -> Result<String> {
    require_valid(task)?;
    require_workspace(task)?;
    jj::fetch();
    if has_work(config, task) {
        let lines = jj::work_lines(config, task).unwrap_or_default();
        return Err(Error::message(format!(
            "rookery: '{task}' still has unmerged/uncommitted work; refusing to remove.\n{lines}Merge the PR, then retry; or 'rookery rm --force {task}' to discard."
        )));
    }
    let dir = realpath_m(&jj::ws_root_for(config, task)?);
    if !force && live_in(config, &dir) {
        return Err(Error::message(format!(
            "rookery: a process is still active in '{task}'; close it first, or 'rookery done --force {task}'."
        )));
    }
    rip_workspace(config, task, warnings)?;
    Ok(format!(
        "Done: forgot '{task}' (directory sent to the graveyard)."
    ))
}

pub fn rm(config: &Config, task: &str, force: bool, warnings: &mut Vec<String>) -> Result<String> {
    require_valid(task)?;
    require_workspace(task)?;
    if !force {
        if has_work(config, task) {
            return Err(Error::message(format!(
                "rookery: '{task}' has unmerged/uncommitted work; use 'rookery done {task}' after merging, or 'rookery rm --force {task}' to discard."
            )));
        }
        let dir = realpath_m(&jj::ws_root_for(config, task)?);
        if live_in(config, &dir) {
            return Err(Error::message(format!(
                "rookery: a process is still active in '{task}'; close it first, or 'rookery rm --force {task}'."
            )));
        }
    }
    rip_workspace(config, task, warnings)?;
    Ok(format!(
        "Forgot '{task}' (directory sent to the graveyard)."
    ))
}

/// Every removal path funnels through here, so a wait or clean marker cannot
/// outlive its workspace and a task row cannot keep a removed name.
pub fn rip_workspace(config: &Config, task: &str, warnings: &mut Vec<String>) -> Result<()> {
    let dir = realpath_m(&jj::ws_root_for(config, task)?);
    if dir == realpath_m(&config.root) {
        return Err(Error::message(format!(
            "rookery: refusing to remove the current workspace ('{task}')"
        )));
    }
    run_hook(
        config,
        "pre_delete",
        task,
        &dir,
        &config.hooks.pre_delete,
        warnings,
    );
    let wait = config.wait_marker(task);
    if wait.exists() {
        remove_with_rip(&wait)?;
    }
    let clean = config.clean_marker(task);
    if clean.exists() {
        remove_with_rip(&clean)?;
    }
    let store = Store::new(&config.base);
    let owner = store.ws_task(task);
    if owner.is_some() && store.drop_ws(task).is_err() {
        warnings.push(format!(
            "rookery: could not update the task store for '{task}'"
        ));
    }
    if let Some(id) = owner {
        match store.archive_if_empty_done(&id) {
            Ok(true) => warnings.push(format!(
                "rookery: task '{id}' archived (no workspaces remain)"
            )),
            Ok(false) => {}
            Err(_) => warnings.push(format!(
                "rookery: could not archive task '{id}' after removing '{task}'"
            )),
        }
    }
    jj::forget(task)?;
    if dir.is_dir() {
        remove_with_rip(&dir)?;
    }
    run_hook(
        config,
        "post_delete",
        task,
        &dir,
        &config.hooks.post_delete,
        warnings,
    );
    Ok(())
}

pub fn gc(config: &Config, fetch_first: bool, warnings: &mut Vec<String>) -> Result<Vec<Line>> {
    if !jj::trunk_exists(&config.trunk) {
        return Ok(vec![Line::err(format!(
            "rookery gc: {} not found; skipping",
            config.trunk
        ))]);
    }
    if fetch_first {
        jj::fetch();
    }
    let store = Store::new(&config.base);
    let open_tasks: HashMap<String, (String, i64)> = match store.read() {
        Ok(state) => {
            let mut open = HashMap::new();
            for task in state.tasks {
                if task.status != task::OPEN {
                    continue;
                }
                let id = task.id;
                let updated = task.updated;
                for workspace in task.workspaces {
                    open.insert(workspace, (id.clone(), updated));
                }
            }
            open
        }
        Err(_) => {
            return Ok(vec![Line::err(
                "rookery gc: task store is unreadable; skipping (unknown never means empty)",
            )]);
        }
    };
    let mut nudged: HashSet<String> = HashSet::new();
    let workspaces = jj::workspace_list()?;
    let known: HashSet<String> = workspaces.iter().map(|entry| entry.name.clone()).collect();
    let snapshot = ProcSnapshot::scan(None);
    let provider = SqliteSessions::new(config.session_db.clone());
    let sessions: HashSet<String> = provider
        .recent_dirs(config.gc_session_age)
        .into_iter()
        .collect();
    let now = now_secs();
    let stale_days = config.wait_stale_days;
    let mut lines = Vec::new();
    for entry in workspaces {
        if entry.name == "default" {
            continue;
        }
        let dir = realpath_m(entry.root.as_deref().unwrap_or(&config.ws_dir(&entry.name)));
        if dir == realpath_m(&config.root) {
            continue;
        }
        if !dir.is_dir() {
            rip_workspace(config, &entry.name, warnings)?;
            lines.push(Line::out(format!(
                "rookery gc: forgot '{}' (directory missing)",
                entry.name
            )));
            continue;
        }
        if let Some((reason, created)) = wait_marker(config, &entry.name) {
            let shown = if reason.is_empty() {
                "(no reason given)"
            } else {
                reason.as_str()
            };
            lines.push(Line::out(format!(
                "rookery gc: keeping '{}' (parked: {shown})",
                entry.name
            )));
            let days = (now - created) / 86400;
            if days >= stale_days {
                lines.push(Line::out(format!(
                    "rookery gc: '{}' has waited {days} days - resume it ('rookery go {}') or release the workspace",
                    entry.name, entry.name
                )));
            }
            continue;
        }
        // An open task is an explicit hold: the lead has not finished the
        // work, so gc must not reclaim a worker between reports even when the
        // workspace is momentarily empty.
        if let Some((id, updated)) = open_tasks.get(&entry.name) {
            lines.push(Line::out(format!(
                "rookery gc: keeping '{}' (task '{id}' open)",
                entry.name
            )));
            // A lead that disappeared must not hold workspaces silently. The
            // nudge is once per task, not once per grouped workspace.
            let days = (now - updated) / 86400;
            if days >= config.task_stale_days && nudged.insert(id.clone()) {
                lines.push(Line::out(format!(
                    "rookery gc: task '{id}' has been open {days} days - finish it ('rookery task done {id}') or record why with 'rookery task note {id} ...'"
                )));
            }
            continue;
        }
        let state = match jj::state(config, &entry.name, Some(&entry.description)) {
            Ok((state, description)) => (state, description),
            Err(_) => {
                lines.push(Line::err(format!(
                    "rookery gc: cannot classify '{}'; keeping it",
                    entry.name
                )));
                continue;
            }
        };
        let (state, description) = state;
        match state {
            State::Unpushed => {
                lines.push(Line::out(format!(
                    "rookery gc: keeping '{}' (unpushed work)",
                    entry.name
                )));
                continue;
            }
            State::InFlight => {
                lines.push(Line::out(format!(
                    "rookery gc: keeping '{}' (open PR)",
                    entry.name
                )));
                continue;
            }
            State::Conflict => {
                lines.push(Line::err(format!(
                    "rookery gc: keeping '{}' (conflict)",
                    entry.name
                )));
                continue;
            }
            State::Empty | State::Merged => {}
        }
        if snapshot.live(&dir, &config.live_exclude) {
            lines.push(Line::out(format!(
                "rookery gc: keeping '{}' (live process)",
                entry.name
            )));
            continue;
        }
        if sessions.contains(&dir.to_string_lossy().into_owned()) {
            lines.push(Line::out(format!(
                "rookery gc: keeping '{}' (recent session)",
                entry.name
            )));
            continue;
        }
        let age_limit = if state == State::Empty && description.is_empty() {
            config.gc_empty_age
        } else {
            config.gc_age
        };
        if touched_since(&dir, now - age_limit) {
            continue;
        }
        rip_workspace(config, &entry.name, warnings)?;
        lines.push(Line::out(format!(
            "rookery gc: forgot '{}' ({})",
            entry.name,
            state.as_str()
        )));
    }
    // A workspace forgotten by another path leaves its wait marker behind,
    // and `go` cannot clear it because the workspace is gone. The snapshot is
    // not authoritative: a workspace added after it must keep its fresh
    // marker, so each candidate is re-checked against jj before removal.
    for (name, marker) in stale_wait_markers(&config.base, &known) {
        if !matches!(jj::exists(&name), Ok(false)) {
            continue;
        }
        match remove_with_rip(&marker) {
            Ok(()) => lines.push(Line::out(format!(
                "rookery gc: dropped the stale wait marker for '{name}'"
            ))),
            Err(err) => lines.push(Line::err(format!(
                "rookery gc: cannot drop the stale wait marker for '{name}': {err}"
            ))),
        }
    }
    Ok(lines)
}

/// Wait markers whose workspace is not in the current jj list. Markers are
/// keyed by workspace name and live beside the workspace directories; a
/// marker with an invalid name or for a known workspace is not stale.
fn stale_wait_markers(base: &Path, known: &HashSet<String>) -> Vec<(String, PathBuf)> {
    let Ok(entries) = fs::read_dir(base) else {
        return Vec::new();
    };
    let mut stale = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".wait"))
        else {
            continue;
        };
        if !valid_task(name) || known.contains(name) {
            continue;
        }
        stale.push((name.to_string(), path));
    }
    stale.sort();
    stale
}

pub fn clean_mark(
    config: &Config,
    target: Option<&str>,
    unmark: bool,
    current: bool,
) -> Result<String> {
    let target = if current {
        if target.is_some() {
            return Err(Error::usage("rookery: --current takes no workspace"));
        }
        jj::workspace_at_cwd(config)?
            .ok_or_else(|| Error::message("rookery clean: not inside a registered workspace"))?
    } else {
        target
            .map(str::to_string)
            .ok_or_else(|| Error::usage("rookery clean: a workspace is required"))?
    };
    if !valid_task(&target) {
        return Err(Error::invalid(format!(
            "rookery clean: invalid workspace '{target}'"
        )));
    }
    if target == "default" {
        return Err(Error::message(
            "rookery clean: the primary checkout is the user's",
        ));
    }
    if !jj::exists(&target)? {
        return Err(Error::message(format!(
            "rookery clean: no workspace named '{target}'"
        )));
    }
    let marker = config.clean_marker(&target);
    if unmark {
        if !marker.exists() {
            return Err(Error::message(format!(
                "rookery clean: '{target}' is not marked"
            )));
        }
        remove_with_rip(&marker)?;
        Ok(format!("rookery: '{target}' unmarked"))
    } else {
        fs::write(&marker, format!("{}\n", now_secs()))
            .map_err(|err| Error::message(format!("rookery: cannot mark '{target}': {err}")))?;
        Ok(format!("rookery: '{target}' marked for cleanup"))
    }
}

pub fn clean_apply(config: &Config, warnings: &mut Vec<String>) -> Result<Vec<Line>> {
    let snapshot = ProcSnapshot::scan(None);
    let mut names: Vec<String> = Vec::new();
    if let Ok(entries) = fs::read_dir(&config.base) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".clean") else {
                continue;
            };
            if valid_task(stem) {
                names.push(stem.to_string());
            }
        }
    }
    names.sort();
    let store = Store::new(&config.base);
    let tasks = match store.read() {
        Ok(state) => state.tasks,
        Err(_) => {
            return Ok(vec![Line::err(
                "rookery clean: task store is unreadable; keeping every marked workspace",
            )]);
        }
    };
    let mut removed = 0;
    let mut kept = 0;
    let mut lines = Vec::new();
    for name in names {
        if !jj::exists(&name)? {
            remove_with_rip(&config.clean_marker(&name))?;
            lines.push(Line::out(format!(
                "rookery clean: dropped the stale mark for '{name}'"
            )));
            continue;
        }
        let dir = realpath_m(&jj::ws_root_for(config, &name)?);
        if dir == realpath_m(&config.root) {
            lines.push(Line::out(format!(
                "rookery clean: keeping '{name}' (current workspace)"
            )));
            kept += 1;
            continue;
        }
        if config.wait_marker(&name).exists() {
            lines.push(Line::out(format!(
                "rookery clean: keeping '{name}' (parked)"
            )));
            kept += 1;
            continue;
        }
        // A mark is consent to sweep, but not consent to close someone else's
        // open task. Only the lead's `task done` flips the status.
        if let Some(task) = tasks
            .iter()
            .find(|task| task.workspaces.iter().any(|workspace| workspace == &name))
        {
            if task.status == task::OPEN {
                lines.push(Line::out(format!(
                    "rookery clean: keeping '{name}' (task '{}' open)",
                    task.id
                )));
                kept += 1;
                continue;
            }
        }
        let state = match jj::state(config, &name, None) {
            Ok((state, _)) => state,
            Err(_) => {
                lines.push(Line::err(format!(
                    "rookery clean: cannot classify '{name}'; keeping it"
                )));
                kept += 1;
                continue;
            }
        };
        if state.has_work() {
            lines.push(Line::out(format!(
                "rookery clean: keeping '{name}' ({})",
                state.as_str()
            )));
            kept += 1;
            continue;
        }
        if snapshot.live(&dir, &config.live_exclude) {
            lines.push(Line::out(format!(
                "rookery clean: keeping '{name}' (live process)"
            )));
            kept += 1;
            continue;
        }
        rip_workspace(config, &name, warnings)?;
        lines.push(Line::out(format!(
            "rookery clean: removed '{name}' ({})",
            state.as_str()
        )));
        removed += 1;
    }
    lines.push(Line::out(format!(
        "rookery clean: {removed} removed, {kept} kept"
    )));
    Ok(lines)
}

/// A session id is used as an archive directory name, so it must not carry a
/// path separator or a traversal component.
fn valid_session_id(value: &str) -> bool {
    value.starts_with("ses_")
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn archive_slug(value: &str) -> String {
    let slug: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '-'
            }
        })
        .collect();
    if slug.is_empty() {
        "session".to_string()
    } else {
        slug
    }
}

/// Export every member of `top`'s tree into the archive directory named
/// `archive_id`, validating each export as JSON. Nothing is deleted until the
/// whole tree is readable, so a failed export keeps the conversation.
fn export_tree(
    config: &Config,
    provider: &SqliteSessions,
    top: &str,
    archive_id: &str,
) -> std::result::Result<(), String> {
    if !valid_session_id(top) || !valid_session_id(archive_id) {
        return Err("unexpected session id".to_string());
    }
    let members = provider.tree_members(top);
    if members.is_empty() {
        return Err("no readable members".to_string());
    }
    let archive = config.session_archive.join(archive_id);
    fs::create_dir_all(&archive)
        .map_err(|err| format!("cannot create {}: {err}", archive.display()))?;
    for (member, slug) in &members {
        if !valid_session_id(member) {
            return Err(format!("unexpected member id '{member}'"));
        }
        let output = Command::new(&config.agent_bin)
            .args(["session", "export", member])
            .output()
            .map_err(|err| format!("cannot run {}: {err}", config.agent_bin))?;
        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr);
            return Err(format!("export of {member} failed: {}", detail.trim()));
        }
        if serde_json::from_slice::<serde_json::Value>(&output.stdout).is_err() {
            return Err(format!("export of {member} is not JSON"));
        }
        let path = archive.join(format!("{member}-{}.json", archive_slug(slug)));
        fs::write(&path, &output.stdout)
            .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    }
    Ok(())
}

/// Prune child subagent transcripts and, only with `roots`, export and delete
/// top-level conversations whose working directory is gone. A top-level
/// conversation is the person's review record: the default pass never deletes
/// one. The export is written before any delete, and one failed member abandons
/// the whole tree, so a conversation is never deleted without a readable
/// archive.
pub fn reap(config: &Config, dry_run: bool, roots: bool) -> Result<Vec<Line>> {
    let mut lines = Vec::new();
    if !config.session_db.is_file() {
        lines.push(Line::out(format!(
            "rookery reap: no session database at {}",
            config.session_db.display()
        )));
        return Ok(lines);
    }
    let provider = SqliteSessions::new(config.session_db.clone());
    let top_level = provider.roots();
    if top_level.is_empty() {
        lines.push(Line::out("rookery reap: no top-level sessions to inspect"));
        return Ok(lines);
    }
    let top_count = top_level.len();

    let mut exported = 0;
    let mut kept = 0;
    let mut roots_deleted = 0;

    if !roots {
        let age = config.reap_child_age.max(0);
        let cutoff = now_ms() - age * 1000;
        let candidates = provider.prune_candidates(cutoff);
        lines.push(Line::out(format!(
            "rookery reap: pruning child subagent transcripts older than {age}s; top-level conversations are kept"
        )));
        for child in candidates {
            let label = child
                .title
                .as_deref()
                .unwrap_or(&child.slug)
                .replace('\n', " ");
            if dry_run {
                lines.push(Line::out(format!(
                    "rookery reap: would export and delete child '{}' ({label}) of '{}'",
                    child.id, child.root
                )));
                continue;
            }
            if let Err(reason) = export_tree(config, &provider, &child.id, &child.root) {
                kept += 1;
                lines.push(Line::err(format!(
                    "rookery reap: keeping child '{}' ({reason})",
                    child.id
                )));
                continue;
            }
            let deleted = Command::new(&config.agent_bin)
                .args(["session", "delete", &child.id])
                .status()
                .is_ok_and(|status| status.success());
            if deleted {
                exported += 1;
                lines.push(Line::out(format!(
                    "rookery reap: exported and deleted child '{}' ({label}) of '{}'",
                    child.id, child.root
                )));
            } else {
                kept += 1;
                lines.push(Line::err(format!(
                    "rookery reap: export kept for child '{}' but delete failed",
                    child.id
                )));
            }
        }
    } else {
        let workspaces: Vec<PathBuf> = jj::workspace_list()?
            .into_iter()
            .map(|entry| realpath_m(entry.root.as_deref().unwrap_or(&config.ws_dir(&entry.name))))
            .collect();
        lines.push(Line::out(
            "rookery reap: explicit --roots pass; live top-level conversations are kept",
        ));
        for root in &top_level {
            let label = root
                .title
                .as_deref()
                .unwrap_or("(no title)")
                .replace('\n', " ");
            if !valid_session_id(&root.id) {
                lines.push(Line::err(format!(
                    "rookery reap: keeping '{}' (unexpected session id)",
                    root.id
                )));
                kept += 1;
                continue;
            }
            let dir = realpath_m(Path::new(&root.directory));
            if dir.is_dir() {
                continue;
            }
            // A session started in a subdirectory of a live workspace is not
            // dead just because that subdirectory is gone.
            if workspaces
                .iter()
                .any(|workspace| dir == *workspace || dir.starts_with(workspace))
            {
                continue;
            }
            if dry_run {
                lines.push(Line::out(format!(
                    "rookery reap: would export and delete '{}' ({label})",
                    root.id
                )));
                continue;
            }
            if let Err(reason) = export_tree(config, &provider, &root.id, &root.id) {
                kept += 1;
                lines.push(Line::err(format!(
                    "rookery reap: keeping '{}' ({reason})",
                    root.id
                )));
                continue;
            }
            let deleted = Command::new(&config.agent_bin)
                .args(["session", "delete", &root.id])
                .status()
                .is_ok_and(|status| status.success());
            if deleted {
                exported += 1;
                roots_deleted += 1;
                lines.push(Line::out(format!(
                    "rookery reap: exported and deleted '{}' ({label})",
                    root.id
                )));
            } else {
                kept += 1;
                lines.push(Line::err(format!(
                    "rookery reap: export kept for '{}' but delete failed",
                    root.id
                )));
            }
        }
    }

    lines.push(Line::out(format!(
        "rookery reap: {exported} conversations exported and deleted, {kept} kept; {} top-level conversations kept",
        top_count - roots_deleted
    )));
    Ok(lines)
}

fn wait_marker(config: &Config, workspace: &str) -> Option<(String, i64)> {
    let body = fs::read_to_string(config.wait_marker(workspace)).ok()?;
    let mut lines = body.lines();
    let reason = lines.next().unwrap_or_default().to_string();
    let created = lines
        .next()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(0);
    Some((reason, created))
}

fn run_hook(
    config: &Config,
    name: &str,
    task: &str,
    dir: &Path,
    command: &str,
    warnings: &mut Vec<String>,
) {
    if command.is_empty() {
        return;
    }
    let status = Command::new("bash")
        .arg("-c")
        .arg(command)
        .env("ROOKERY_TASK", task)
        .env("ROOKERY_DIR", dir)
        .env("ROOKERY_ROOT", &config.root)
        .env("ROOKERY_BASE", &config.base)
        .env("OCWS_TASK", task)
        .env("OCWS_DIR", dir)
        .env("OCWS_ROOT", &config.root)
        .env("OCWS_BASE", &config.base)
        .status();
    match status {
        Ok(status) if status.success() => {}
        _ => warnings.push(format!("rookery: '{name}' hook failed")),
    }
}

pub fn approve_envrc(config: &Config, dir: &Path) {
    let local = dir.join(".envrc");
    if !local.is_file() {
        return;
    }
    let Ok(trunk) = jj::file_show(dir, &config.trunk, ".envrc") else {
        return;
    };
    let Ok(current) = fs::read(&local) else {
        return;
    };
    if trunk != current {
        return;
    }
    let _ = Command::new("direnv").arg("allow").arg(dir).status();
}

fn tab_rename(task: &str) {
    let Some(tab) = std::env::var("HERDR_TAB_ID")
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let _ = Command::new("herdr")
        .args(["tab", "rename", &tab, task])
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;

    #[test]
    fn stale_wait_markers_skip_known_and_foreign_names() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "kept.wait",
            "gone.wait",
            "gone.too.wait",
            "bad name.wait",
            "notes.txt",
        ] {
            fs::write(dir.path().join(name), "reason\n1\n").unwrap();
        }
        let known = HashSet::from(["kept".to_string(), "default".to_string()]);
        let stale: Vec<String> = stale_wait_markers(dir.path(), &known)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(stale, vec!["gone", "gone.too"]);
    }
}
