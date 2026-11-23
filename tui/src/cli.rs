// The CLI is the only data source and the only writer. The runner seam is the
// fake binary a test would put on PATH: it receives exactly the argv the TUI
// would exec, and the production runner spawns it.
use crate::model::Board;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

pub trait Runner: Send + Sync {
    fn run(&self, args: &[String]) -> Result<String, String>;
}

#[derive(Clone)]
pub struct Cli {
    runner: Arc<dyn Runner>,
}

impl Cli {
    /// Resolve the CLI once at startup. `ROOKERY_BIN` pins an absolute path
    /// (a wrapper from the broker module); otherwise the PATH name is used.
    pub fn system() -> Self {
        let program = std::env::var_os("ROOKERY_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("rookery"));
        Self {
            runner: Arc::new(ProcessRunner { program }),
        }
    }

    #[cfg(test)]
    pub fn with_runner(runner: Arc<dyn Runner>) -> Self {
        Self { runner }
    }

    /// One refresh is exactly two read-only calls: the workspace rows and the
    /// stored tasks. The TUI never reads the task store or the jj store.
    pub fn load(&self) -> Result<Board, String> {
        let ls = self.run(&["ls".to_string(), "--json".to_string()])?;
        let tasks = self.run(&["task".to_string(), "list".to_string(), "--json".to_string()])?;
        Board::parse(&ls, &tasks)
    }

    pub fn run(&self, args: &[String]) -> Result<String, String> {
        self.runner.run(args)
    }
}

pub struct ProcessRunner {
    pub program: PathBuf,
}

impl Runner for ProcessRunner {
    fn run(&self, args: &[String]) -> Result<String, String> {
        let output = Command::new(&self.program)
            .args(args)
            .output()
            .map_err(|err| format!("cannot run {}: {err}", self.program.display()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stderr.trim();
            return Err(if detail.is_empty() {
                format!("{} exited with {}", self.program.display(), output.status)
            } else {
                detail.to_string()
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Fake {
        calls: Mutex<Vec<Vec<String>>>,
        ls: String,
        tasks: String,
    }

    impl Runner for Fake {
        fn run(&self, args: &[String]) -> Result<String, String> {
            self.calls.lock().unwrap().push(args.to_vec());
            match args.first().map(String::as_str) {
                Some("ls") => Ok(self.ls.clone()),
                Some("task") => Ok(self.tasks.clone()),
                _ => Ok(String::new()),
            }
        }
    }

    #[test]
    fn load_reads_exactly_the_two_streams() {
        let fake = Arc::new(Fake {
            calls: Mutex::new(Vec::new()),
            ls: "{\"workspace\":\"a\",\"state\":\"empty\",\"hold\":\"-\"}\n".into(),
            tasks: "{\"id\":\"t\",\"title\":\"T\",\"status\":\"open\",\"workspaces\":[]}\n".into(),
        });
        let cli = Cli::with_runner(fake.clone());
        let board = cli.load().unwrap();
        assert_eq!(board.workspaces.len(), 1);
        assert_eq!(board.tasks.len(), 1);
        assert_eq!(
            fake.calls.lock().unwrap().as_slice(),
            [
                vec!["ls".to_string(), "--json".to_string()],
                vec!["task".to_string(), "list".to_string(), "--json".to_string()],
            ]
        );
    }

    #[test]
    fn process_runner_spawns_the_program() {
        let runner = ProcessRunner {
            program: PathBuf::from("echo"),
        };
        assert_eq!(runner.run(&["hello".to_string()]).unwrap(), "hello");
    }

    #[test]
    fn process_runner_reports_the_exit() {
        let runner = ProcessRunner {
            program: PathBuf::from("false"),
        };
        let err = runner.run(&[]).unwrap_err();
        assert!(err.contains("exited"), "unexpected error: {err}");
    }
}
