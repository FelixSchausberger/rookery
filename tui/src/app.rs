// Board state and key handling. The TUI owns only a cached view: every write
// becomes an Action whose argv the runtime hands to the CLI.
use crate::model::{Board, TaskView, Workspace};
use crate::view::{self, CardKey, Filter, LaneModel, Palette, Sort};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rookery_core::util::now_secs;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Quit,
    Refresh,
    Open(String),
    MarkTask(String),
    MarkWorkspace(String),
    StartSeat(String),
    AddTask(String),
    AssignTask {
        task: String,
        workspace: String,
    },
    SetParent {
        child: String,
        parent: Option<String>,
    },
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
            Action::SetParent { .. } => Some("parent"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub lane: usize,
    /// The root card in the lane.
    pub card: usize,
    /// The card inside the root: 0 is the root itself, 1.. are its children.
    pub node: usize,
    /// The workspace row inside the selected card.
    pub child: Option<usize>,
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
    Parent {
        child: String,
        selected: usize,
    },
    Filter {
        before_query: String,
        before_selection: Option<CardKey>,
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
    /// The board snapshot time, also the age baseline for `refreshed <age>`.
    pub now: i64,
    pub clock: i64,
    pub detail: bool,
    pub help_scroll: usize,
    pub filter: Filter,
    pub query: String,
    pub sort: Sort,
    pub palette: Palette,
    /// Per-lane presentation state, written by `ui::render` and read by the
    /// paging keys: the applied scroll offset, the viewport height, and the
    /// height of the lane's content as last drawn.
    pub scroll: [usize; 5],
    pub viewports: [usize; 5],
    pub content_heights: [usize; 5],
    pub expanded: HashSet<CardKey>,
    pub reveal: bool,
    load_error: bool,
    pending_g: bool,
    filter_selection: Option<CardKey>,
}

fn selected_in(lane: &LaneModel, card: usize, node: usize) -> Option<&view::Card> {
    let root = lane.cards.get(card)?;
    if node == 0 {
        Some(root)
    } else {
        root.children.get(node - 1)
    }
}

/// The nodes `j`/`k` can visit: every root, plus the children of an expanded
/// root. Workspace rows are a sub-state of their card, not nodes.
fn visible_nodes(lane: &LaneModel, expanded: &HashSet<CardKey>) -> Vec<(usize, usize)> {
    let mut nodes = Vec::new();
    for (card, root) in lane.cards.iter().enumerate() {
        nodes.push((card, 0));
        if expanded.contains(&root.key) {
            for node in 1..=root.children.len() {
                nodes.push((card, node));
            }
        }
    }
    nodes
}

impl App {
    pub fn new(board: Board) -> Self {
        let mut app = Self::new_at(board, now_secs());
        app.palette = Palette::from_env();
        app
    }

    pub fn new_at(board: Board, now: i64) -> Self {
        let lanes = view::build(&board, now, Filter::All, "", Sort::Attention);
        let mut app = Self {
            board,
            lanes,
            now,
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
        self.lanes = view::build(&board, now, self.filter, &self.query, self.sort);
        self.board = board;
        self.now = now;
        self.clock = now;
        if self.load_error {
            self.status.clear();
            self.error = false;
            self.load_error = false;
        }
        let found = self.restore_selection(key, child, lane, card);
        let stale: Vec<CardKey> = self
            .expanded
            .iter()
            .filter(|key| self.find_key(key).is_none())
            .cloned()
            .collect();
        for key in stale {
            self.expanded.remove(&key);
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

    pub fn has_filter(&self) -> bool {
        self.filter != Filter::All || !self.query.trim().is_empty()
    }

    fn start_filter_input(&mut self) {
        self.pending_g = false;
        self.mode = Mode::Filter {
            before_query: self.query.clone(),
            before_selection: self.selected_key(),
        };
    }

    /// Apply an edit to `query`, then rebuild the lanes. `filter_selection`
    /// remembers the card the board was on when the filter first became active,
    /// so clearing or cancelling can return to it.
    fn edit_query(&mut self, edit: impl FnOnce(&mut String)) {
        let was_filtered = self.has_filter();
        edit(&mut self.query);
        if !was_filtered && self.has_filter() {
            self.filter_selection = self.selected_key();
        }
        if !self.has_filter() {
            self.filter_selection = None;
        }
        self.rebuild_lanes();
        if self.has_filter() {
            self.expand_roots_with_children();
        }
    }

    fn cycle_filter(&mut self) {
        let was_filtered = self.has_filter();
        self.filter = self.filter.next();
        if !was_filtered && self.has_filter() {
            self.filter_selection = self.selected_key();
        }
        if !self.has_filter() {
            self.filter_selection = None;
        }
        self.rebuild_lanes();
        if self.has_filter() {
            self.expand_roots_with_children();
        }
        self.set_status(format!("filter: {}", self.filter.label()));
        self.reveal = true;
    }

    /// A filter can match a child whose root is collapsed. Expand the roots so
    /// the match is visible; the person can still collapse them by hand.
    fn expand_roots_with_children(&mut self) {
        for lane in &self.lanes {
            for card in &lane.cards {
                if !card.children.is_empty() {
                    self.expanded.insert(card.key.clone());
                }
            }
        }
    }

    fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
        self.rebuild_lanes();
        self.set_status(format!("sort: {}", self.sort.label()));
        self.reveal = true;
    }

    fn clear_filter(&mut self) {
        self.filter = Filter::All;
        self.query.clear();
        self.rebuild_lanes();
        if let Some(key) = self.filter_selection.take() {
            self.restore_key(Some(key));
        }
        self.set_status("filter cleared");
    }

    fn rebuild_lanes(&mut self) {
        let key = self.selected_key();
        let child = self.selected_child_name();
        let previous = self.cursor;
        self.lanes = view::build(&self.board, self.now, self.filter, &self.query, self.sort);
        let found = self.restore_selection(key, child, previous.lane, previous.card);
        if !self.selection_is_expanded() {
            self.cursor.child = None;
        }
        self.reveal = !found || self.cursor != previous;
    }

    fn restore_key(&mut self, key: Option<CardKey>) {
        if let Some(key) = key {
            if let Some((lane, card, node)) = self.find_key(&key) {
                self.cursor = Cursor {
                    lane,
                    card,
                    node,
                    child: None,
                };
                self.reveal = true;
                return;
            }
        }
        self.clamp();
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
        let lane = self.lanes.get(self.cursor.lane)?;
        let root = lane.cards.get(self.cursor.card)?;
        if self.cursor.node == 0 {
            Some(root)
        } else {
            root.children.get(self.cursor.node - 1)
        }
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
        tasks.sort_by_cached_key(|(_, title)| title.to_lowercase());
        tasks
    }

    /// The roots a task may be attached to. A child cannot be a parent, so
    /// only tasks with no parent are listed.
    pub fn open_parents(&self, child: &str) -> Vec<(&str, &str)> {
        let mut tasks: Vec<(&str, &str)> = self
            .board
            .tasks
            .iter()
            .filter(|task| task.status == "open" && task.id != child && task.parent.is_none())
            .map(|task| (task.id.as_str(), task.title.as_str()))
            .collect();
        tasks.sort_by_cached_key(|(_, title)| title.to_lowercase());
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
            KeyCode::Char('/') => {
                self.start_filter_input();
                None
            }
            KeyCode::Char('f') => {
                self.cycle_filter();
                None
            }
            KeyCode::Char('o') => {
                self.cycle_sort();
                None
            }
            KeyCode::Esc => {
                if self.has_filter() {
                    self.clear_filter();
                } else if self.detail {
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
            KeyCode::Char('p') => self.start_parent(),
            KeyCode::Enter => self.open_selected(),
            _ => None,
        }
    }

    /// A modal owns the keyboard while it is up. Each arm either closes the
    /// mode by returning early, or handles the key and lets the tail restore
    /// the mode; help closes on any key it does not scroll with.
    fn handle_modal(&mut self, key: KeyEvent) -> Option<Action> {
        let mode = std::mem::take(&mut self.mode);
        match mode {
            Mode::Help => {
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => self.help_scroll += 1,
                    KeyCode::Char('k') | KeyCode::Up => {
                        self.help_scroll = self.help_scroll.saturating_sub(1);
                    }
                    KeyCode::PageDown => self.help_scroll += 10,
                    KeyCode::PageUp => {
                        self.help_scroll = self.help_scroll.saturating_sub(10);
                    }
                    _ => return None,
                }
                self.mode = Mode::Help;
                None
            }
            Mode::Filter {
                before_query,
                before_selection,
            } => {
                match key.code {
                    KeyCode::Esc => {
                        self.query = before_query;
                        if self.query.is_empty() && self.filter == Filter::All {
                            self.filter_selection = None;
                        }
                        self.rebuild_lanes();
                        self.restore_key(before_selection);
                        return None;
                    }
                    KeyCode::Enter => {
                        if self.query.trim().is_empty() {
                            self.set_status("filter cleared");
                        } else {
                            self.set_status(format!("filter \"{}\"", self.query.trim()));
                        }
                        return None;
                    }
                    KeyCode::Backspace => self.edit_query(|text| {
                        text.pop();
                    }),
                    KeyCode::Char(c) => self.edit_query(|text| text.push(c)),
                    _ => {}
                }
                self.mode = Mode::Filter {
                    before_query,
                    before_selection,
                };
                None
            }
            Mode::TaskInput { mut text } => {
                match key.code {
                    KeyCode::Esc => return None,
                    KeyCode::Enter => {
                        let title = text.trim().to_string();
                        if title.is_empty() {
                            self.set_error("a task needs a title");
                            return None;
                        }
                        return Some(Action::AddTask(title));
                    }
                    KeyCode::Backspace => {
                        text.pop();
                    }
                    KeyCode::Char(c) => text.push(c),
                    _ => {}
                }
                self.mode = Mode::TaskInput { text };
                None
            }
            Mode::Assign {
                workspace,
                mut selected,
            } => {
                match key.code {
                    KeyCode::Esc => return None,
                    KeyCode::Char('j') | KeyCode::Down => {
                        let len = self.open_tasks().len();
                        if len > 0 {
                            selected = (selected + 1).min(len - 1);
                        }
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        selected = selected.saturating_sub(1);
                    }
                    KeyCode::Enter => {
                        let tasks = self.open_tasks();
                        return match tasks.get(selected) {
                            Some((id, _)) => Some(Action::AssignTask {
                                task: (*id).to_string(),
                                workspace,
                            }),
                            None => {
                                self.set_error("no open tasks");
                                None
                            }
                        };
                    }
                    _ => {}
                }
                self.mode = Mode::Assign {
                    workspace,
                    selected,
                };
                None
            }
            Mode::Parent {
                child,
                mut selected,
            } => {
                let len = self.open_parents(&child).len();
                match key.code {
                    KeyCode::Esc => return None,
                    KeyCode::Char('j') | KeyCode::Down => {
                        selected = (selected + 1).min(len);
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        selected = selected.saturating_sub(1);
                    }
                    KeyCode::Enter => {
                        let parents = self.open_parents(&child);
                        let parent = if selected == 0 {
                            None
                        } else {
                            parents.get(selected - 1).map(|(id, _)| (*id).to_string())
                        };
                        return Some(Action::SetParent { child, parent });
                    }
                    _ => {}
                }
                self.mode = Mode::Parent { child, selected };
                None
            }
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
        let node = self.cursor.node;
        let Some(selected) = selected_in(lane, card, node) else {
            return;
        };
        let expanded = self.expanded.contains(&selected.key);
        if self.cursor.child.is_none() && expanded && !selected.workspaces().is_empty() {
            self.cursor.child = Some(0);
            self.reveal = true;
            return;
        }
        if let Some(child) = self.cursor.child {
            if child + 1 < selected.workspaces().len() {
                self.cursor.child = Some(child + 1);
                self.reveal = true;
                return;
            }
        }
        self.move_node(true);
        self.reveal = true;
    }

    fn move_up(&mut self) {
        match self.cursor.child {
            Some(0) => self.cursor.child = None,
            Some(child) => self.cursor.child = Some(child - 1),
            None => {
                self.move_node(false);
            }
        }
        self.reveal = true;
    }

    fn move_node(&mut self, forward: bool) {
        let Some(lane) = self.lanes.get(self.cursor.lane) else {
            return;
        };
        let nodes = visible_nodes(lane, &self.expanded);
        let Some(position) = nodes
            .iter()
            .position(|&(card, node)| card == self.cursor.card && node == self.cursor.node)
        else {
            return;
        };
        let next = if forward {
            position + 1
        } else {
            match position.checked_sub(1) {
                Some(previous) => previous,
                None => return,
            }
        };
        if let Some(&(card, node)) = nodes.get(next) {
            self.cursor.card = card;
            self.cursor.node = node;
            self.cursor.child = None;
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
            ..Cursor::default()
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
            ..Cursor::default()
        };
        self.reveal = true;
    }

    fn jump_lane(&mut self, lane: usize) {
        if lane >= self.lanes.len() {
            return;
        }
        self.cursor = Cursor {
            lane,
            ..Cursor::default()
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
        let nodes = visible_nodes(lane, &self.expanded);
        let Some(&(card, node)) = (if last { nodes.last() } else { nodes.first() }) else {
            return;
        };
        self.cursor.card = card;
        self.cursor.node = node;
        self.cursor.child = None;
        self.reveal = true;
    }

    fn toggle_expand(&mut self) {
        let Some(card) = self.selected_card() else {
            return;
        };
        if card.workspaces().is_empty() && card.children().is_empty() {
            return;
        }
        let key = card.key.clone();
        if self.expanded.remove(&key) {
            // The selected child or workspace belongs to the collapsed card.
            self.cursor.node = 0;
            self.cursor.child = None;
        } else {
            self.expanded.insert(key);
        }
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
        let offset = self.scroll.get(lane).copied().unwrap_or(0);
        let offset = if direction < 0 {
            offset.saturating_sub(step)
        } else {
            (offset + step).min(max_offset)
        };
        if let Some(slot) = self.scroll.get_mut(lane) {
            *slot = offset;
        }
        self.reveal = false;
    }

    fn open_selected(&mut self) -> Option<Action> {
        if let Some(workspace) = self.selected_child() {
            return Some(Action::Open(workspace.workspace.clone()));
        }
        let card = self.selected_card()?;
        if card.id().is_some() && card.workspaces().is_empty() {
            if !card.children().is_empty() {
                self.expanded.insert(card.key.clone());
                self.cursor.node += 1;
                self.cursor.child = None;
                self.reveal = true;
                return None;
            }
            self.set_error("task has no workspace; press s to start a seat");
            return None;
        }
        let target = card.id().map(str::to_string).or_else(|| {
            card.workspaces()
                .first()
                .map(|workspace| workspace.workspace.clone())
        });
        target.map(Action::Open)
    }

    fn mark_selected(&mut self) -> Option<Action> {
        if let Some(workspace) = self.selected_child() {
            return Some(Action::MarkWorkspace(workspace.workspace.clone()));
        }
        let card = self.selected_card()?;
        if let Some(id) = card.id() {
            if card.workspaces().is_empty() {
                self.set_error("task has no workspaces; select a child");
                return None;
            }
            return Some(Action::MarkTask(id.to_string()));
        }
        card.workspaces()
            .first()
            .map(|workspace| Action::MarkWorkspace(workspace.workspace.clone()))
    }

    /// A seat starts only for a task that has no workspace and no children. A
    /// task that already has one is opened instead; starting another would put
    /// a second agent under the same task without saying so.
    fn seat_selected(&mut self) -> Option<Action> {
        let card = self.selected_card()?;
        let Some(id) = card.id() else {
            self.set_error("assign the workspace to a task first");
            return None;
        };
        if !card.workspaces().is_empty() {
            self.set_status("task already has a workspace; enter opens it");
            return None;
        }
        if !card.children().is_empty() {
            self.set_status("task has child tasks; start a seat for a child");
            return None;
        }
        Some(Action::StartSeat(id.to_string()))
    }

    fn start_parent(&mut self) -> Option<Action> {
        let Some(card) = self.selected_card() else {
            self.set_error("select a task");
            return None;
        };
        let Some(id) = card.id().map(str::to_string) else {
            self.set_error("assign the workspace to a task first");
            return None;
        };
        let current = card.parent().map(str::to_string);
        let selected = current
            .and_then(|parent| {
                self.open_parents(&id)
                    .iter()
                    .position(|(candidate, _)| *candidate == parent)
                    .map(|position| position + 1)
            })
            .unwrap_or(0);
        self.mode = Mode::Parent {
            child: id,
            selected,
        };
        None
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
            .map(|card| self.expanded.contains(&card.key))
            .unwrap_or(false)
    }

    fn find_key(&self, key: &CardKey) -> Option<(usize, usize, usize)> {
        self.lanes
            .iter()
            .enumerate()
            .find_map(|(lane_index, lane)| {
                lane.cards
                    .iter()
                    .enumerate()
                    .find_map(|(card_index, root)| {
                        if &root.key == key {
                            return Some((lane_index, card_index, 0));
                        }
                        root.children
                            .iter()
                            .position(|child| &child.key == key)
                            .map(|node| (lane_index, card_index, node + 1))
                    })
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
            if let Some((lane_index, card_index, node_index)) = self.find_key(&key) {
                if node_index > 0 {
                    let root_key = self.lanes[lane_index].cards[card_index].key.clone();
                    self.expanded.insert(root_key);
                }
                let child_index = child.and_then(|name| {
                    selected_in(&self.lanes[lane_index], card_index, node_index).and_then(|card| {
                        card.workspaces()
                            .iter()
                            .position(|workspace| workspace.workspace == name)
                    })
                });
                self.cursor = Cursor {
                    lane: lane_index,
                    card: card_index,
                    node: node_index,
                    child: child_index,
                };
                return true;
            }
        }
        self.select_nearest(lane, card);
        false
    }

    fn select_nearest(&mut self, lane: usize, card: usize) {
        if self.lanes.is_empty() {
            self.cursor = Cursor::default();
            return;
        }
        let lane = lane.min(self.lanes.len() - 1);
        if !self.lanes[lane].cards.is_empty() {
            self.cursor = Cursor {
                lane,
                card: card.min(self.lanes[lane].cards.len() - 1),
                ..Cursor::default()
            };
            return;
        }
        for distance in 1..self.lanes.len() {
            if let Some(left) = lane.checked_sub(distance) {
                if !self.lanes[left].cards.is_empty() {
                    self.cursor = Cursor {
                        lane: left,
                        ..Cursor::default()
                    };
                    return;
                }
            }
            let right = lane + distance;
            if right < self.lanes.len() && !self.lanes[right].cards.is_empty() {
                self.cursor = Cursor {
                    lane: right,
                    ..Cursor::default()
                };
                return;
            }
        }
        self.cursor = Cursor {
            lane,
            ..Cursor::default()
        };
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
            self.cursor.node = 0;
            self.cursor.child = None;
            return;
        }
        if self.cursor.card >= lane.cards.len() {
            self.cursor.card = lane.cards.len() - 1;
            self.cursor.node = 0;
            self.cursor.child = None;
        }
        let root = &lane.cards[self.cursor.card];
        if self.cursor.node > root.children.len()
            || (self.cursor.node > 0 && !self.expanded.contains(&root.key))
        {
            self.cursor.node = 0;
            self.cursor.child = None;
        }
        if let Some(child) = self.cursor.child {
            let workspaces = selected_in(lane, self.cursor.card, self.cursor.node)
                .map(|card| card.workspaces().len())
                .unwrap_or(0);
            if child >= workspaces {
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
        assert!(app.expanded.contains(&CardKey::Task("active".into())));
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
        assert!(app.expanded.is_empty());
        assert_eq!(app.cursor.child, None);
    }

    #[test]
    fn space_is_a_noop_on_a_backlog_card() {
        let mut app = App::new_at(board(), NOW);
        assert_eq!(app.selected_task().unwrap().id.as_deref(), Some("backlog"));
        press(&mut app, KeyCode::Char(' '));
        assert!(app.expanded.is_empty());
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
    fn s_starts_a_seat_only_for_an_empty_task() {
        let mut app = App::new_at(board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(press(&mut app, KeyCode::Char('s')), None);
        assert!(!app.error);
        assert_eq!(app.status, "task already has a workspace; enter opens it");

        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected_task().unwrap().id, None);
        assert_eq!(press(&mut app, KeyCode::Char('s')), None);
        assert!(app.error);

        press(&mut app, KeyCode::Char('1'));
        assert_eq!(app.selected_task().unwrap().id.as_deref(), Some("backlog"));
        assert_eq!(
            press(&mut app, KeyCode::Char('s')),
            Some(Action::StartSeat("backlog".into()))
        );
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
    fn p_opens_a_parent_picker_and_clears_or_sets_the_link() {
        let mut app = App::new_at(board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(press(&mut app, KeyCode::Char('p')), None);
        assert!(matches!(
            &app.mode,
            Mode::Parent { child, selected } if child == "active" && *selected == 0
        ));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::SetParent {
                child: "active".into(),
                parent: None
            })
        );

        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Some(Action::SetParent {
                child: "active".into(),
                parent: Some("backlog".into())
            })
        );
    }

    #[test]
    fn p_refuses_a_standalone_workspace() {
        let mut app = App::new_at(board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(press(&mut app, KeyCode::Char('p')), None);
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
        assert_eq!(app.scroll[1], 0);
        press(&mut app, KeyCode::PageDown);
        assert_eq!(app.scroll[1], 10);
        press(&mut app, KeyCode::PageUp);
        assert_eq!(app.scroll[1], 0);
        press_ctrl(&mut app, KeyCode::Char('d'));
        assert_eq!(app.scroll[1], 5);
        press_ctrl(&mut app, KeyCode::Char('u'));
        assert_eq!(app.scroll[1], 0);
        for _ in 0..5 {
            press(&mut app, KeyCode::PageDown);
        }
        assert_eq!(app.scroll[1], 30);
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
    fn filter_preset_and_clear_restore_the_selection() {
        let mut app = App::new_at(board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.selected_task().unwrap().id.as_deref(), Some("active"));
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(app.filter, Filter::Attention);
        assert_eq!(app.lanes[app.cursor.lane].lane, Lane::Blocked);
        assert!(app.selected_task().unwrap().title.contains("Blocked"));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.filter, Filter::All);
        assert_eq!(app.selected_task().unwrap().id.as_deref(), Some("active"));
    }

    #[test]
    fn filter_query_applies_live_and_esc_clears() {
        let mut app = App::new_at(board(), NOW);
        assert_eq!(app.selected_task().unwrap().id.as_deref(), Some("backlog"));
        press(&mut app, KeyCode::Char('/'));
        for ch in "active".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        assert_eq!(app.query, "active");
        assert_eq!(app.selected_task().unwrap().title, "Active task");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, Mode::Normal);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.filter, Filter::All);
        assert!(app.query.is_empty());
        assert_eq!(app.selected_task().unwrap().id.as_deref(), Some("backlog"));
    }

    #[test]
    fn filter_input_escape_cancels_the_query() {
        let mut app = App::new_at(board(), NOW);
        press(&mut app, KeyCode::Char('/'));
        for ch in "active".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        assert_eq!(app.query, "active");
        press(&mut app, KeyCode::Esc);
        assert!(app.query.is_empty());
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.selected_task().unwrap().id.as_deref(), Some("backlog"));
    }

    #[test]
    fn sort_key_cycles_presentation_order() {
        let mut app = App::new_at(board(), NOW);
        assert_eq!(app.sort, Sort::Attention);
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.sort, Sort::Title);
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.sort, Sort::Updated);
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.sort, Sort::Attention);
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
