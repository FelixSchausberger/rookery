// Data model for the board as the CLI reports it: workspace rows from
// `rookery ls --json` and task rows from `rookery task list --json`. Lanes are
// derived here and never stored.
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct Session {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub updated: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct Wait {
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub age: i64,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct Clean {
    #[serde(default)]
    pub age: i64,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct Workspace {
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub hold: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub session: Option<Session>,
    #[serde(default)]
    pub wait: Option<Wait>,
    #[serde(default)]
    pub clean: Option<Clean>,
    #[serde(default)]
    pub age: i64,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct Task {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub workspaces: Vec<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub created: i64,
    #[serde(default)]
    pub updated: i64,
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

/// One row group: a task with its workspaces, or a standalone workspace with no
/// task. The workspace itself is the execution; the task is only the grouping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskView {
    pub id: Option<String>,
    pub title: String,
    pub workspaces: Vec<Workspace>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaneView {
    pub lane: Lane,
    pub tasks: Vec<TaskView>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Board {
    pub workspaces: Vec<Workspace>,
    pub tasks: Vec<Task>,
}

impl Board {
    /// Parse the two NDJSON streams. An empty stream is an empty list; a
    /// malformed line aborts the refresh rather than showing a partial board.
    pub fn parse(ls: &str, tasks: &str) -> Result<Self, String> {
        Ok(Self {
            workspaces: parse_lines(ls, "ls --json")?,
            tasks: parse_lines(tasks, "task list --json")?,
        })
    }

    /// Group workspaces under their open tasks and bucket the groups into the
    /// five derived lanes. Archived tasks are not shown; a workspace whose task
    /// id is not an open task (a stale row, or no task at all) is its own row.
    pub fn lanes(&self) -> Vec<LaneView> {
        let open: BTreeMap<&str, &Task> = self
            .tasks
            .iter()
            .filter(|t| t.status == "open")
            .map(|t| (t.id.as_str(), t))
            .collect();
        let mut grouped: BTreeMap<String, Vec<Workspace>> = BTreeMap::new();
        let mut standalone: Vec<Workspace> = Vec::new();
        for ws in &self.workspaces {
            // The primary checkout is not a seat and never appears on the
            // board; `ls --json` includes it for the text board.
            if ws.workspace == "default" {
                continue;
            }
            match ws.task.as_deref() {
                Some(id) if open.contains_key(id) => {
                    grouped.entry(id.to_string()).or_default().push(ws.clone());
                }
                _ => standalone.push(ws.clone()),
            }
        }

        let mut views: Vec<TaskView> = Vec::new();
        for (id, task) in open.iter() {
            let mut workspaces = grouped.remove(*id).unwrap_or_default();
            workspaces.sort_by(|a, b| a.workspace.cmp(&b.workspace));
            views.push(TaskView {
                id: Some((*id).to_string()),
                title: task.title.clone(),
                workspaces,
            });
        }
        for ws in standalone {
            let title = ws
                .description
                .clone()
                .filter(|d| !d.trim().is_empty())
                .unwrap_or_else(|| ws.workspace.clone());
            views.push(TaskView {
                id: None,
                title,
                workspaces: vec![ws],
            });
        }

        let mut lanes: Vec<LaneView> = Lane::ALL
            .iter()
            .map(|lane| LaneView {
                lane: *lane,
                tasks: Vec::new(),
            })
            .collect();
        for view in views {
            let lane = derive_lane(&view.workspaces);
            lanes[lane.index()].tasks.push(view);
        }
        for lane in &mut lanes {
            lane.tasks.sort_by(|a, b| {
                a.title
                    .to_lowercase()
                    .cmp(&b.title.to_lowercase())
                    .then_with(|| a.title.cmp(&b.title))
                    .then_with(|| a.id.cmp(&b.id))
            });
        }
        lanes
    }
}

/// Lane precedence: a fully marked task is queued for cleanup even when some
/// of its work still blocks the sweep; a wait marker or a conflict blocks; live
/// or unpushed work is active; everything else is in review. Backlog is the
/// zero-workspace task.
pub fn derive_lane(workspaces: &[Workspace]) -> Lane {
    if workspaces.is_empty() {
        return Lane::Backlog;
    }
    if workspaces.iter().all(|w| w.clean.is_some()) {
        return Lane::ToClean;
    }
    if workspaces
        .iter()
        .any(|w| w.wait.is_some() || w.state == "conflict")
    {
        return Lane::Blocked;
    }
    if workspaces
        .iter()
        .any(|w| w.hold == "live" || w.state == "unpushed")
    {
        return Lane::Active;
    }
    Lane::Review
}

fn parse_lines<T: for<'de> Deserialize<'de>>(text: &str, what: &str) -> Result<Vec<T>, String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(|err| format!("{what}: {err}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(name: &str) -> Workspace {
        Workspace {
            workspace: name.to_string(),
            state: "empty".to_string(),
            hold: "-".to_string(),
            ..Workspace::default()
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
    fn parses_ndjson_and_rejects_a_bad_line() {
        let board = Board::parse(
            "{\"workspace\":\"a\",\"state\":\"empty\",\"hold\":\"-\"}\n\n",
            "",
        )
        .unwrap();
        assert_eq!(board.workspaces.len(), 1);
        assert!(Board::parse("{nope}\n", "").is_err());
    }

    #[test]
    fn lane_precedence() {
        assert_eq!(derive_lane(&[]), Lane::Backlog);

        let mut live = ws("a");
        live.hold = "live".into();
        assert_eq!(derive_lane(&[live]), Lane::Active);

        let mut dirty = ws("a");
        dirty.state = "unpushed".into();
        assert_eq!(derive_lane(&[dirty]), Lane::Active);

        let mut parked = ws("a");
        parked.wait = Some(Wait::default());
        assert_eq!(derive_lane(&[parked]), Lane::Blocked);

        let mut conflict = ws("a");
        conflict.state = "conflict".into();
        assert_eq!(derive_lane(&[conflict]), Lane::Blocked);

        let mut merged = ws("a");
        merged.state = "merged".into();
        assert_eq!(derive_lane(&[merged]), Lane::Review);

        let mut clean = ws("a");
        clean.clean = Some(Clean::default());
        assert_eq!(derive_lane(&[clean]), Lane::ToClean);

        // The cleanup mark outranks a still-blocking wait: the task is queued,
        // and the sweep decides whether it is safe.
        let mut both = ws("a");
        both.clean = Some(Clean::default());
        both.wait = Some(Wait::default());
        assert_eq!(derive_lane(&[both]), Lane::ToClean);
    }

    #[test]
    fn groups_workspaces_and_marks_standalone_rows() {
        let mut grouped = ws("ws-1");
        grouped.task = Some("task".into());
        grouped.state = "unpushed".into();
        let mut grouped2 = ws("ws-2");
        grouped2.task = Some("task".into());
        grouped2.state = "merged".into();
        let mut loose = ws("loose");
        loose.description = Some("A loose seat".into());
        loose.state = "unpushed".into();

        let board = Board {
            workspaces: vec![grouped, grouped2, loose],
            tasks: vec![task("task", "The task", vec!["ws-1", "ws-2"])],
        };
        let lanes = board.lanes();
        let active = lanes.iter().find(|l| l.lane == Lane::Active).unwrap();
        assert_eq!(active.tasks.len(), 2);
        let grouped = active.tasks.iter().find(|t| t.id.is_some()).unwrap();
        assert_eq!(grouped.title, "The task");
        assert_eq!(grouped.workspaces.len(), 2);
        let loose = active.tasks.iter().find(|t| t.id.is_none()).unwrap();
        assert_eq!(loose.title, "A loose seat");
        assert_eq!(loose.workspaces[0].workspace, "loose");
    }

    #[test]
    fn empty_task_is_backlog_and_archived_is_hidden() {
        let board = Board {
            workspaces: vec![ws("default")],
            tasks: vec![
                task("backlog", "Backlog task", vec![]),
                Task {
                    status: "archived".into(),
                    ..task("done", "Done task", vec![])
                },
            ],
        };
        let lanes = board.lanes();
        let backlog = lanes.iter().find(|l| l.lane == Lane::Backlog).unwrap();
        assert_eq!(backlog.tasks.len(), 1);
        assert_eq!(backlog.tasks[0].title, "Backlog task");
        assert!(
            lanes
                .iter()
                .all(|l| l.tasks.iter().all(|t| t.title != "Done task"))
        );
        // The primary checkout is not a seat and has no row anywhere.
        assert!(lanes.iter().all(|l| {
            l.tasks
                .iter()
                .all(|t| t.workspaces.iter().all(|w| w.workspace != "default"))
        }));
    }
}
