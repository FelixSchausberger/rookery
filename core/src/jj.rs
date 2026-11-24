use crate::config::Config;
use crate::error::{Error, Result};
use crate::util::{realpath_m, sha256_hex};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Empty,
    Merged,
    Unpushed,
    InFlight,
    Conflict,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Merged => "merged",
            Self::Unpushed => "unpushed",
            Self::InFlight => "in-flight",
            Self::Conflict => "conflict",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "empty" => Some(Self::Empty),
            "merged" => Some(Self::Merged),
            "unpushed" => Some(Self::Unpushed),
            "in-flight" => Some(Self::InFlight),
            "conflict" => Some(Self::Conflict),
            _ => None,
        }
    }

    pub fn has_work(self) -> bool {
        matches!(self, Self::Unpushed | Self::InFlight | Self::Conflict)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Flags {
    pub conflict: bool,
    pub empty: bool,
    pub remote: bool,
    pub contained: bool,
}

impl Flags {
    fn parse(value: &str) -> Self {
        Self {
            conflict: value.contains('C'),
            empty: value.contains('E'),
            remote: value.contains('R'),
            contained: value.contains('M'),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct WsEntry {
    pub name: String,
    pub root: Option<PathBuf>,
    pub target: String,
    pub stamp: i64,
    pub description: String,
}

pub struct FleetCommit {
    parents: Vec<String>,
    flags: Flags,
}

pub struct Fleet {
    commits: HashMap<String, FleetCommit>,
    absorbed: HashMap<String, i8>,
    targets: HashMap<String, String>,
}

impl Fleet {
    pub fn target_of(&self, name: &str) -> Option<&str> {
        self.targets.get(name).map(String::as_str)
    }

    /// The board's per-workspace walk: the target plus the off-trunk commits
    /// reachable from it, with a trunk commit included only as the target.
    pub fn state_for(&self, name: &str, target: Option<&str>) -> Option<(State, bool)> {
        let start = target
            .filter(|value| !value.is_empty())
            .or_else(|| self.target_of(name))?;
        let mut queue = VecDeque::from([start.to_string()]);
        let mut seen = HashSet::new();
        let mut lines: Vec<(String, Flags)> = Vec::new();
        while let Some(cid) = queue.pop_front() {
            let Some(commit) = self.commits.get(&cid) else {
                continue;
            };
            if !seen.insert(cid.clone()) {
                continue;
            }
            if cid != start && commit.flags.contained {
                continue;
            }
            lines.push((cid.clone(), commit.flags));
            if commit.flags.contained {
                continue;
            }
            for parent in &commit.parents {
                queue.push_back(parent.clone());
            }
        }
        if lines.is_empty() {
            return None;
        }
        Some(classify(
            lines.iter().map(|(cid, flags)| (cid.as_str(), *flags)),
            |cid| self.absorbed.get(cid).copied().unwrap_or(2),
        ))
    }
}

/// Shared by the direct per-workspace call and the fleet walk. The closure
/// returns 0 for absorbed, 1 for work and 2 for an unreadable absorption.
pub fn classify<'a, I, F>(items: I, mut absorbed: F) -> (State, bool)
where
    I: IntoIterator<Item = (&'a str, Flags)>,
    F: FnMut(&str) -> i8,
{
    let mut conflict = false;
    let mut has_work = false;
    let mut has_remote = false;
    let mut offmain = false;
    let mut ok = true;
    for (cid, flags) in items {
        if flags.conflict {
            conflict = true;
        }
        if flags.contained {
            continue;
        }
        if flags.empty {
            continue;
        }
        offmain = true;
        if flags.remote {
            has_remote = true;
        }
        match absorbed(cid) {
            0 => {}
            1 => has_work = true,
            _ => {
                has_work = true;
                ok = false;
            }
        }
    }
    let state = if conflict {
        State::Conflict
    } else if !offmain {
        State::Empty
    } else if !has_work {
        State::Merged
    } else if has_remote {
        State::InFlight
    } else {
        State::Unpushed
    };
    (state, ok)
}

pub fn run_in(cwd: Option<&Path>, args: &[&str], ignore: bool) -> Result<Output> {
    let mut command = Command::new("jj");
    if ignore {
        command.arg("--ignore-working-copy");
    }
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command
        .output()
        .map_err(|err| Error::message(format!("rookery: cannot run jj: {err}")))
}

pub fn stdout_in(cwd: Option<&Path>, args: &[&str], ignore: bool) -> Result<String> {
    let output = run_in(cwd, args, ignore)?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(Error::message(format!(
            "rookery: jj {}: {}",
            args.first().copied().unwrap_or(""),
            detail.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// jj's own narration belongs on the terminal for the mutating verbs, exactly
/// as the shell leaves it visible. Reads capture instead.
fn run_status(args: &[&str]) -> Result<()> {
    let status = Command::new("jj")
        .args(args)
        .status()
        .map_err(|err| Error::message(format!("rookery: cannot run jj: {err}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::message(format!(
            "rookery: jj {} failed",
            args.first().copied().unwrap_or("")
        )))
    }
}

pub fn try_stdout(args: &[&str], ignore: bool) -> Option<String> {
    let output = run_in(None, args, ignore).ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn workspace_root() -> Result<PathBuf> {
    let output = run_in(None, &["workspace", "root"], false)?;
    if !output.status.success() {
        return Err(Error::message("rookery: not inside a jj workspace"));
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let root = body.trim();
    if root.is_empty() {
        return Err(Error::message("rookery: not inside a jj workspace"));
    }
    Ok(realpath_m(Path::new(root)))
}

pub fn workspace_list() -> Result<Vec<WsEntry>> {
    let template = "name ++ \"\\t\" ++ root ++ \"\\t\" ++ target.commit_id() ++ \"\\t\" ++ target.committer().timestamp().format(\"%s\") ++ \"\\t\" ++ target.description().first_line() ++ \"\\n\"";
    let output = run_in(None, &["workspace", "list", "-T", template], true)?;
    if !output.status.success() {
        return Err(Error::message("rookery: cannot list jj workspaces"));
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let mut entries = Vec::new();
    for line in body.lines() {
        if line.is_empty() {
            continue;
        }
        let mut fields = line.splitn(5, '\t');
        let name = fields.next().unwrap_or_default().to_string();
        if name.is_empty() {
            continue;
        }
        let root = fields.next().unwrap_or_default();
        let target = fields.next().unwrap_or_default().to_string();
        let stamp = fields
            .next()
            .unwrap_or_default()
            .parse::<i64>()
            .unwrap_or(0);
        let description = fields.next().unwrap_or_default().to_string();
        let root = if root.is_empty() || root == "none" {
            None
        } else {
            Some(PathBuf::from(root))
        };
        entries.push(WsEntry {
            name,
            root,
            target,
            stamp,
            description,
        });
    }
    if entries.is_empty() {
        return Err(Error::message("rookery: no jj workspaces found"));
    }
    Ok(entries)
}

pub fn workspace_names() -> Result<Vec<String>> {
    let output = run_in(None, &["workspace", "list", "-T", "name ++ \"\\n\""], true)?;
    if !output.status.success() {
        return Err(Error::message("rookery: cannot list jj workspaces"));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

pub fn exists(name: &str) -> Result<bool> {
    Ok(workspace_names()?.iter().any(|entry| entry == name))
}

pub fn ws_root_for(config: &Config, name: &str) -> Result<PathBuf> {
    if let Some(entry) = workspace_list()?
        .into_iter()
        .find(|entry| entry.name == name)
    {
        if let Some(root) = entry.root {
            return Ok(root);
        }
    }
    Ok(config.ws_dir(name))
}

pub fn workspace_at_cwd(config: &Config) -> Result<Option<String>> {
    let here = realpath_m(&std::env::current_dir().map_err(|err| {
        Error::message(format!("rookery: cannot read the current directory: {err}"))
    })?);
    for entry in workspace_list()? {
        let root = realpath_m(entry.root.as_deref().unwrap_or(&config.ws_dir(&entry.name)));
        if here == root || here.starts_with(&root) {
            return Ok(Some(entry.name));
        }
    }
    Ok(None)
}

pub fn op_id() -> Option<String> {
    let body = try_stdout(&["op", "log", "--no-graph", "-n", "1", "-T", "id"], true)?;
    Some(body.trim().to_string())
}

pub fn trunk_commit(trunk: &str) -> Option<String> {
    let body = try_stdout(&["log", "--no-graph", "-r", trunk, "-T", "commit_id"], true)?;
    Some(body.trim().to_string())
}

pub fn repo_config_path() -> Option<PathBuf> {
    let body = try_stdout(&["config", "path", "--repo"], false)?;
    Some(PathBuf::from(body.trim()))
}

/// The cache identity is the repo-scoped config directory name, which every
/// workspace of the repository shares; the workspace root is not shared.
pub fn repo_id(root: &Path) -> String {
    if let Some(path) = repo_config_path() {
        let path = path.to_string_lossy();
        if let Some(stripped) = path.strip_suffix("/config.toml") {
            if let Some(id) = stripped.rsplit('/').next() {
                if !id.is_empty() {
                    return id.to_string();
                }
            }
        }
    }
    sha256_hex(&root.to_string_lossy())[..16].to_string()
}

pub fn add(path: &Path, name: &str, trunk: &str) -> Result<()> {
    let message = format!("{name}: started from {trunk}");
    run_status(&[
        "workspace",
        "add",
        &path.to_string_lossy(),
        "--name",
        name,
        "-r",
        trunk,
        "-m",
        &message,
    ])
}

pub fn forget(name: &str) -> Result<()> {
    run_status(&["workspace", "forget", name])
}

pub fn rename(name: &str) -> Result<()> {
    run_status(&["workspace", "rename", name])
}

pub fn describe(message: &str) -> Result<()> {
    run_status(&["describe", "-m", message])
}

pub fn current_description() -> Result<String> {
    let body = stdout_in(
        None,
        &[
            "log",
            "--no-graph",
            "-r",
            "@",
            "-T",
            "description.first_line()",
        ],
        true,
    )?;
    Ok(body.trim_end_matches('\n').to_string())
}

pub fn fetch() {
    let _ = run_in(None, &["git", "fetch"], false);
}

pub fn trunk_exists(trunk: &str) -> bool {
    run_in(None, &["log", "-r", trunk, "-n", "0"], true)
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub fn state(
    config: &Config,
    name: &str,
    known_description: Option<&str>,
) -> Result<(State, String)> {
    let trunk = &config.trunk;
    let rev = format!("{name}@");
    let description = match known_description {
        Some(value) => value.to_string(),
        None => stdout_in(
            None,
            &[
                "log",
                "--no-graph",
                "-r",
                &rev,
                "-T",
                "description.first_line()",
            ],
            true,
        )?
        .trim_end_matches('\n')
        .to_string(),
    };
    let template = format!(
        "commit_id ++ \" \" ++ if(self.conflict(), \"C\", \"-\") ++ if(self.empty(), \"E\", \"-\") ++ if(self.remote_bookmarks(), \"R\", \"-\") ++ if(self.contained_in(\"::{trunk}\"), \"M\", \"-\") ++ \"\\n\""
    );
    let revset = format!("{rev} | (ancestors({rev}) ~ ::{trunk})");
    let body = stdout_in(
        None,
        &["log", "--no-graph", "-r", &revset, "-T", &template],
        true,
    )?;
    let mut lines: Vec<(String, Flags)> = Vec::new();
    for line in body.lines() {
        if line.is_empty() {
            continue;
        }
        let Some((cid, flags)) = line.split_once(' ') else {
            continue;
        };
        lines.push((cid.to_string(), Flags::parse(flags)));
    }
    if lines.is_empty() {
        return Err(Error::message(format!(
            "rookery: cannot classify workspace '{name}'"
        )));
    }
    let mut error: Option<Error> = None;
    let (state, _ok) = classify(
        lines.iter().map(|(cid, flags)| (cid.as_str(), *flags)),
        |cid| match absorbed(cid, &config.trunk) {
            Ok(true) => 0,
            Ok(false) => 1,
            Err(err) => {
                error = Some(err);
                2
            }
        },
    );
    if let Some(err) = error.take() {
        return Err(err);
    }
    Ok((state, description))
}

/// True when every file the commit touches has its content on the trunk.
/// Exit 2 in the shell (an unreadable check) is an Err here; callers treat it
/// as work still present and refuse to cache the guessed state.
pub fn absorbed(commit: &str, trunk: &str) -> Result<bool> {
    let files = stdout_in(None, &["diff", "-r", commit, "--name-only"], true)?;
    if files.trim().is_empty() {
        return Ok(true);
    }
    let mut args: Vec<&str> = vec!["diff", "--from", trunk, "--to", commit, "--"];
    let paths: Vec<&str> = files.lines().filter(|line| !line.is_empty()).collect();
    args.extend(paths.iter().copied());
    let diff = stdout_in(None, &args, true)?;
    Ok(diff.is_empty())
}

pub fn board_scan(trunk: &str, jobs: usize) -> Result<Fleet> {
    let template = format!(
        "commit_id ++ \"\\t\" ++ self.parents().map(|p| p.commit_id()).join(\",\") ++ \"\\t\" ++ if(self.conflict(), \"C\", \"-\") ++ if(self.empty(), \"E\", \"-\") ++ if(self.remote_bookmarks(), \"R\", \"-\") ++ if(self.contained_in(\"::{trunk}\"), \"M\", \"-\") ++ \"\\t\" ++ self.working_copies().map(|w| w.name()).join(\",\") ++ \"\\n\""
    );
    let revset = format!("working_copies() | ({trunk}..working_copies())");
    let body = stdout_in(
        None,
        &["log", "--no-graph", "-r", &revset, "-T", &template],
        true,
    )
    .map_err(|_| Error::message("rookery: cannot classify workspaces"))?;
    let mut commits = HashMap::new();
    let mut targets: HashMap<String, String> = HashMap::new();
    let mut pending: Vec<String> = Vec::new();
    for line in body.lines() {
        if line.is_empty() {
            continue;
        }
        let mut fields = line.splitn(4, '\t');
        let cid = fields.next().unwrap_or_default().to_string();
        if cid.is_empty() {
            continue;
        }
        let parents = fields
            .next()
            .unwrap_or_default()
            .split(',')
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect();
        let flags = Flags::parse(fields.next().unwrap_or_default());
        let names: Vec<String> = fields
            .next()
            .unwrap_or_default()
            .split(',')
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect();
        for name in &names {
            targets.insert(name.clone(), cid.clone());
        }
        if !flags.contained && !flags.empty {
            pending.push(cid.clone());
        }
        commits.insert(cid, FleetCommit { parents, flags });
    }
    let mut absorbed_states = HashMap::new();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs.clamp(1, 32))
        .build()
        .ok();
    let results: Vec<(String, i8)> = match pool {
        Some(pool) => pool.install(|| {
            pending
                .par_iter()
                .map(|cid| {
                    let status = match absorbed(cid, trunk) {
                        Ok(true) => 0,
                        Ok(false) => 1,
                        Err(_) => 2,
                    };
                    (cid.clone(), status)
                })
                .collect()
        }),
        None => pending
            .iter()
            .map(|cid| {
                let status = match absorbed(cid, trunk) {
                    Ok(true) => 0,
                    Ok(false) => 1,
                    Err(_) => 2,
                };
                (cid.clone(), status)
            })
            .collect(),
    };
    for (cid, status) in results {
        absorbed_states.insert(cid, status);
    }
    Ok(Fleet {
        commits,
        absorbed: absorbed_states,
        targets,
    })
}

pub fn work_lines(config: &Config, name: &str) -> Result<String> {
    let revset = format!("ancestors({name}@) ~ ::{} ~ empty()", config.trunk);
    stdout_in(
        None,
        &[
            "log",
            "--no-graph",
            "-r",
            &revset,
            "-T",
            "\"  \" ++ commit_id.short() ++ \" \" ++ description.first_line() ++ \"\\n\"",
        ],
        true,
    )
}

pub fn file_show(cwd: &Path, trunk: &str, path: &str) -> Result<Vec<u8>> {
    let output = run_in(Some(cwd), &["file", "show", "-r", trunk, path], true)?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(Error::message(format!(
            "rookery: cannot read '{path}': {}",
            detail.trim()
        )));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(value: &str) -> Flags {
        Flags::parse(value)
    }

    #[test]
    fn state_precedence_matches_the_shell() {
        let cases = [
            ("-E--", 0, State::Empty),
            ("C---", 0, State::Conflict),
            ("---M", 0, State::Empty),
            ("-ER-", 0, State::Empty),
            ("----", 0, State::Merged),
            ("----", 1, State::Unpushed),
            ("--R-", 1, State::InFlight),
        ];
        for (raw, work, expected) in cases {
            let (state, ok) = classify([("cid", flags(raw))], |_| work);
            assert_eq!(state, expected, "flags {raw} work {work}");
            assert!(ok);
        }
    }

    #[test]
    fn unreadable_absorption_is_work_and_never_ok() {
        let (state, ok) = classify([("cid", flags("----"))], |_| 2);
        assert_eq!(state, State::Unpushed);
        assert!(!ok);
    }

    #[test]
    fn contained_target_stops_content_but_keeps_conflicts() {
        let (state, _) = classify([("cid", flags("C--M"))], |_| 0);
        assert_eq!(state, State::Conflict);
    }

    #[test]
    fn fleet_walk_skips_parents_of_the_target() {
        let fleet = Fleet {
            commits: HashMap::from([
                (
                    "start".to_string(),
                    FleetCommit {
                        parents: vec!["trunk-parent".to_string()],
                        flags: flags("-E--"),
                    },
                ),
                (
                    "trunk-parent".to_string(),
                    FleetCommit {
                        parents: vec![],
                        flags: flags("---M"),
                    },
                ),
            ]),
            absorbed: HashMap::new(),
            targets: HashMap::from([("alpha".to_string(), "start".to_string())]),
        };
        let (state, ok) = fleet.state_for("alpha", None).unwrap();
        assert_eq!(state, State::Empty);
        assert!(ok);
    }
}
