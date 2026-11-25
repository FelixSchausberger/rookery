use crate::error::{Error, Result};
use crate::util::{home, is_writable, xdg_cache, xdg_data};
use regex::Regex;
use std::env;
use std::path::{Path, PathBuf};

/// The pre-rename names are read only when the new name is empty. The wrapper
/// pinning `ROOKERY_*` defaults therefore wins over an inherited `OCWS_*`.
const FALLBACK: &[&str] = &[
    "BASE",
    "WS_PREFIX",
    "TRUNK",
    "V2",
    "GC_AGE",
    "GC_EMPTY_AGE",
    "GC_SESSION_AGE",
    "WAIT_STALE_DAYS",
    "LIVE_EXCLUDE",
    "BOARD_JOBS",
    "NO_MAINT",
    "HOOK_PRE_CREATE",
    "HOOK_POST_CREATE",
    "HOOK_PRE_DELETE",
    "HOOK_POST_DELETE",
];

const DEFAULT_LIVE_EXCLUDE: &str = r"(^|/)[^ ]*(mcp-[a-z0-9._-]+-wrapped|nix-language-server)[^ ]*";

#[derive(Clone, Debug, Default)]
pub struct Hooks {
    pub pre_create: String,
    pub post_create: String,
    pub pre_delete: String,
    pub post_delete: String,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub root: PathBuf,
    pub base: PathBuf,
    pub ws_prefix: String,
    pub trunk: String,
    pub v2: bool,
    pub gc_age: i64,
    pub gc_empty_age: i64,
    pub gc_session_age: i64,
    pub wait_stale_days: i64,
    pub task_stale_days: i64,
    pub live_exclude: Regex,
    pub board_jobs: usize,
    pub no_maint: bool,
    pub task_assign: Option<String>,
    pub hooks: Hooks,
    pub session_db: PathBuf,
    pub cache_dir: PathBuf,
    pub herdr_bin: String,
    /// The CLI the herdr pane re-enters to create a seat. The CLI itself uses
    /// its own executable; the TUI wrapper points this at the configured CLI.
    pub rookery_bin: PathBuf,
}

impl Config {
    pub fn load(v2_flag: bool) -> Result<Self> {
        apply_fallbacks();
        let root = crate::jj::workspace_root()?;
        let base = resolve_base(&root)?;
        let live_exclude = build_live_exclude();
        Ok(Self {
            root,
            base,
            ws_prefix: env_nonempty("ROOKERY_WS_PREFIX").unwrap_or_else(|| "ws-".to_string()),
            trunk: env_nonempty("ROOKERY_TRUNK").unwrap_or_else(|| "main@origin".to_string()),
            v2: v2_flag || env_nonempty("ROOKERY_V2").as_deref() == Some("1"),
            gc_age: env_i64("ROOKERY_GC_AGE", 86400),
            gc_empty_age: env_i64("ROOKERY_GC_EMPTY_AGE", 21600),
            gc_session_age: env_i64("ROOKERY_GC_SESSION_AGE", 604800),
            wait_stale_days: env_i64("ROOKERY_WAIT_STALE_DAYS", 14),
            task_stale_days: env_i64("ROOKERY_TASK_STALE_DAYS", 14),
            live_exclude,
            board_jobs: board_jobs(),
            no_maint: env_nonempty("ROOKERY_NO_MAINT").as_deref() == Some("1"),
            task_assign: env_nonempty("ROOKERY_TASK_ASSIGN"),
            hooks: Hooks {
                pre_create: env_nonempty("ROOKERY_HOOK_PRE_CREATE").unwrap_or_default(),
                post_create: env_nonempty("ROOKERY_HOOK_POST_CREATE").unwrap_or_default(),
                pre_delete: env_nonempty("ROOKERY_HOOK_PRE_DELETE").unwrap_or_default(),
                post_delete: env_nonempty("ROOKERY_HOOK_POST_DELETE").unwrap_or_default(),
            },
            session_db: xdg_data("opencode").join("opencode.db"),
            cache_dir: xdg_cache("rookery"),
            herdr_bin: env_nonempty("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".to_string()),
            rookery_bin: env_nonempty("ROOKERY_BIN")
                .map(PathBuf::from)
                .or_else(|| std::env::current_exe().ok())
                .unwrap_or_else(|| PathBuf::from("rookery")),
        })
    }

    pub fn ws_dir(&self, task: &str) -> PathBuf {
        self.base.join(format!("{}{task}", self.ws_prefix))
    }

    pub fn wait_marker(&self, workspace: &str) -> PathBuf {
        self.base.join(format!("{workspace}.wait"))
    }

    pub fn clean_marker(&self, workspace: &str) -> PathBuf {
        self.base.join(format!("{workspace}.clean"))
    }
}

fn apply_fallbacks() {
    for name in FALLBACK {
        let new = format!("ROOKERY_{name}");
        let old = format!("OCWS_{name}");
        if env_nonempty(&new).is_some() {
            continue;
        }
        if let Some(value) = env_nonempty(&old) {
            // Edition 2021: set_var is safe here and the process is before threads.
            env::set_var(new, value);
        }
    }
}

fn env_nonempty(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn env_i64(name: &str, default: i64) -> i64 {
    env_nonempty(name)
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn board_jobs() -> usize {
    let mut jobs = env_nonempty("ROOKERY_BOARD_JOBS")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(8);
    if jobs < 1 {
        jobs = 8;
    }
    jobs.min(32)
}

fn build_live_exclude() -> Regex {
    let pattern = match env_nonempty("ROOKERY_LIVE_EXCLUDE") {
        Some(extra) => format!("{DEFAULT_LIVE_EXCLUDE}|{extra}"),
        None => DEFAULT_LIVE_EXCLUDE.to_string(),
    };
    Regex::new(&pattern).unwrap_or_else(|_| Regex::new(DEFAULT_LIVE_EXCLUDE).unwrap())
}

fn resolve_base(root: &Path) -> Result<PathBuf> {
    let configured = env_nonempty("ROOKERY_BASE");
    let mut base = match configured {
        Some(value) => PathBuf::from(value),
        None => {
            let candidate = root
                .parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/"));
            if is_writable(&candidate) {
                candidate
            } else {
                let fallback = xdg_data("ocws");
                std::fs::create_dir_all(&fallback).map_err(|err| {
                    Error::message(format!(
                        "rookery: cannot create the base directory '{}': {err}",
                        fallback.display()
                    ))
                })?;
                fallback
            }
        }
    };
    if !is_writable(&base) {
        base = PathBuf::from("/tmp/opencode");
    }
    std::fs::create_dir_all(&base).map_err(|err| {
        Error::message(format!(
            "rookery: cannot create the base directory '{}': {err}",
            base.display()
        ))
    })?;
    Ok(base)
}

/// Only used by tests and the CLI help; the process home is the same one the
/// shell defaults to.
#[allow(dead_code)]
pub fn home_dir() -> PathBuf {
    home()
}
