// The TUI's only seam: a typed board source plus the six writes. The real
// backend calls rookery-core in process; tests substitute a fake.
use crate::model::Board;
use rookery_core::config::Config;
use rookery_core::seat::HerdrOptions;
use rookery_core::{board::Board as CoreBoard, lifecycle, ops, seat};
use std::sync::OnceLock;

pub trait Backend: Send + Sync {
    fn load(&self) -> Result<Board, String>;
    fn open(&self, target: &str) -> Result<String, String>;
    fn mark_task(&self, task: &str) -> Result<String, String>;
    fn mark_workspace(&self, workspace: &str) -> Result<String, String>;
    fn start_seat(&self, task: &str) -> Result<(), String>;
    fn add_task(&self, title: &str) -> Result<String, String>;
    fn assign(&self, task: &str, workspace: &str) -> Result<String, String>;
    fn set_parent(&self, child: &str, parent: Option<&str>) -> Result<String, String>;
}

/// The backend caches the loaded config for the life of the board. `Config`
/// settles the repository root, base directory and environment once; reloading
/// it every two seconds only re-ran the jj root probe. A failed load is not
/// cached, so `r` still retries.
#[derive(Debug, Default)]
pub struct CoreBackend {
    config: OnceLock<Config>,
}

impl CoreBackend {
    fn config(&self) -> Result<Config, String> {
        if let Some(config) = self.config.get() {
            return Ok(config.clone());
        }
        let config = Config::load(false).map_err(|err| err.to_string())?;
        let _ = self.config.set(config.clone());
        Ok(config)
    }
}

impl Backend for CoreBackend {
    fn load(&self) -> Result<Board, String> {
        CoreBoard::load(&self.config()?, false, true).map_err(|err| err.to_string())
    }

    fn open(&self, target: &str) -> Result<String, String> {
        seat::open(&self.config()?, target).map_err(|err| err.to_string())
    }

    fn mark_task(&self, task: &str) -> Result<String, String> {
        let lines = ops::task_done(&self.config()?, task, false).map_err(|err| err.to_string())?;
        Ok(lines
            .first()
            .map(|line| line.text.clone())
            .unwrap_or_default())
    }

    fn mark_workspace(&self, workspace: &str) -> Result<String, String> {
        lifecycle::clean_mark(&self.config()?, Some(workspace), false, false)
            .map_err(|err| err.to_string())
    }

    fn start_seat(&self, task: &str) -> Result<(), String> {
        let options = HerdrOptions {
            focus: false,
            wait_secs: 0,
            task_id: Some(task.to_string()),
            ..HerdrOptions::default()
        };
        seat::herdr(&self.config()?, &options)
            .map(|_| ())
            .map_err(|err| err.to_string())
    }

    fn add_task(&self, title: &str) -> Result<String, String> {
        let id =
            ops::task_add(&self.config()?, title, None, "", None).map_err(|err| err.to_string())?;
        Ok(format!("rookery: task '{id}' added ({title})"))
    }

    fn assign(&self, task: &str, workspace: &str) -> Result<String, String> {
        ops::task_assign(&self.config()?, task, workspace, false).map_err(|err| err.to_string())
    }

    fn set_parent(&self, child: &str, parent: Option<&str>) -> Result<String, String> {
        ops::task_parent(&self.config()?, child, parent).map_err(|err| err.to_string())
    }
}
