// Board state and key handling. The TUI owns only a cached view: every write
// becomes an Action whose argv the runtime hands to the CLI.
use crate::model::{Board, TaskView, Workspace};
use crate::view::{self, CardKey, LaneModel};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rookery_core::util::now_secs;

pub const HINTS: &str = "enter open  d done  n new  s seat  a assign  r refresh  ? help  q quit";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Quit,
    Refresh,
    Open(String),
    MarkTask(String),
    MarkWorkspace(String),
    StartSeat(String),
    AddTask(String),
    AssignTask { task: String, workspace: String },
}

impl Action {
    pub fn label(&self) -> Option<&'static str> {
        match self {
            Action::Quit | Action::Refresh => None,
            Action::Open(_) => Some("opened"),
            Action::MarkTask(_) | Action::MarkWorkspace(_) => Some("marked"),
            Action::StartSeat(_) => Some("seat"),
            Action::AddTask(_) => Some("added"),
            Action::AssignTask { .. } => Some("assigned"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub lane: usize,
    pub card: usize,
    pub child: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Normal,
    Help,
    TaskInput {
        text: String,
    },
    Assign {
        workspace: String,
        selected: usize,
    },
}

#[derive(Debug, Clone, Default)]
pub struct App {
    pub board: Board,
    pub lanes: Vec<LaneModel>,
    pub cursor: Cursor,
    pub mode: Mode,
    pub status: String,
    pub error: bool,
    pub now: i64,
    load_error: bool,
}

impl App {
    pub fn new(board: Board) -> Self {
        Self::new_at(board, now_secs())
    }

    pub fn new_at(board: Board, now: i64) -> Self {
        let lanes = view::build(&board);
        let mut app = Self {
            board,
            lanes,
            now,
            ..Self::default()
        };
        app.clamp();
        app
    }

    pub fn set_board(&mut self, board: Board) {
        self.set_board_at(board, now_secs());
    }

    pub fn set_board_at(&mut self, board: Board, now: i64) {
        let key = self.selected_key();
        let child = self.selected_child_name();
        let lane = self.cursor.lane;
        let card = self.cursor.card;
        self.lanes = view::build(&board);
        self.board = board;
        self.now = now;
        if self.load_error {
            self.status.clear();
            self.error = false;
            self.load_error = false;
        }
        self.restore_selection(key, child, lane, card);
    }

    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
        self.error = false;
        self.load_error = false;
    }

    pub fn set_error(&mut self, error: impl Into<String>) {
        self.status = error.into();
        self.error = true;
        self.load_error = false;
    }

    /// A failed refresh: the last board stays visible, and the next successful
    /// refresh clears this message. An action failure uses `set_error` and is
    /// kept until the next action or status.
    pub fn set_load_error(&mut self, error: impl Into<String>) {
        self.status = error.into();
        self.error = true;
        self.load_error = true;
    }

    pub fn selected_card(&self) -> Option<&view::Card> {
        self.lanes
            .get(self.cursor.lane)
            .and_then(|lane| lane.cards.get(self.cursor.card))
    }

    pub fn selected_task(&self) -> Option<&TaskView> {
        self.selected_card().map(|card| &card.view)
    }

    pub fn selected_child(&self) -> Option<&Workspace> {
        let child = self.cursor.child?;
        self.selected_task()
            .and_then(|task| task.workspaces.get(child))
    }

    pub fn open_tasks(&self) -> Vec<(&str, &str)> {
        let mut tasks: Vec<(&str, &str)> = self
            .board
            .tasks
            .iter()
            .filter(|task| task.status == "open")
            .map(|task| (task.id.as_str(), task.title.as_str()))
            .collect();
        tasks.sort_by_key(|a| a.1.to_lowercase());
        tasks
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Action::Quit);
        }
        if !matches!(self.mode, Mode::Normal) {
            return self.handle_modal(key);
        }
        match key.code {
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Char('?') => {
                self.mode = Mode::Help;
                None
            }
            KeyCode::Char('r') => Some(Action::Refresh),
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_down();
                None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_up();
                None
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.move_lane(-1);
                None
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.move_lane(1);
                None
            }
            KeyCode::Char('n') => {
                self.mode = Mode::TaskInput {
                    text: String::new(),
                };
                None
            }
            KeyCode::Char('a') => self.start_assign(),
            KeyCode::Char('d') => self.mark_selected(),
            KeyCode::Char('s') => self.seat_selected(),
            KeyCode::Enter => self.open_selected(),
            _ => None,
        }
    }

    fn handle_modal(&mut self, key: KeyEvent) -> Option<Action> {
        let mode = std::mem::take(&mut self.mode);
        match mode {
            Mode::Help => None,
            Mode::TaskInput { mut text } => match key.code {
                KeyCode::Esc => None,
                KeyCode::Enter => {
                    let title = text.trim().to_string();
                    if title.is_empty() {
                        self.set_error("a task needs a title");
                        None
                    } else {
                        Some(Action::AddTask(title))
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.mode = Mode::TaskInput { text };
                    None
                }
                KeyCode::Char(c) => {
                    text.push(c);
                    self.mode = Mode::TaskInput { text };
                    None
                }
                _ => {
                    self.mode = Mode::TaskInput { text };
                    None
                }
            },
            Mode::Assign {
                workspace,
                mut selected,
            } => match key.code {
                KeyCode::Esc => None,
                KeyCode::Char('j') | KeyCode::Down => {
                    let len = self.open_tasks().len();
                    if len > 0 {
                        selected = (selected + 1).min(len - 1);
                    }
                    self.mode = Mode::Assign {
                        workspace,
                        selected,
                    };
                    None
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                    self.mode = Mode::Assign {
                        workspace,
                        selected,
                    };
                    None
                }
                KeyCode::Enter => {
                    let tasks = self.open_tasks();
                    match tasks.get(selected) {
                        Some((id, _)) => Some(Action::AssignTask {
                            task: (*id).to_string(),
                            workspace,
                        }),
                        None => {
                            self.set_error("no open tasks");
                            None
                        }
                    }
                }
                _ => {
                    self.mode = Mode::Assign {
                        workspace,
                        selected,
                    };
                    None
                }
            },
            Mode::Normal => None,
        }
    }

    fn move_down(&mut self) {
        let Some(lane) = self.lanes.get(self.cursor.lane) else {
            return;
        };
        if lane.cards.is_empty() {
            return;
        }
        let card = self.cursor.card.min(lane.cards.len() - 1);
        match self.cursor.child {
            None => {
                if !lane.cards[card].workspaces().is_empty() {
                    self.cursor.child = Some(0);
                } else if card + 1 < lane.cards.len() {
                    self.cursor.card = card + 1;
                }
            }
            Some(child) => {
                if child + 1 < lane.cards[card].workspaces().len() {
                    self.cursor.child = Some(child + 1);
                } else if card + 1 < lane.cards.len() {
                    self.cursor.card = card + 1;
                    self.cursor.child = None;
                }
            }
        }
    }

    fn move_up(&mut self) {
        match self.cursor.child {
            Some(0) => self.cursor.child = None,
            Some(child) => self.cursor.child = Some(child - 1),
            None if self.cursor.card > 0 => {
                self.cursor.card -= 1;
                self.cursor.child = None;
            }
            None => {}
        }
    }

    fn move_lane(&mut self, delta: isize) {
        if self.lanes.is_empty() {
            return;
        }
        let lane =
            (self.cursor.lane as isize + delta).clamp(0, self.lanes.len() as isize - 1) as usize;
        self.cursor = Cursor {
            lane,
            card: 0,
            child: None,
        };
    }

    fn open_selected(&mut self) -> Option<Action> {
        if let Some(workspace) = self.selected_child() {
            return Some(Action::Open(workspace.workspace.clone()));
        }
        self.selected_task()
            .and_then(|task| {
                task.id.clone().or_else(|| {
                    task.workspaces
                        .first()
                        .map(|workspace| workspace.workspace.clone())
                })
            })
            .map(Action::Open)
    }

    fn mark_selected(&mut self) -> Option<Action> {
        if let Some(workspace) = self.selected_child() {
            return Some(Action::MarkWorkspace(workspace.workspace.clone()));
        }
        self.selected_task().and_then(|task| match &task.id {
            Some(id) => Some(Action::MarkTask(id.clone())),
            None => task
                .workspaces
                .first()
                .map(|workspace| Action::MarkWorkspace(workspace.workspace.clone())),
        })
    }

    fn seat_selected(&mut self) -> Option<Action> {
        match self.selected_task() {
            Some(task) => match &task.id {
                Some(id) => Some(Action::StartSeat(id.clone())),
                None => {
                    self.set_error("assign the workspace to a task first");
                    None
                }
            },
            None => None,
        }
    }

    fn start_assign(&mut self) -> Option<Action> {
        let Some(task) = self.selected_task() else {
            self.set_error("select an ungrouped workspace");
            return None;
        };
        if task.id.is_some() {
            self.set_error("only an ungrouped workspace can be assigned");
            return None;
        }
        if self.open_tasks().is_empty() {
            self.set_error("no open tasks; press n to create one");
            return None;
        }
        let Some(workspace) = task.workspaces.first() else {
            self.set_error("the selected card has no workspace");
            return None;
        };
        self.mode = Mode::Assign {
            workspace: workspace.workspace.clone(),
            selected: 0,
        };
        None
    }

    fn selected_key(&self) -> Option<CardKey> {
        self.selected_card().map(|card| card.key.clone())
    }

    fn selected_child_name(&self) -> Option<String> {
        self.selected_child()
            .map(|workspace| workspace.workspace.clone())
    }

    fn find_key(&self, key: &CardKey) -> Option<(usize, usize)> {
        self.lanes
            .iter()
            .enumerate()
            .find_map(|(lane_index, lane)| {
                lane.cards
                    .iter()
                    .position(|card| &card.key == key)
                    .map(|card_index| (lane_index, card_index))
            })
    }

    fn restore_selection(
        &mut self,
        key: Option<CardKey>,
        child: Option<String>,
        lane: usize,
        card: usize,
    ) {
        if let Some(key) = key {
            if let Some((lane_index, card_index)) = self.find_key(&key) {
                let child_index = child.and_then(|name| {
                    self.lanes[lane_index].cards[card_index]
                        .view
                        .workspaces
                        .iter()
                        .position(|workspace| workspace.workspace == name)
                });
                self.cursor = Cursor {
                    lane: lane_index,
                    card: card_index,
                    child: child_index,
                };
                return;
            }
        }
        self.cursor = Cursor {
            lane,
            card,
            child: None,
        };
        self.clamp();
    }

    fn clamp(&mut self) {
        if self.lanes.is_empty() {
            self.cursor = Cursor::default();
            return;
        }
        if self.cursor.lane >= self.lanes.len() {
            self.cursor.lane = self.lanes.len() - 1;
        }
        let lane = &self.lanes[self.cursor.lane];
        if lane.cards.is_empty() {
            self.cursor.card = 0;
            self.cursor.child = None;
            return;
        }
        if self.cursor.card >= lane.cards.len() {
            self.cursor.card = lane.cards.len() - 1;
            self.cursor.child = None;
        }
        if let Some(child) = self.cursor.child {
            if child >= lane.cards[self.cursor.card].workspaces().len() {
                self.cursor.child = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Clean, Hold, Lane, State, Wait};

    const NOW: i64 = 1_000_000;

    fn ws(name: &str, state: &str, hold: &str) -> Workspace {
        Workspace {
            workspace: name.to_string(),
            state: State::parse(state).unwrap_or_default(),
            hold: match hold {
                "live" => Hold::Live,
                "session" => Hold::Session,
                _ => Hold::Idle,
            },
            ..Workspace::default()
        }
    }

    fn task(id: &str, title: &str, workspaces: Vec<&str>) -> crate::model::Task {
        crate::model::Task {
            id: id.to_string(),
            title: title.to_string(),
            status: "open".to_string(),
            workspaces: workspaces.into_iter().map(str::to_string).collect(),
            ..crate::model::Task::default()
        }
    }

    fn board() -> Board {
        let mut active = ws("ws-active", "unpushed", "live");
        active.task = Some("active".into());
        let mut child = ws("ws-active-2", "merged", "-");
        child.task = Some("active".into());
        let mut loose = ws("ws-loose", "unpushed", "-");
        loose.description = "Loose work".into();
        let mut marked = ws("ws-clean", "merged", "-");
        marked.task = Some("cleanup".into());
        marked.clean = Some(Clean::default());
        let mut parked = ws("ws-parked", "empty", "-");
        parked.task = Some("blocked".into());
        parked.wait = Some(Wait::default());
        Board {
            workspaces: vec![active, child, loose, marked, parked],
            tasks: vec![
                task("active", "Active task", vec!["ws-active", "ws-active-2"]),
                task("cleanup", "Cleanup task", vec!["ws-clean"]),
                task("blocked", "Blocked task", vec!["ws-parked"]),
                task("backlog", "Backlog task", vec![]),
            ],
        }
    }

    fn press(app: &mut App, code: KeyCode) -> Option<Action> {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn lanes_from_the_fixture() {
        let app = App::new_at(board(), NOW);
        let lanes: Vec<(Lane, usize)> = app
            .lanes
            .iter()
            .map(|lane| (lane.lane, lane.cards.len()))
            .collect();
        assert_eq!(
            lanes,
            vec![
                (crate::model::Lane::Backlog, 1),
                (crate::model::Lane::Active, 2),
                (crate::model::Lane::Review, 0),
                (crate::model::Lane::Blocked, 1),
                (crate::model::Lane::ToClean, 1),
            ]
        );
    }

    #[test]
    fn navigation_expands_the_selected_task() {
        let mut app = App::new_at(board(), NOW);
        // Active is the second lane; the attention order puts "Active task"
        // before "Loose work".
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.cursor.lane, 1);
        assert_eq!(app.selected_task().unwrap().title, "Active task");
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.cursor.child, Some(0));
        assert_eq!(app.selected_child().unwrap().workspace, "ws-active");
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.cursor.child, Some(1));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.cursor.child, None);
        assert_eq!(app.selected_task().unwrap().title, "Loose work");
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.selected_task().unwrap().title, "Active task");
    }

    #[test]
    fn enter_opens_the_task_or_the_child() {
        let mut app = App::new_at(board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::Open("active".into()))
        );
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::Open("ws-active".into()))
        );
    }

    #[test]
    fn d_marks_the_task_or_the_workspace() {
        let mut app = App::new_at(board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(
            press(&mut app, KeyCode::Char('d')),
            Some(Action::MarkTask("active".into()))
        );
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            press(&mut app, KeyCode::Char('d')),
            Some(Action::MarkWorkspace("ws-active".into()))
        );
        // The standalone row has no task id.
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            press(&mut app, KeyCode::Char('d')),
            Some(Action::MarkWorkspace("ws-loose".into()))
        );
    }

    #[test]
    fn s_starts_a_seat_only_for_an_assigned_task() {
        let mut app = App::new_at(board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(
            press(&mut app, KeyCode::Char('s')),
            Some(Action::StartSeat("active".into()))
        );
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected_task().unwrap().id, None);
        assert_eq!(press(&mut app, KeyCode::Char('s')), None);
        assert!(app.error);
    }

    #[test]
    fn n_prompts_and_submits_a_task_title() {
        let mut app = App::new_at(board(), NOW);
        assert_eq!(press(&mut app, KeyCode::Char('n')), None);
        for ch in "Fix board".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::AddTask("Fix board".into()))
        );
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn a_assigns_the_selected_ungrouped_workspace() {
        let mut app = App::new_at(board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected_task().unwrap().id, None);
        assert_eq!(press(&mut app, KeyCode::Char('a')), None);
        assert!(matches!(
            &app.mode,
            Mode::Assign { workspace, .. } if workspace == "ws-loose"
        ));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::AssignTask {
                task: "active".into(),
                workspace: "ws-loose".into()
            })
        );
    }

    #[test]
    fn a_refuses_an_assigned_task() {
        let mut app = App::new_at(board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(press(&mut app, KeyCode::Char('a')), None);
        assert!(app.error);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn selection_follows_a_task_to_its_new_lane() {
        let mut app = App::new_at(board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.selected_task().unwrap().id.as_deref(), Some("active"));

        let mut moved = board();
        for workspace in &mut moved.workspaces {
            if workspace.workspace == "ws-active" {
                workspace.state = State::Merged;
                workspace.hold = Hold::Idle;
            }
        }
        app.set_board_at(moved, NOW);
        assert_eq!(app.lanes[app.cursor.lane].lane, Lane::Review);
        assert_eq!(app.selected_task().unwrap().id.as_deref(), Some("active"));
    }

    #[test]
    fn selection_clamps_when_the_card_disappears() {
        let mut app = App::new_at(board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected_task().unwrap().id, None);

        app.set_board_at(Board::default(), NOW);
        assert_eq!(app.cursor.lane, 1);
        assert_eq!(app.cursor.card, 0);
        assert!(app.selected_task().is_none());
    }

    #[test]
    fn help_and_quit() {
        let mut app = App::new_at(board(), NOW);
        assert_eq!(press(&mut app, KeyCode::Char('?')), None);
        assert_eq!(app.mode, Mode::Help);
        // Any key closes the overlay.
        assert_eq!(press(&mut app, KeyCode::Char('x')), None);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(press(&mut app, KeyCode::Char('q')), Some(Action::Quit));
    }
}
