use crate::board::ProcSnapshot;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::jj;
use crate::lifecycle::approve_envrc;
use crate::task::Store;
use crate::util::{now_secs, realpath_m, shell_quote, task_slug, valid_task};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
struct Pane {
    tab_id: String,
    cwd: Option<String>,
    foreground_cwd: Option<String>,
}

#[derive(Clone, Debug)]
struct Agent {
    pane_id: String,
    tab_id: String,
    workspace_id: String,
    agent_status: String,
    session: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SeatInfo {
    pub task: String,
    pub dir: String,
    pub herdr_workspace: String,
    pub herdr_tab: String,
    pub herdr_pane: String,
    pub agent_session: String,
    pub agent_status: String,
    pub source: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct HerdrSeat {
    pub task: String,
    pub task_id: String,
    pub dir: String,
    pub label: String,
    pub herdr_workspace: String,
    pub herdr_tab: String,
    pub herdr_pane: String,
    pub agent_status: String,
    pub agent_session: String,
}

#[derive(Clone, Debug, Default)]
pub struct HerdrOptions {
    pub focus: bool,
    pub workspace: Option<String>,
    pub wait_secs: u64,
    pub task_id: Option<String>,
    pub task: Option<String>,
    pub extra: Vec<String>,
}

fn herdr_json(bin: &str, args: &[&str]) -> Result<serde_json::Value> {
    let output = Command::new(bin).args(args).output().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            Error::message(format!("herdr binary not found: {bin}"))
        } else {
            Error::message(format!("cannot run {bin}: {err}"))
        }
    })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(Error::message(format!(
            "cannot read herdr: {}",
            detail.trim()
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|err| Error::message(format!("cannot parse herdr output: {err}")))
}

fn pane_list(bin: &str) -> Result<Vec<Pane>> {
    let value = herdr_json(bin, &["pane", "list"])?;
    let panes = value
        .pointer("/result/panes")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(panes
        .into_iter()
        .map(|pane| Pane {
            tab_id: string_field(&pane, "tab_id"),
            cwd: optional_field(&pane, "cwd"),
            foreground_cwd: optional_field(&pane, "foreground_cwd"),
        })
        .collect())
}

fn agent_list(bin: &str) -> Result<Vec<Agent>> {
    let value = herdr_json(bin, &["agent", "list"])?;
    let agents = value
        .pointer("/result/agents")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(agents
        .into_iter()
        .map(|agent| Agent {
            pane_id: string_field(&agent, "pane_id"),
            tab_id: string_field(&agent, "tab_id"),
            workspace_id: string_field(&agent, "workspace_id"),
            agent_status: string_field(&agent, "agent_status"),
            session: agent
                .pointer("/agent_session/value")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string(),
        })
        .collect())
}

fn string_field(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string()
}

fn optional_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn pane_run(bin: &str, pane: &str, command: &str) -> Result<()> {
    // The pane's own output belongs on the caller's terminal, exactly as the
    // shell's `pane run` inherits it. Only the exit status is inspected.
    let status = Command::new(bin)
        .args(["pane", "run", pane, command])
        .status()
        .map_err(|err| Error::message(format!("cannot run {bin}: {err}")))?;
    if status.success() {
        return Ok(());
    }
    Err(Error::message("cannot run the pane"))
}

fn tab_focus(bin: &str, tab: &str) -> Result<()> {
    let output = Command::new(bin)
        .args(["tab", "focus", tab])
        .output()
        .map_err(|err| Error::message(format!("cannot run {bin}: {err}")))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    Err(Error::message(format!(
        "cannot focus tab {tab}: {}",
        detail.trim()
    )))
}

fn tab_rename(bin: &str, tab: &str, label: &str) {
    let _ = Command::new(bin)
        .args(["tab", "rename", tab, label])
        .output();
}

fn tab_create(
    bin: &str,
    cwd: &Path,
    label: &str,
    focus: bool,
    workspace: Option<&str>,
) -> Result<(String, String, String)> {
    let cwd = cwd.to_string_lossy().into_owned();
    let focus = if focus { "--focus" } else { "--no-focus" };
    let mut args = vec![
        "tab",
        "create",
        "--cwd",
        cwd.as_str(),
        "--label",
        label,
        focus,
    ];
    if let Some(workspace) = workspace {
        args.push("--workspace");
        args.push(workspace);
    }
    let value = herdr_json(bin, &args)?;
    let pane = string_field(
        value.pointer("/result/root_pane").unwrap_or(&value),
        "pane_id",
    );
    let tab = value
        .pointer("/result/tab")
        .map(|tab| string_field(tab, "tab_id"))
        .unwrap_or_default();
    let workspace = value
        .pointer("/result/tab")
        .and_then(|tab| tab.get("workspace_id"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    if pane.is_empty() {
        return Err(Error::message(format!(
            "herdr did not return a root pane: {value}"
        )));
    }
    if tab.is_empty() {
        return Err(Error::message(format!(
            "herdr did not return a tab: {value}"
        )));
    }
    Ok((pane, tab, workspace))
}

pub fn open(config: &Config, target: &str) -> Result<String> {
    if target == "default" {
        return Err(Error::message(
            "rookery open: the primary checkout is not a seat",
        ));
    }
    let workspace = if jj::exists(target)? {
        target.to_string()
    } else {
        let store = Store::new(&config.base);
        let snapshot = ProcSnapshot::scan(None);
        store
            .pick_workspace(
                target,
                |name| jj::exists(name).unwrap_or(false),
                |name| {
                    let dir = realpath_m(
                        &jj::ws_root_for(config, name).unwrap_or_else(|_| config.ws_dir(name)),
                    );
                    snapshot.live(&dir, &config.live_exclude)
                },
            )
            .ok_or_else(|| {
                Error::message(format!(
                    "rookery open: no workspace or task named '{target}'"
                ))
            })?
    };
    let dir = realpath_m(&jj::ws_root_for(config, &workspace)?);
    let panes = pane_list(&config.herdr_bin)
        .map_err(|err| Error::message(format!("rookery open: {err}")))?;
    let pane = panes
        .iter()
        .find(|pane| {
            let cwd = pane
                .foreground_cwd
                .as_deref()
                .or(pane.cwd.as_deref())
                .unwrap_or_default();
            let path = Path::new(cwd);
            path == dir || path.starts_with(&dir)
        })
        .ok_or_else(|| {
            Error::message(format!("rookery open: no herdr pane runs in '{workspace}'"))
        })?;
    if pane.tab_id.is_empty() {
        return Err(Error::message("rookery open: the pane has no tab id"));
    }
    tab_focus(&config.herdr_bin, &pane.tab_id)
        .map_err(|err| Error::message(format!("rookery open: {err}")))?;
    Ok(format!("rookery: opened '{workspace}'"))
}

pub fn seat(config: &Config) -> Result<SeatInfo> {
    let session = std::env::var("OPENCODE_SESSION_ID")
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            Error::message(
                "rookery seat: OPENCODE_SESSION_ID is unset; run this from an opencode tool call",
            )
        })?;
    let agents = agent_list(&config.herdr_bin)
        .map_err(|err| Error::message(format!("rookery seat: {err}")))?;
    let matches: Vec<&Agent> = agents
        .iter()
        .filter(|agent| agent.session == session)
        .collect();
    let agent = match matches.as_slice() {
        [agent] => *agent,
        [] => {
            return Err(Error::message(format!(
                "rookery seat: no herdr agent reports opencode session {session}\nrookery seat: check that tui.herdr.opencodeIntegration is on and the tab is a herdr pane"
            )));
        }
        many => {
            let mut message = format!(
                "rookery seat: session {session} is registered on more than one herdr pane; refusing to guess"
            );
            for agent in many {
                message.push_str(&format!(
                    "\n  {} {} {} {}",
                    agent.pane_id, agent.tab_id, agent.workspace_id, agent.agent_status
                ));
            }
            return Err(Error::message(message));
        }
    };
    let task = jj::workspace_at_cwd(config)?
        .ok_or_else(|| Error::message("rookery seat: not inside a registered jj workspace"))?;
    if task == "default" {
        return Err(Error::message(
            "rookery seat: the primary checkout is not a seat",
        ));
    }
    let dir = realpath_m(&jj::ws_root_for(config, &task)?);
    Ok(SeatInfo {
        task,
        dir: dir.to_string_lossy().into_owned(),
        herdr_workspace: agent.workspace_id.clone(),
        herdr_tab: agent.tab_id.clone(),
        herdr_pane: agent.pane_id.clone(),
        agent_session: session,
        agent_status: agent.agent_status.clone(),
        source: "agent-session".to_string(),
    })
}

/// Claim the current seat when its description is still the `jj workspace add`
/// placeholder. Returns None when the claim is not ours to replace.
pub fn title(config: &Config, text: &str, warnings: &mut Vec<String>) -> Result<Option<String>> {
    let task = jj::workspace_at_cwd(config)?
        .ok_or_else(|| Error::message("rookery title: not inside a registered workspace"))?;
    if task == "default" {
        return Err(Error::message(
            "rookery title: the primary checkout is not a seat",
        ));
    }
    let description = jj::current_description()?;
    let placeholder = description.split(": ").next().unwrap_or_default();
    let Some(rest) = placeholder.strip_prefix("t-") else {
        return Ok(None);
    };
    let (digits, suffix) = match rest.split_once('-') {
        Some((digits, suffix)) => (digits, Some(suffix)),
        None => (rest, None),
    };
    if !(4..=6).contains(&digits.len()) || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Ok(None);
    }
    if let Some(suffix) = suffix {
        if suffix.len() != 1 || !suffix.chars().all(|c| c.is_ascii_digit()) {
            return Ok(None);
        }
    }
    if !description.ends_with(&format!(": started from {}", config.trunk)) {
        return Ok(None);
    }
    let slug = task_slug(text);
    if slug.is_empty() {
        return Ok(None);
    }
    let names = jj::workspace_names()?;
    let mut final_name = slug.clone();
    let mut number = 2;
    while names.contains(&final_name) {
        final_name = format!("{slug}-{number}");
        number += 1;
        if number > 9 {
            break;
        }
    }
    // Rename before describing: a refused rename leaves the placeholder intact
    // and the next call retries the whole claim.
    jj::rename(&final_name)?;
    jj::describe(text)?;
    let store = Store::new(&config.base);
    if store.ws_task(&task).is_some() && store.rename_ws(&task, &final_name).is_err() {
        warnings.push("rookery: could not update the task store after the rename".to_string());
    }
    rename_tab(&final_name);
    Ok(Some(format!("rookery: claimed '{task}' as '{final_name}'")))
}

fn rename_tab(final_name: &str) {
    let Some(tab) = std::env::var("HERDR_TAB_ID")
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let Ok(panes) = pane_list("herdr") else {
        return;
    };
    let Some(pane) = panes.iter().find(|pane| pane.tab_id == tab) else {
        return;
    };
    let Some(cwd) = pane.foreground_cwd.as_deref().or(pane.cwd.as_deref()) else {
        return;
    };
    let here = realpath_m(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
    let cwd = realpath_m(Path::new(cwd));
    if cwd == here || cwd.starts_with(&here) {
        tab_rename("herdr", &tab, final_name);
    }
}

pub fn herdr(config: &Config, options: &HerdrOptions) -> Result<HerdrSeat> {
    let store = Store::new(&config.base);
    let task = if let Some(task_id) = &options.task_id {
        if !valid_task(task_id) {
            return Err(Error::invalid(format!(
                "rookery herdr: invalid task '{task_id}' (use [A-Za-z0-9._-])"
            )));
        }
        if !store.is_open(task_id).unwrap_or(false) {
            return Err(Error::message(format!(
                "rookery herdr: no open task named '{task_id}'"
            )));
        }
        if options.task.is_some() {
            return Err(Error::usage(
                "rookery herdr: --task takes no workspace name",
            ));
        }
        let mut name = task_id.clone();
        let mut number = 2;
        while jj::exists(&name)? {
            name = format!("{task_id}-{number}");
            number += 1;
            if number > 9 {
                return Err(Error::message(format!(
                    "rookery herdr: no free workspace name for task '{task_id}'"
                )));
            }
        }
        name
    } else if let Some(name) = &options.task {
        name.clone()
    } else {
        let stamp = now_secs().rem_euclid(86400);
        let mut name = format!(
            "t-{:02}{:02}{:02}",
            stamp / 3600,
            (stamp % 3600) / 60,
            stamp % 60
        );
        let mut number = 2;
        while jj::exists(&name)? {
            name = format!(
                "t-{:02}{:02}{:02}-{number}",
                stamp / 3600,
                (stamp % 3600) / 60,
                stamp % 60
            );
            number += 1;
            if number > 9 {
                break;
            }
        }
        name
    };
    if !valid_task(&task) {
        return Err(Error::invalid(format!(
            "rookery herdr: invalid task '{task}' (use [A-Za-z0-9._-])"
        )));
    }

    let tab_root = if jj::exists("default")? {
        realpath_m(&jj::ws_root_for(config, "default")?)
    } else {
        config.root.clone()
    };
    let tab_root = if tab_root.is_dir() {
        tab_root
    } else {
        config.root.clone()
    };
    approve_envrc(config, &tab_root);

    let program = if config.v2 { "opencode2" } else { "opencode" };
    let (dir, inner) = if jj::exists(&task)? {
        let dir = realpath_m(&jj::ws_root_for(config, &task)?);
        approve_envrc(config, &dir);
        let snapshot = ProcSnapshot::scan(None);
        if snapshot.live(&dir, &config.live_exclude) {
            return Err(Error::message(format!(
                "'{task}' already has a live session in {}; refusing a second agent in the same workspace",
                dir.display()
            )));
        }
        let mut inner = format!(
            "cd {} && exec {}",
            shell_quote(&dir.to_string_lossy()),
            shell_quote(program)
        );
        for argument in &options.extra {
            inner.push(' ');
            inner.push_str(&shell_quote(argument));
        }
        (dir, inner)
    } else {
        let dir = config.ws_dir(&task);
        let executable = config.rookery_bin.to_string_lossy().into_owned();
        let mut inner = String::from("ROOKERY_NO_MAINT=1");
        if let Some(task_id) = &options.task_id {
            inner.push_str(&format!(" ROOKERY_TASK_ASSIGN={}", shell_quote(task_id)));
        }
        inner.push(' ');
        inner.push_str(&shell_quote(&executable));
        if config.v2 {
            inner.push_str(" --v2");
        }
        inner.push(' ');
        inner.push_str(&shell_quote(&task));
        if !options.extra.is_empty() {
            inner.push_str(" --");
            for argument in &options.extra {
                inner.push(' ');
                inner.push_str(&shell_quote(argument));
            }
        }
        (dir, inner)
    };

    let (pane, tab, workspace) = tab_create(
        &config.herdr_bin,
        &tab_root,
        &task,
        options.focus,
        options.workspace.as_deref(),
    )
    .map_err(|err| Error::message(format!("rookery herdr: {err}")))?;
    if let Err(_err) = pane_run(&config.herdr_bin, &pane, &inner) {
        return Err(Error::message(format!(
            "rookery herdr: tab created, but the agent did not start; run 'rookery {task}' in it"
        )));
    }

    let mut agent_status = String::new();
    let mut agent_session = String::new();
    if options.wait_secs > 0 {
        let deadline = Instant::now() + Duration::from_secs(options.wait_secs);
        loop {
            let agents = agent_list(&config.herdr_bin).unwrap_or_default();
            if let Some(agent) = agents.iter().find(|agent| agent.pane_id == pane) {
                agent_status = agent.agent_status.clone();
                agent_session = agent.session.clone();
                match agent_status.as_str() {
                    "idle" | "done" => break,
                    "blocked" => {
                        return Err(Error::message(format!(
                            "rookery herdr: tab {tab} pane {pane} is blocked; read it with 'herdr agent read {pane} --source recent-unwrapped' and let the person answer"
                        )));
                    }
                    _ => {}
                }
            }
            if Instant::now() >= deadline {
                return Err(Error::message(format!(
                    "rookery herdr: tab {tab} pane {pane} did not reach an agent prompt in {}s; inspect it before prompting",
                    options.wait_secs
                )));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    let herdr_workspace = if workspace.is_empty() {
        options.workspace.clone().unwrap_or_default()
    } else {
        workspace
    };
    Ok(HerdrSeat {
        task: task.clone(),
        task_id: options.task_id.clone().unwrap_or_default(),
        dir: realpath_m(&dir).to_string_lossy().into_owned(),
        label: task,
        herdr_workspace,
        herdr_tab: tab,
        herdr_pane: pane,
        agent_status,
        agent_session,
    })
}
