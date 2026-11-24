// Board state and key handling. The TUI owns only a cached view: every write
// becomes an Action whose argv the runtime hands to the CLI.
use crate::model::{Board, TaskView, Workspace};
use crate::view::{self, CardKey, LaneModel};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rookery_core::util::now_secs;

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScrollState {
    pub offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollAmount {
    Half,
    Full,
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
    pub loaded_at: i64,
    pub clock: i64,
    pub detail: bool,
    pub help_scroll: usize,
    pub scroll: [ScrollState; 5],
    pub viewports: [usize; 5],
    pub content_heights: [usize; 5],
    pub expanded: Option<CardKey>,
    pub reveal: bool,
    load_error: bool,
    pending_g: bool,
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
            loaded_at: now,
            clock: now,
            reveal: true,
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
        let previous = self.cursor;
        let lane = self.cursor.lane;
        let card = self.cursor.card;
        self.lanes = view::build(&board);
        self.board = board;
        self.now = now;
        self.loaded_at = now;
        self.clock = now;
        if self.load_error {
            self.status.clear();
            self.error = false;
            self.load_error = false;
        }
        let found = self.restore_selection(key, child, lane, card);
        if let Some(expanded) = self.expanded.clone() {
            if self.find_key(&expanded).is_none() {
                self.expanded = None;
            }
        }
        if !self.selection_is_expanded() {
            self.cursor.child = None;
        }
        self.reveal = !found || self.cursor != previous;
    }

    pub fn tick(&mut self, now: i64) {
        self.clock = now;
    }

    pub fn has_load_error(&self) -> bool {
        self.load_error
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
            self.pending_g = false;
            return self.handle_modal(key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            self.pending_g = false;
            match key.code {
                KeyCode::Char('d') => {
                    self.scroll_by(1, ScrollAmount::Half);
                    return None;
                }
                KeyCode::Char('u') => {
                    self.scroll_by(-1, ScrollAmount::Half);
                    return None;
                }
                _ => {}
            }
        }
        let is_g = matches!(key.code, KeyCode::Char('g'));
        if !is_g {
            self.pending_g = false;
        }
        match key.code {
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Char('?') => {
                self.help_scroll = 0;
                self.mode = Mode::Help;
                None
            }
            KeyCode::Char('i') => {
                self.detail = !self.detail;
                self.reveal = true;
                None
            }
            KeyCode::Esc => {
                if self.detail {
                    self.detail = false;
                    self.reveal = true;
                }
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
            KeyCode::Tab => {
                let delta = if key.modifiers.contains(KeyModifiers::SHIFT) {
                    -1
                } else {
                    1
                };
                self.cycle_lane(delta);
                None
            }
            KeyCode::BackTab => {
                self.cycle_lane(-1);
                None
            }
            KeyCode::Char(c @ '1'..='5') => {
                self.jump_lane(c.to_digit(10).unwrap() as usize - 1);
                None
            }
            KeyCode::Char(' ') => {
                self.toggle_expand();
                None
            }
            KeyCode::Char('g') => {
                if self.pending_g {
                    self.pending_g = false;
                    self.jump_first();
                } else {
                    self.pending_g = true;
                }
                None
            }
            KeyCode::Char('G') => {
                self.jump_last();
                None
            }
            KeyCode::PageDown => {
                self.scroll_by(1, ScrollAmount::Full);
                None
            }
            KeyCode::PageUp => {
                self.scroll_by(-1, ScrollAmount::Full);
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
            Mode::Help => match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    self.help_scroll += 1;
                    self.mode = Mode::Help;
                    None
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.help_scroll = self.help_scroll.saturating_sub(1);
                    self.mode = Mode::Help;
                    None
                }
                KeyCode::PageDown => {
                    self.help_scroll += 10;
                    self.mode = Mode::Help;
                    None
                }
                KeyCode::PageUp => {
                    self.help_scroll = self.help_scroll.saturating_sub(10);
                    self.mode = Mode::Help;
                    None
                }
                _ => None,
            },
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
        let expanded = self.expanded.as_ref() == Some(&lane.cards[card].key);
        match self.cursor.child {
            None => {
                if expanded && !lane.cards[card].workspaces().is_empty() {
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
        self.reveal = true;
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
        self.reveal = true;
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
        self.reveal = true;
    }

    fn cycle_lane(&mut self, delta: isize) {
        if self.lanes.is_empty() {
            return;
        }
        let len = self.lanes.len() as isize;
        let lane = (self.cursor.lane as isize + delta).rem_euclid(len) as usize;
        self.cursor = Cursor {
            lane,
            card: 0,
            child: None,
        };
        self.reveal = true;
    }

    fn jump_lane(&mut self, lane: usize) {
        if lane >= self.lanes.len() {
            return;
        }
        self.cursor = Cursor {
            lane,
            card: 0,
            child: None,
        };
        self.reveal = true;
    }

    fn jump_first(&mut self) {
        self.jump_to_edge(false);
    }

    fn jump_last(&mut self) {
        self.jump_to_edge(true);
    }

    fn jump_to_edge(&mut self, last: bool) {
        let Some(lane) = self.lanes.get(self.cursor.lane) else {
            return;
        };
        if lane.cards.is_empty() {
            return;
        }
        self.cursor.card = if last { lane.cards.len() - 1 } else { 0 };
        self.cursor.child = None;
        self.reveal = true;
    }

    fn toggle_expand(&mut self) {
        let Some(card) = self.selected_card() else {
            return;
        };
        if card.workspaces().is_empty() {
            return;
        }
        let key = card.key.clone();
        if self.expanded.as_ref() == Some(&key) {
            self.expanded = None;
        } else {
            self.expanded = Some(key);
        }
        self.cursor.child = None;
        self.reveal = true;
    }

    fn scroll_by(&mut self, direction: isize, amount: ScrollAmount) {
        let lane = self.cursor.lane;
        let viewport = self.viewports.get(lane).copied().unwrap_or(0).max(1);
        let content = self.content_heights.get(lane).copied().unwrap_or(0);
        let max_offset = content.saturating_sub(viewport);
        let step = match amount {
            ScrollAmount::Half => (viewport / 2).max(1),
            ScrollAmount::Full => viewport,
        };
        let offset = self.scroll.get(lane).map(|state| state.offset).unwrap_or(0);
        let offset = if direction < 0 {
            offset.saturating_sub(step)
        } else {
            (offset + step).min(max_offset)
        };
        if let Some(state) = self.scroll.get_mut(lane) {
            state.offset = offset;
        }
        self.reveal = false;
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

    fn selection_is_expanded(&self) -> bool {
        self.selected_card()
            .map(|card| self.expanded.as_ref() == Some(&card.key))
            .unwrap_or(false)
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
    ) -> bool {
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
                return true;
            }
        }
        self.cursor = Cursor {
            lane,
            card,
            child: None,
        };
        self.clamp();
        false
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
        if !self.selection_is_expanded() {
            self.cursor.child = None;
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

    fn press_ctrl(app: &mut App, code: KeyCode) -> Option<Action> {
        app.handle_key(KeyEvent::new(code, KeyModifiers::CONTROL))
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
    fn j_moves_between_cards_until_space_expands_one() {
        let mut app = App::new_at(board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.selected_task().unwrap().title, "Active task");
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected_task().unwrap().title, "Loose work");
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.selected_task().unwrap().title, "Active task");

        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.expanded, Some(CardKey::Task("active".into())));
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
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.expanded, None);
        assert_eq!(app.cursor.child, None);
    }

    #[test]
    fn space_is_a_noop_on_a_backlog_card() {
        let mut app = App::new_at(board(), NOW);
        assert_eq!(app.selected_task().unwrap().id.as_deref(), Some("backlog"));
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.expanded, None);
    }

    #[test]
    fn enter_opens_the_task_or_the_child() {
        let mut app = App::new_at(board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::Open("active".into()))
        );
        press(&mut app, KeyCode::Char(' '));
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
        press(&mut app, KeyCode::Char(' '));
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
        assert_eq!(app.selected_task().unwrap().id, None);

        app.set_board_at(Board::default(), NOW);
        assert_eq!(app.cursor.lane, 1);
        assert_eq!(app.cursor.card, 0);
        assert!(app.selected_task().is_none());
    }

    #[test]
    fn paging_moves_the_scroll_state_without_moving_the_cursor() {
        let mut app = App::new_at(board(), NOW);
        app.viewports = [10; 5];
        app.content_heights = [40; 5];
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.cursor.lane, 1);
        assert_eq!(app.scroll[1].offset, 0);
        press(&mut app, KeyCode::PageDown);
        assert_eq!(app.scroll[1].offset, 10);
        press(&mut app, KeyCode::PageUp);
        assert_eq!(app.scroll[1].offset, 0);
        press_ctrl(&mut app, KeyCode::Char('d'));
        assert_eq!(app.scroll[1].offset, 5);
        press_ctrl(&mut app, KeyCode::Char('u'));
        assert_eq!(app.scroll[1].offset, 0);
        for _ in 0..5 {
            press(&mut app, KeyCode::PageDown);
        }
        assert_eq!(app.scroll[1].offset, 30);
        assert_eq!(app.cursor.card, 0);
    }

    #[test]
    fn gg_and_shift_g_move_to_the_edges_of_the_lane() {
        let mut app = App::new_at(board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(app.cursor.card, 1);
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.cursor.card, 0);
    }

    #[test]
    fn tab_and_numbers_move_between_lanes() {
        let mut app = App::new_at(board(), NOW);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.cursor.lane, 1);
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.cursor.lane, 0);
        press(&mut app, KeyCode::Char('5'));
        assert_eq!(app.cursor.lane, 4);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.cursor.lane, 0);
        press(&mut app, KeyCode::Char('3'));
        assert_eq!(app.cursor.lane, 2);
    }

    #[test]
    fn i_toggles_the_detail_pane_and_esc_closes_it() {
        let mut app = App::new_at(board(), NOW);
        assert!(!app.detail);
        press(&mut app, KeyCode::Char('i'));
        assert!(app.detail);
        press(&mut app, KeyCode::Esc);
        assert!(!app.detail);
    }

    #[test]
    fn help_scrolls_and_closes() {
        let mut app = App::new_at(board(), NOW);
        press(&mut app, KeyCode::Char('?'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.mode, Mode::Help);
        assert_eq!(app.help_scroll, 1);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, Mode::Normal);
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
