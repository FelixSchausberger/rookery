use crate::config::Config;
use crate::error::{Error, Result};
use crate::jj;
use crate::lifecycle::Line;
use crate::task::Store;
use crate::util::{now_secs, task_slug, valid_task};

/// The task verbs shared by the CLI and the board, so both frontends report
/// the same ids, refusals and marker writes.
pub fn task_add(config: &Config, title: &str, id: Option<&str>, notes: &str) -> Result<String> {
    if title.is_empty() {
        return Err(Error::usage("rookery task add: a title is required"));
    }
    let explicit = id.is_some();
    let base = id.map(str::to_string).unwrap_or_else(|| task_slug(title));
    if base.is_empty() {
        return Err(Error::message(
            "rookery task add: the title needs a letter or digit",
        ));
    }
    if !valid_task(&base) {
        return Err(Error::invalid(format!(
            "rookery task add: invalid id '{base}' (use [A-Za-z0-9._-])"
        )));
    }
    let store = Store::new(&config.base);
    if explicit && store.exists(&base)? {
        return Err(Error::invalid(format!(
            "rookery task add: task '{base}' already exists"
        )));
    }
    let mut attempt = base.clone();
    let mut number = 2;
    loop {
        if store.exists(&attempt)? {
            attempt = format!("{base}-{number}");
            number += 1;
            if number > 9 {
                return Err(Error::message(format!(
                    "rookery task add: no free id for '{base}'"
                )));
            }
            continue;
        }
        match store.add(&attempt, title, notes) {
            Ok(()) => return Ok(attempt),
            Err(Error::Refused(_)) => {
                attempt = format!("{base}-{number}");
                number += 1;
                if number > 9 {
                    return Err(Error::message(format!(
                        "rookery task add: no free id for '{base}'"
                    )));
                }
            }
            Err(err) => return Err(err),
        }
    }
}

pub fn task_assign(
    config: &Config,
    task: &str,
    workspace: &str,
    move_workspace: bool,
) -> Result<String> {
    if !valid_task(task) {
        return Err(Error::invalid(format!(
            "rookery task assign: invalid task '{task}'"
        )));
    }
    if !valid_task(workspace) {
        return Err(Error::invalid(format!(
            "rookery task assign: invalid workspace '{workspace}'"
        )));
    }
    if workspace == "default" {
        return Err(Error::message(
            "rookery task assign: the primary checkout is not a workspace to group",
        ));
    }
    if !jj::exists(workspace)? {
        return Err(Error::message(format!(
            "rookery task assign: no workspace named '{workspace}'"
        )));
    }
    Store::new(&config.base).assign(task, workspace, move_workspace)?;
    Ok(format!("rookery: '{workspace}' grouped under '{task}'"))
}

pub fn task_done(config: &Config, task: &str) -> Result<Vec<Line>> {
    let store = Store::new(&config.base);
    let state = store.read()?;
    let Some(target) = state.tasks.into_iter().find(|entry| entry.id == task) else {
        return Err(Error::message(format!(
            "rookery task done: no task named '{task}'"
        )));
    };
    if target.workspaces.is_empty() {
        return Err(Error::message(format!(
            "rookery task done: '{task}' has no workspaces; use 'rookery task archive {task}'"
        )));
    }
    let mut lines = Vec::new();
    for workspace in target.workspaces {
        if !jj::exists(&workspace)? {
            lines.push(Line {
                stderr: true,
                text: format!("rookery: skipping '{workspace}' (no such workspace)"),
            });
            continue;
        }
        let marker = config.clean_marker(&workspace);
        std::fs::write(&marker, format!("{}\n", now_secs()))
            .map_err(|err| Error::message(format!("rookery: cannot mark '{workspace}': {err}")))?;
        lines.push(Line {
            stderr: false,
            text: format!("rookery: '{workspace}' marked for cleanup"),
        });
    }
    Ok(lines)
}
