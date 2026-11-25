// The board's presentation model: cards, their aggregates, and the lines the
// terminal draws. The core derives the five lanes; this module only orders and
// formats them. Nothing here reads jj or the task store.
use crate::model::{Board, Clean, Hold, Lane, State, Task, TaskView, Wait, Workspace};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::cmp::Reverse;
use std::collections::HashMap;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The stable identity of a card across refreshes. A task card follows its id;
/// a standalone card follows its workspace name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CardKey {
    Task(String),
    Workspace(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Attention,
    Live,
    Unpushed,
    Reclaimable,
}

impl Filter {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Attention,
            Self::Attention => Self::Live,
            Self::Live => Self::Unpushed,
            Self::Unpushed => Self::Reclaimable,
            Self::Reclaimable => Self::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Attention => "attention",
            Self::Live => "live",
            Self::Unpushed => "unpushed",
            Self::Reclaimable => "reclaimable",
        }
    }

    pub fn matches(self, card: &Card) -> bool {
        match self {
            Self::All => true,
            Self::Attention => card
                .workspaces()
                .iter()
                .any(|workspace| workspace.wait.is_some() || workspace.state == State::Conflict),
            Self::Live => card
                .workspaces()
                .iter()
                .any(|workspace| workspace.hold == Hold::Live),
            Self::Unpushed => card
                .workspaces()
                .iter()
                .any(|workspace| workspace.state == State::Unpushed),
            Self::Reclaimable => card.workspaces().iter().any(|workspace| {
                workspace.clean.is_some()
                    || (workspace.hold == Hold::Idle
                        && workspace.wait.is_none()
                        && matches!(workspace.state, State::Empty | State::Merged))
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    #[default]
    Attention,
    Title,
    Updated,
}

impl Sort {
    pub fn next(self) -> Self {
        match self {
            Self::Attention => Self::Title,
            Self::Title => Self::Updated,
            Self::Updated => Self::Attention,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Attention => "attention",
            Self::Title => "title",
            Self::Updated => "updated",
        }
    }
}

/// The terminal's color policy. `NO_COLOR` keeps the layout, selection and
/// modifiers but removes every foreground color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub color: bool,
}

impl Palette {
    pub const COLOR: Self = Self { color: true };
    pub const PLAIN: Self = Self { color: false };

    pub fn from_env() -> Self {
        if std::env::var_os("NO_COLOR").is_some() {
            Self::PLAIN
        } else {
            Self::COLOR
        }
    }

    pub fn fg(self, color: Color) -> Style {
        if self.color {
            Style::default().fg(color)
        } else {
            Style::default()
        }
    }

    pub fn border(self) -> Style {
        self.fg(Color::DarkGray)
    }

    /// A selected border is bold in both palettes: without color, `fg` already
    /// returns the plain style, so the modifier is the only difference to add.
    pub fn selected_border(self) -> Style {
        self.fg(Color::Cyan).add_modifier(Modifier::BOLD)
    }

    pub fn error(self) -> Style {
        let style = self.fg(Color::Red);
        if self.color {
            style
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        }
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::COLOR
    }
}

#[derive(Clone, Debug)]
pub struct Card {
    pub key: CardKey,
    pub view: TaskView,
    pub task: Option<Task>,
}

#[derive(Clone, Debug)]
pub struct LaneModel {
    pub lane: Lane,
    pub cards: Vec<Card>,
}

/// Build the five lanes in their presentation order. The core's `lanes()` is
/// the input; the order here is the board's, never the core's.
pub fn build(board: &Board, now: i64, filter: Filter, query: &str, sort: Sort) -> Vec<LaneModel> {
    let tasks: HashMap<&str, &Task> = board
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), task))
        .collect();
    board
        .lanes()
        .into_iter()
        .map(|lane| {
            let mut cards: Vec<Card> = lane
                .tasks
                .into_iter()
                .map(|view| Card::new(view, &tasks))
                .filter(|card| filter.matches(card) && matches_query(card, query))
                .collect();
            sort_cards(&mut cards, sort, now);
            LaneModel {
                lane: lane.lane,
                cards,
            }
        })
        .collect()
}

/// Match a card's title, id, notes, workspace names, descriptions, or session
/// titles. The query is case-insensitive; a leading '#' is ignored.
pub fn matches_query(card: &Card, query: &str) -> bool {
    let query = query.trim().trim_start_matches('#').to_lowercase();
    if query.is_empty() {
        return true;
    }
    if card.title().to_lowercase().contains(&query) {
        return true;
    }
    if card
        .id()
        .map(|id| id.to_lowercase().contains(&query))
        .unwrap_or(false)
    {
        return true;
    }
    if card
        .task
        .as_ref()
        .map(|task| task.notes.to_lowercase().contains(&query))
        .unwrap_or(false)
    {
        return true;
    }
    card.workspaces().iter().any(|workspace| {
        workspace.workspace.to_lowercase().contains(&query)
            || workspace.description.to_lowercase().contains(&query)
            || workspace
                .session
                .as_ref()
                .and_then(|session| session.title.as_deref())
                .map(|title| title.to_lowercase().contains(&query))
                .unwrap_or(false)
    })
}

impl Card {
    fn new(view: TaskView, tasks: &HashMap<&str, &Task>) -> Self {
        let task = view
            .id
            .as_deref()
            .and_then(|id| tasks.get(id))
            .copied()
            .cloned();
        let key = match &view.id {
            Some(id) => CardKey::Task(id.clone()),
            None => CardKey::Workspace(
                view.workspaces
                    .first()
                    .map(|workspace| workspace.workspace.clone())
                    .unwrap_or_else(|| view.title.clone()),
            ),
        };
        Self { key, view, task }
    }

    pub fn title(&self) -> &str {
        &self.view.title
    }

    pub fn id(&self) -> Option<&str> {
        self.view.id.as_deref()
    }

    pub fn is_standalone(&self) -> bool {
        self.view.id.is_none()
    }

    pub fn workspaces(&self) -> &[Workspace] {
        &self.view.workspaces
    }

    pub fn oldest_wait(&self) -> Option<&Wait> {
        self.workspaces()
            .iter()
            .filter_map(|workspace| workspace.wait.as_ref())
            .max_by_key(|wait| wait.age)
    }

    pub fn oldest_clean(&self) -> Option<&Clean> {
        self.workspaces()
            .iter()
            .filter_map(|workspace| workspace.clean.as_ref())
            .max_by_key(|clean| clean.age)
    }

    fn latest_session_title(&self) -> Option<(&str, i64)> {
        self.workspaces()
            .iter()
            .filter_map(|workspace| workspace.session.as_ref())
            .filter_map(|session| {
                session
                    .title
                    .as_deref()
                    .map(|title| (title, session.updated.unwrap_or(0)))
            })
            .max_by_key(|(_, updated)| *updated)
    }

    /// The newest timestamp that describes the card: the task's own update,
    /// a recent session, or the workspace target's commit. The card label is
    /// always "updated <age>"; the detail pane can say which one it is.
    pub fn activity_time(&self, now: i64) -> Option<i64> {
        let task_time = self
            .task
            .as_ref()
            .map(|task| task.updated)
            .filter(|time| *time > 0);
        let session_time = self
            .workspaces()
            .iter()
            .filter_map(|workspace| workspace.session.as_ref()?.updated)
            .max();
        let workspace_time = self
            .workspaces()
            .iter()
            .map(|workspace| now - workspace.age)
            .max();
        task_time
            .into_iter()
            .chain(session_time)
            .chain(workspace_time)
            .max()
    }

    pub fn activity_age(&self, now: i64) -> Option<i64> {
        self.activity_time(now).map(|time| (now - time).max(0))
    }

    fn holds_label(&self) -> Option<String> {
        let live = self
            .workspaces()
            .iter()
            .filter(|workspace| workspace.hold == Hold::Live)
            .count();
        let session = self
            .workspaces()
            .iter()
            .filter(|workspace| workspace.hold == Hold::Session)
            .count();
        let mut parts = Vec::new();
        if live > 0 {
            parts.push(format!("{live} live"));
        }
        if session > 0 {
            parts.push(format!("{session} session"));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" · "))
        }
    }

    /// The aggregate state and marker line. Parts are appended while they fit
    /// the card; the detail pane has the complete list.
    pub fn badges(&self, width: usize) -> Option<String> {
        let mut parts = Vec::new();
        for state in [
            State::Conflict,
            State::Unpushed,
            State::InFlight,
            State::Merged,
            State::Empty,
        ] {
            let count = self
                .workspaces()
                .iter()
                .filter(|workspace| workspace.state == state)
                .count();
            if count > 0 {
                parts.push(format!("{count} {}", state.as_str()));
            }
        }
        if let Some(wait) = self.oldest_wait() {
            parts.push(format!("waiting {}", relative_age(wait.age)));
        }
        if let Some(clean) = self.oldest_clean() {
            parts.push(format!("clean {}", relative_age(clean.age)));
        }
        if parts.is_empty() {
            if self.workspaces().is_empty() {
                return Some(truncate("press s to start a seat", width));
            }
            return None;
        }
        Some(join_fitting(&parts, width))
    }

    /// The fourth body line: the wait reason, the newest session title, the
    /// hold counts, or the cleanup hint. Everything else lives in the detail
    /// pane so the card stays four lines tall.
    pub fn context(&self) -> Option<String> {
        if let Some(wait) = self.oldest_wait() {
            let reason = wait.reason.trim();
            return Some(if reason.is_empty() {
                format!("waiting {}", relative_age(wait.age))
            } else {
                format!("wait: {reason}")
            });
        }
        if let Some((title, _)) = self
            .latest_session_title()
            .filter(|(title, _)| !title.trim().is_empty())
        {
            return Some(format!("session \"{title}\""));
        }
        if let Some(holds) = self.holds_label() {
            return Some(holds);
        }
        if !self.workspaces().is_empty()
            && self
                .workspaces()
                .iter()
                .all(|workspace| workspace.clean.is_some())
            && self
                .workspaces()
                .iter()
                .any(|workspace| workspace.state == State::Unpushed)
        {
            return Some("sweep keeps unpushed".to_string());
        }
        None
    }

    pub fn lines(
        &self,
        width: usize,
        selected: bool,
        expanded: bool,
        child: Option<usize>,
        now: i64,
        palette: Palette,
    ) -> Vec<Line<'static>> {
        if width < 4 {
            return Vec::new();
        }
        let inner = width - 2;
        let accent = accent_style(self, selected, palette);
        let border = if selected {
            palette.selected_border()
        } else {
            palette.border()
        };
        let content = self.content_lines(inner, expanded, child, now);
        let mut lines = Vec::with_capacity(content.len() + 2);
        lines.push(border_line("╭", "╮", inner, accent, border));
        lines.extend(
            content
                .into_iter()
                .map(|line| box_line(line, inner, accent, border)),
        );
        lines.push(border_line("╰", "╯", inner, accent, border));
        lines
    }

    /// A borderless card for narrow terminals: a state stripe, the title and
    /// aggregate badges, and the expanded card's workspaces underneath.
    pub fn compact_lines(
        &self,
        width: usize,
        selected: bool,
        expanded: bool,
        child: Option<usize>,
        palette: Palette,
    ) -> Vec<Line<'static>> {
        if width == 0 {
            return Vec::new();
        }
        let accent = accent_style(self, selected, palette);
        let available = width.saturating_sub(2);
        let title = if self.view.title.trim().is_empty() {
            "(untitled)"
        } else {
            self.view.title.trim()
        };
        let count = if !self.is_standalone() && !self.workspaces().is_empty() {
            format!(" ({})", self.workspaces().len())
        } else {
            String::new()
        };
        let title_width = available.saturating_sub(display_width(&count));
        let heading = format!("{}{count}", truncate(title, title_width));
        let style = if selected {
            Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::default()
        };
        let mut lines = vec![Line::from(vec![
            Span::styled("▌", accent),
            Span::raw(" "),
            Span::styled(heading, style),
        ])];
        let mut detail = self.badges(available).unwrap_or_default();
        if let Some(context) = self.context() {
            if !detail.is_empty() {
                detail.push_str(" · ");
            }
            detail.push_str(&context);
        }
        if !detail.is_empty() {
            lines.push(Line::styled(
                truncate(&format!("  {detail}"), width),
                dim_style(),
            ));
        }
        if expanded {
            for (index, workspace) in self.workspaces().iter().enumerate() {
                lines.extend(child_lines(workspace, index, child, width));
            }
        }
        lines
    }

    fn content_lines(
        &self,
        inner: usize,
        expanded: bool,
        child: Option<usize>,
        now: i64,
    ) -> Vec<Line<'static>> {
        let mut content = Vec::new();
        let mut count = String::new();
        if !self.is_standalone() && !self.workspaces().is_empty() {
            let candidate = self.workspaces().len().to_string();
            if display_width(&candidate) + 1 < inner {
                count = candidate;
            }
        }
        let count_width = display_width(&count);
        let title_width = if count_width > 0 {
            inner.saturating_sub(count_width + 1)
        } else {
            inner
        };
        let title = if self.view.title.trim().is_empty() {
            "(untitled)"
        } else {
            self.view.title.trim()
        };
        let wrapped = wrap(title, title_width, 2);
        if wrapped.is_empty() {
            content.push(Line::from(vec![Span::raw(String::new())]));
        } else {
            for (index, part) in wrapped.into_iter().enumerate() {
                let part_width = display_width(&part);
                let mut spans = vec![Span::styled(part, title_style())];
                if index == 0 && count_width > 0 {
                    let pad = inner.saturating_sub(part_width + count_width);
                    spans.push(Span::raw(" ".repeat(pad)));
                    spans.push(Span::styled(count.clone(), dim_style()));
                }
                content.push(Line::from(spans));
            }
        }

        let mut meta = Vec::new();
        if let Some(id) = self.id() {
            meta.push(format!("#{id}"));
        }
        if let Some(age) = self.activity_age(now) {
            meta.push(format!("updated {}", relative_age(age)));
        }
        if !meta.is_empty() {
            content.push(Line::styled(
                truncate(&meta.join(" · "), inner),
                dim_style(),
            ));
        }

        if let Some(badges) = self.badges(inner) {
            content.push(Line::styled(badges, dim_style()));
        }
        if let Some(context) = self.context() {
            content.push(Line::styled(truncate(&context, inner), dim_style()));
        }

        if expanded {
            for (index, workspace) in self.workspaces().iter().enumerate() {
                content.extend(child_lines(workspace, index, child, inner));
            }
        }
        content
    }
}

/// One workspace row at wide inner widths; a name line and a state line when
/// the lane is too narrow for columns.
fn child_lines(
    workspace: &Workspace,
    index: usize,
    selected: Option<usize>,
    inner: usize,
) -> Vec<Line<'static>> {
    let state = if workspace.wait.is_some() {
        "wait"
    } else {
        workspace.state.as_str()
    };
    let hold = match workspace.hold {
        Hold::Live => Some("live"),
        Hold::Session => Some("session"),
        Hold::Idle => None,
    };
    let age = relative_age(workspace.age);
    let child_style = if selected == Some(index) {
        reverse_style()
    } else {
        dim_style()
    };
    if inner >= 28 {
        let mut suffix = format!(" {state}");
        if let Some(hold) = hold {
            suffix.push(' ');
            suffix.push_str(hold);
        }
        suffix.push(' ');
        suffix.push_str(&age);
        let glyph = "▸ ";
        let name_width = inner
            .saturating_sub(display_width(glyph) + display_width(&suffix))
            .max(1);
        let name = truncate(&workspace.workspace, name_width);
        let text = truncate(&format!("{glyph}{name}{suffix}"), inner);
        vec![Line::styled(text, child_style)]
    } else {
        let name = truncate(&workspace.workspace, inner.saturating_sub(2));
        let mut details = state.to_string();
        if let Some(hold) = hold {
            details.push(' ');
            details.push_str(hold);
        }
        details.push(' ');
        details.push_str(&age);
        let name_line = Line::styled(truncate(&format!("▸ {name}"), inner), child_style);
        let detail_line = Line::styled(truncate(&format!("  {details}"), inner), dim_style());
        vec![name_line, detail_line]
    }
}

fn sort_cards(cards: &mut [Card], sort: Sort, now: i64) {
    cards.sort_by(|a, b| {
        let order = match sort {
            Sort::Attention => attention_key(a).cmp(&attention_key(b)),
            Sort::Title => std::cmp::Ordering::Equal,
            Sort::Updated => b
                .activity_time(now)
                .unwrap_or(0)
                .cmp(&a.activity_time(now).unwrap_or(0)),
        };
        order
            .then_with(|| a.title().to_lowercase().cmp(&b.title().to_lowercase()))
            .then_with(|| a.title().cmp(b.title()))
            .then_with(|| a.key.cmp(&b.key))
    });
}

fn attention_key(card: &Card) -> (u8, Reverse<i64>) {
    if let Some(wait) = card.oldest_wait() {
        return (0, Reverse(wait.age));
    }
    if card
        .workspaces()
        .iter()
        .any(|workspace| workspace.state == State::Conflict)
    {
        return (1, Reverse(0));
    }
    if card
        .workspaces()
        .iter()
        .any(|workspace| workspace.state == State::Unpushed)
    {
        return (2, Reverse(0));
    }
    if card
        .workspaces()
        .iter()
        .any(|workspace| workspace.hold == Hold::Live)
    {
        return (3, Reverse(0));
    }
    if card
        .workspaces()
        .iter()
        .any(|workspace| workspace.state == State::InFlight)
    {
        return (4, Reverse(0));
    }
    if card.workspaces().is_empty() {
        return (7, Reverse(0));
    }
    if card
        .workspaces()
        .iter()
        .any(|workspace| workspace.clean.is_some())
    {
        return (6, Reverse(0));
    }
    (5, Reverse(0))
}

fn accent_color(card: &Card) -> Color {
    if card.oldest_wait().is_some()
        || card
            .workspaces()
            .iter()
            .any(|workspace| workspace.state == State::Conflict)
    {
        Color::Red
    } else if card
        .workspaces()
        .iter()
        .any(|workspace| workspace.state == State::Unpushed)
    {
        Color::Yellow
    } else if card
        .workspaces()
        .iter()
        .any(|workspace| workspace.hold == Hold::Live)
    {
        Color::Green
    } else if card
        .workspaces()
        .iter()
        .any(|workspace| workspace.state == State::InFlight)
    {
        Color::Blue
    } else if card
        .workspaces()
        .iter()
        .any(|workspace| workspace.clean.is_some())
    {
        Color::Magenta
    } else {
        Color::DarkGray
    }
}

fn accent_style(card: &Card, selected: bool, palette: Palette) -> Style {
    let mut style = palette.fg(accent_color(card));
    if selected {
        style = style.add_modifier(Modifier::BOLD);
    }
    style
}

/// The two styles shared by card bodies and the frame chrome; `ui` imports them
/// so a card title and a lane header cannot drift apart.
pub fn title_style() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

pub fn dim_style() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

fn reverse_style() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

fn border_line(
    left: &'static str,
    right: &'static str,
    inner: usize,
    accent: Style,
    border: Style,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(left, accent),
        Span::styled("─".repeat(inner), border),
        Span::styled(right, border),
    ])
}

fn box_line(line: Line<'static>, inner: usize, left: Style, right: Style) -> Line<'static> {
    let style = line.style;
    let content: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|span| {
            if span.style == Style::default() {
                Span::styled(span.content, style)
            } else {
                span
            }
        })
        .collect();
    let used: usize = content
        .iter()
        .map(|span| display_width(span.content.as_ref()))
        .sum();
    let mut spans = Vec::with_capacity(content.len() + 3);
    spans.push(Span::styled("│", left));
    spans.extend(content);
    if used < inner {
        spans.push(Span::raw(" ".repeat(inner - used)));
    }
    spans.push(Span::styled("│", right));
    Line::from(spans)
}

/// Join `parts` with the board separator, dropping any part that no longer
/// fits. At least the first part is kept, truncated if it is too long.
fn join_fitting(parts: &[String], width: usize) -> String {
    let mut out = String::new();
    for part in parts {
        let candidate = if out.is_empty() {
            part.clone()
        } else {
            format!("{out} · {part}")
        };
        if display_width(&candidate) <= width {
            out = candidate;
        } else if out.is_empty() {
            out = truncate(part, width);
        }
    }
    out
}

pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

pub fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if display_width(text) <= width {
        return text.to_string();
    }
    ellipsize(text, width)
}

fn ellipsize(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + char_width + 1 > width {
            break;
        }
        out.push(ch);
        used += char_width;
    }
    out.push('…');
    out
}

/// Wrap at spaces where possible, hard-splitting a word wider than the line.
/// The last line carries an ellipsis when text did not fit in `max_lines`.
pub fn wrap(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    if width == 0 || max_lines == 0 {
        return Vec::new();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut lines = Vec::new();
    let mut start = 0;
    let mut truncated = false;
    while start < chars.len() && lines.len() < max_lines {
        let mut used = 0;
        let mut end = start;
        let mut last_space = None;
        let mut overflow = false;
        while end < chars.len() {
            let char_width = UnicodeWidthChar::width(chars[end]).unwrap_or(0);
            if used + char_width > width && end > start {
                overflow = true;
                break;
            }
            used += char_width;
            if chars[end] == ' ' {
                last_space = Some(end);
            }
            end += 1;
            if used >= width {
                break;
            }
        }
        if end == start {
            end = start + 1;
        } else if end < chars.len() && (overflow || chars[end] != ' ') {
            if let Some(space) = last_space {
                if space > start {
                    end = space;
                }
            }
        }
        let line: String = chars[start..end].iter().collect();
        let line = line.trim_end().to_string();
        if !line.is_empty() {
            lines.push(line);
        }
        start = end;
        while start < chars.len() && chars[start] == ' ' {
            start += 1;
        }
    }
    if start < chars.len() {
        truncated = true;
    }
    if truncated {
        if let Some(last) = lines.last_mut() {
            *last = ellipsize(last, width);
        }
    }
    lines
}

pub fn relative_age(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        "now".to_string()
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else if seconds < 604_800 {
        format!("{}d", seconds / 86_400)
    } else if seconds < 2_592_000 {
        format!("{}w", seconds / 604_800)
    } else if seconds < 31_536_000 {
        format!("{}mo", seconds / 2_592_000)
    } else {
        format!("{}y", seconds / 31_536_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(name: &str, state: State, hold: Hold) -> Workspace {
        Workspace {
            workspace: name.to_string(),
            state,
            hold,
            ..Workspace::default()
        }
    }

    fn card(id: &str, title: &str, workspaces: Vec<Workspace>) -> Card {
        let view = TaskView {
            id: Some(id.to_string()),
            title: title.to_string(),
            workspaces,
        };
        Card {
            key: CardKey::Task(id.to_string()),
            view,
            task: None,
        }
    }

    #[test]
    fn relative_ages_use_the_largest_unit() {
        assert_eq!(relative_age(0), "now");
        assert_eq!(relative_age(59), "now");
        assert_eq!(relative_age(60), "1m");
        assert_eq!(relative_age(4 * 3_600), "4h");
        assert_eq!(relative_age(2 * 86_400), "2d");
        assert_eq!(relative_age(8 * 86_400), "1w");
        assert_eq!(relative_age(40 * 86_400), "1mo");
        assert_eq!(relative_age(400 * 86_400), "1y");
    }

    #[test]
    fn wrapping_breaks_at_spaces_and_splits_long_words() {
        assert_eq!(
            wrap("Fix the board cache", 12, 2),
            vec!["Fix the", "board cache"]
        );
        assert_eq!(wrap("one two three", 7, 1), vec!["one tw…"]);
        assert_eq!(wrap("日本語", 5, 2), vec!["日本", "語"]);
        assert_eq!(
            wrap("supercalifragilistice", 10, 2),
            vec!["supercalif", "ragilisti…"]
        );
    }

    #[test]
    fn truncation_reserves_the_ellipsis() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("日本語", 4), "日…");
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("abc", 0), "");
    }

    #[test]
    fn attention_order_puts_waits_first_and_titles_last() {
        let mut cards = vec![
            card(
                "merged",
                "Merged",
                vec![workspace("m", State::Merged, Hold::Idle)],
            ),
            card(
                "live",
                "Live",
                vec![workspace("l", State::Empty, Hold::Live)],
            ),
            card(
                "unpushed",
                "Unpushed",
                vec![workspace("u", State::Unpushed, Hold::Idle)],
            ),
            card(
                "conflict",
                "Conflict",
                vec![workspace("c", State::Conflict, Hold::Idle)],
            ),
            card(
                "inflight",
                "In flight",
                vec![workspace("i", State::InFlight, Hold::Idle)],
            ),
        ];
        let mut waiting = workspace("w", State::Empty, Hold::Idle);
        waiting.wait = Some(Wait {
            reason: "review".into(),
            age: 3 * 86_400,
            days: 3,
        });
        cards.push(card("waiting", "Waiting", vec![waiting]));
        sort_cards(&mut cards, Sort::Attention, 10_000);
        let order: Vec<&str> = cards.iter().filter_map(|card| card.id()).collect();
        assert_eq!(
            order,
            vec![
                "waiting", "conflict", "unpushed", "live", "inflight", "merged"
            ]
        );
    }

    #[test]
    fn aggregates_count_states_and_holds() {
        let mut merged = workspace("m1", State::Merged, Hold::Live);
        merged.session = Some(crate::model::Session {
            title: Some("Board polish".into()),
            updated: Some(9_000),
        });
        let mut merged2 = workspace("m2", State::Merged, Hold::Session);
        merged2.session = Some(crate::model::Session {
            title: Some("Older".into()),
            updated: Some(8_000),
        });
        let mut unpushed = workspace("u", State::Unpushed, Hold::Idle);
        unpushed.clean = Some(Clean { age: 7_200 });
        let card = card("cache", "Cache", vec![merged, merged2, unpushed]);
        let badges = card.badges(80).unwrap();
        assert!(badges.contains("1 unpushed"));
        assert!(badges.contains("2 merged"));
        assert!(badges.contains("clean 2h"));
        assert!(card.badges(10).is_some());
        assert_eq!(card.oldest_clean().unwrap().age, 7_200);
        assert_eq!(card.context().unwrap(), "session \"Board polish\"");
        assert_eq!(card.holds_label().unwrap(), "1 live · 1 session");
    }

    #[test]
    fn activity_prefers_the_newest_task_or_session_time() {
        let mut workspace = workspace("w", State::Unpushed, Hold::Idle);
        workspace.age = 4 * 3_600;
        workspace.session = Some(crate::model::Session {
            title: Some("Recent".into()),
            updated: Some(9_000),
        });
        let mut card = card("cache", "Cache", vec![workspace]);
        card.task = Some(Task {
            id: "cache".into(),
            title: "Cache".into(),
            status: "open".into(),
            updated: 8_000,
            created: 1_000,
            ..Task::default()
        });
        assert_eq!(card.activity_time(10_000), Some(9_000));
        assert_eq!(card.activity_age(10_000), Some(1_000));
    }

    #[test]
    fn filters_match_presets_and_query() {
        let mut waiting_ws = workspace("ws-wait", State::Empty, Hold::Idle);
        waiting_ws.wait = Some(Wait {
            reason: "review".into(),
            age: 3 * 86_400,
            days: 3,
        });
        let waiting = card("wait", "Wait", vec![waiting_ws]);
        let conflict = card(
            "conflict",
            "Conflict",
            vec![workspace("ws-conflict", State::Conflict, Hold::Idle)],
        );
        let live = card(
            "live",
            "Live",
            vec![workspace("ws-live", State::Empty, Hold::Live)],
        );
        let unpushed = card(
            "unpushed",
            "Unpushed",
            vec![workspace("ws-unpushed", State::Unpushed, Hold::Idle)],
        );
        let mut clean_ws = workspace("ws-clean", State::Merged, Hold::Idle);
        clean_ws.clean = Some(Clean { age: 1_000 });
        let clean = card("clean", "Clean", vec![clean_ws]);
        let stale = card(
            "stale",
            "Stale",
            vec![workspace("ws-stale", State::Merged, Hold::Idle)],
        );

        assert!(Filter::All.matches(&waiting));
        assert!(Filter::Attention.matches(&waiting));
        assert!(Filter::Attention.matches(&conflict));
        assert!(!Filter::Live.matches(&waiting));
        assert!(Filter::Live.matches(&live));
        assert!(Filter::Unpushed.matches(&unpushed));
        assert!(Filter::Reclaimable.matches(&clean));
        assert!(Filter::Reclaimable.matches(&stale));
        assert!(!Filter::Reclaimable.matches(&unpushed));

        let mut ws = workspace("ws-cache-fix", State::Unpushed, Hold::Idle);
        ws.description = "fix the cache".into();
        ws.session = Some(crate::model::Session {
            title: Some("Board polish".into()),
            updated: Some(9_000),
        });
        let mut searchable = card("cache", "Fix the board cache", vec![ws]);
        searchable.task = Some(Task {
            id: "cache".into(),
            title: "Fix the board cache".into(),
            status: "open".into(),
            notes: "design review".into(),
            ..Task::default()
        });
        assert!(matches_query(&searchable, "cache"));
        assert!(matches_query(&searchable, "#cache"));
        assert!(matches_query(&searchable, "BOARD"));
        assert!(matches_query(&searchable, "ws-cache-fix"));
        assert!(matches_query(&searchable, "polish"));
        assert!(matches_query(&searchable, "design"));
        assert!(!matches_query(&searchable, "nope"));
        assert!(matches_query(&searchable, ""));
    }

    #[test]
    fn sort_title_and_updated_are_presentation_order() {
        let mut alpha = card(
            "alpha",
            "Alpha",
            vec![workspace("a", State::Merged, Hold::Idle)],
        );
        alpha.task = Some(Task {
            id: "alpha".into(),
            title: "Alpha".into(),
            status: "open".into(),
            updated: 2_000,
            ..Task::default()
        });
        let mut beta = card(
            "beta",
            "Beta",
            vec![workspace("b", State::Merged, Hold::Idle)],
        );
        beta.task = Some(Task {
            id: "beta".into(),
            title: "Beta".into(),
            status: "open".into(),
            updated: 1_000,
            ..Task::default()
        });
        let mut cards = vec![beta, alpha];
        sort_cards(&mut cards, Sort::Title, 10_000);
        let order: Vec<&str> = cards.iter().filter_map(|card| card.id()).collect();
        assert_eq!(order, vec!["alpha", "beta"]);
        sort_cards(&mut cards, Sort::Updated, 10_000);
        let order: Vec<&str> = cards.iter().filter_map(|card| card.id()).collect();
        assert_eq!(order, vec!["alpha", "beta"]);
    }

    #[test]
    fn build_filters_cards_before_sorting() {
        let mut waiting_ws = workspace("ws-wait", State::Empty, Hold::Idle);
        waiting_ws.task = Some("wait".into());
        waiting_ws.wait = Some(Wait {
            reason: "review".into(),
            age: 3_600,
            days: 0,
        });
        let mut live_ws = workspace("ws-live", State::Empty, Hold::Live);
        live_ws.task = Some("live".into());
        let board = Board {
            workspaces: vec![waiting_ws, live_ws],
            tasks: vec![
                Task {
                    id: "wait".into(),
                    title: "Wait".into(),
                    status: "open".into(),
                    workspaces: vec!["ws-wait".into()],
                    ..Task::default()
                },
                Task {
                    id: "live".into(),
                    title: "Live".into(),
                    status: "open".into(),
                    workspaces: vec!["ws-live".into()],
                    ..Task::default()
                },
            ],
        };
        let lanes = build(&board, 10_000, Filter::Attention, "", Sort::Attention);
        let cards: usize = lanes.iter().map(|lane| lane.cards.len()).sum();
        assert_eq!(cards, 1);
        let blocked = lanes
            .iter()
            .find(|lane| lane.lane == Lane::Blocked)
            .unwrap();
        assert_eq!(blocked.cards[0].id(), Some("wait"));

        let lanes = build(&board, 10_000, Filter::All, "live", Sort::Title);
        let cards: usize = lanes.iter().map(|lane| lane.cards.len()).sum();
        assert_eq!(cards, 1);
        assert!(
            lanes
                .iter()
                .any(|lane| lane.cards.iter().any(|card| card.id() == Some("live")))
        );
    }

    #[test]
    fn presets_and_sorts_cycle() {
        assert_eq!(Filter::All.next(), Filter::Attention);
        assert_eq!(Filter::Reclaimable.next(), Filter::All);
        assert_eq!(Filter::Attention.label(), "attention");
        assert_eq!(Sort::Attention.next(), Sort::Title);
        assert_eq!(Sort::Updated.next(), Sort::Attention);
        assert_eq!(Sort::Title.label(), "title");
    }
}
