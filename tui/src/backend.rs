// The TUI's only seam: a typed board source plus the six writes. The real
// backend calls rookery-core in process; tests substitute a fake.
use crate::model::Board;
use rookery_core::config::Config;
use rookery_core::seat::HerdrOptions;
use rookery_core::{board::Board as CoreBoard, lifecycle, ops, seat};

pub trait Backend: Send + Sync {
    fn load(&self) -> Result<Board, String>;
    fn open(&self, target: &str) -> Result<String, String>;
    fn mark_task(&self, task: &str) -> Result<String, String>;
    fn mark_workspace(&self, workspace: &str) -> Result<String, String>;
    fn start_seat(&self, task: &str) -> Result<(), String>;
    fn add_task(&self, title: &str) -> Result<String, String>;
    fn assign(&self, task: &str, workspace: &str) -> Result<String, String>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CoreBackend;

fn config() -> Result<Config, String> {
    Config::load(false).map_err(|err| err.to_string())
}

impl Backend for CoreBackend {
    fn load(&self) -> Result<Board, String> {
        CoreBoard::load(&config()?, false, true).map_err(|err| err.to_string())
    }

    fn open(&self, target: &str) -> Result<String, String> {
        seat::open(&config()?, target).map_err(|err| err.to_string())
    }

    fn mark_task(&self, task: &str) -> Result<String, String> {
        let lines = ops::task_done(&config()?, task).map_err(|err| err.to_string())?;
        Ok(lines
            .first()
            .map(|line| line.text.clone())
            .unwrap_or_default())
    }

    fn mark_workspace(&self, workspace: &str) -> Result<String, String> {
        lifecycle::clean_mark(&config()?, Some(workspace), false, false)
            .map_err(|err| err.to_string())
    }

    fn start_seat(&self, task: &str) -> Result<(), String> {
        let options = HerdrOptions {
            focus: false,
            wait_secs: 0,
            task_id: Some(task.to_string()),
            ..HerdrOptions::default()
        };
        seat::herdr(&config()?, &options)
            .map(|_| ())
            .map_err(|err| err.to_string())
    }

    fn add_task(&self, title: &str) -> Result<String, String> {
        let id = ops::task_add(&config()?, title, None, "").map_err(|err| err.to_string())?;
        Ok(format!("rookery: task '{id}' added ({title})"))
    }

    fn assign(&self, task: &str, workspace: &str) -> Result<String, String> {
        ops::task_assign(&config()?, task, workspace, false).map_err(|err| err.to_string())
    }
}
