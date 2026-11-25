// Rendering only: the derived lanes as cards, the header summary, the focused
// lane and its bar on narrow terminals, the detail pane, and the modals.
use crate::app::{App, Mode};
use crate::model::{Board, Hold, Lane, State, Workspace};
use crate::view::{dim_style, relative_age, title_style, truncate, wrap};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    if area.width < 50 || area.height < 12 {
        render_too_small(frame, area);
        return;
    }

    let detail_right = app.detail && area.width >= 143;
    let detail_bottom = app.detail && !detail_right && area.width >= 70 && area.height >= 24;
    let detail_allowed = detail_right || detail_bottom;
    // The right pane and its gutter take 33 columns. Decide between five lanes
    // and one focused lane from what the board actually keeps.
    let board_width = if detail_right {
        area.width.saturating_sub(33)
    } else {
        area.width
    };
    let wide_terminal = board_width >= 110;
    let (header, lane_bar, body, footer) = if wide_terminal {
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);
        (chunks[0], None, chunks[1], chunks[2])
    } else {
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);
        (chunks[0], Some(chunks[1]), chunks[2], chunks[3])
    };

    render_title(frame, header, app);
    if let Some(bar) = lane_bar {
        render_lane_bar(frame, bar, app);
    }

    let (board_area, detail_area) = if detail_right {
        let parts = Layout::horizontal([Constraint::Min(0), Constraint::Length(32)])
            .spacing(1)
            .split(body);
        (parts[0], Some(parts[1]))
    } else if detail_bottom {
        let parts = Layout::vertical([Constraint::Min(0), Constraint::Length(8)]).split(body);
        (parts[0], Some(parts[1]))
    } else {
        (body, None)
    };

    render_lanes(frame, board_area, app, wide_terminal);
    if let Some(detail) = detail_area {
        render_detail(frame, detail, app);
    }
    render_footer(frame, footer, app, detail_allowed);

    if app.mode == Mode::Help {
        render_help(frame, area, &mut app.help_scroll);
    } else {
        match &app.mode {
            Mode::TaskInput { text } => render_input(frame, area, text),
            Mode::Assign {
                workspace,
                selected,
            } => render_assign(frame, area, app, workspace, *selected),
            Mode::Parent { child, selected } => render_parent(frame, area, app, child, *selected),
            Mode::Filter { .. } => render_filter(frame, area, &app.query),
            Mode::Normal | Mode::Help => {}
        }
    }
}

#[derive(Default)]
struct Summary {
    cards: usize,
    workspaces: usize,
    live: usize,
    session: usize,
    wait: usize,
    conflict: usize,
    stale: usize,
}

fn summary(app: &App) -> Summary {
    let mut summary = Summary::default();
    for lane in &app.lanes {
        for card in &lane.cards {
            summary.cards += card.node_count();
            for workspace in card.all_workspaces() {
                summary.workspaces += 1;
                match workspace.hold {
                    Hold::Live => summary.live += 1,
                    Hold::Session => summary.session += 1,
                    Hold::Idle => {}
                }
                if workspace.wait.is_some() {
                    summary.wait += 1;
                }
                if workspace.state == State::Conflict {
                    summary.conflict += 1;
                }
                if workspace.hold == Hold::Idle
                    && workspace.wait.is_none()
                    && matches!(workspace.state, State::Empty | State::Merged)
                {
                    summary.stale += 1;
                }
            }
        }
    }
    summary
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn refresh_age(app: &App) -> String {
    let seconds = (app.clock - app.now).max(0);
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        relative_age(seconds)
    }
}

fn render_title(frame: &mut Frame, area: Rect, app: &App) {
    if app.has_load_error() {
        let text = truncate(&format!("! {}", app.status), area.width as usize);
        frame.render_widget(
            Paragraph::new(Line::styled(
                text,
                app.palette.error().add_modifier(Modifier::BOLD),
            )),
            area,
        );
        return;
    }
    let summary = summary(app);
    let age = refresh_age(app);
    let cards = plural(summary.cards, "card");
    let workspaces = if area.width >= 60 {
        plural(summary.workspaces, "workspace")
    } else {
        format!("{} ws", summary.workspaces)
    };
    let mut spans = vec![
        Span::styled("rookery board", title_style()),
        Span::raw(format!("  {cards}  {workspaces}")),
    ];
    if area.width >= 110 {
        spans.push(Span::styled(
            format!(
                "   live {}  session {}  wait {}  conflict {}  stale {}",
                summary.live, summary.session, summary.wait, summary.conflict, summary.stale
            ),
            dim_style(),
        ));
    }
    spans.push(Span::styled(format!("   refreshed {age}"), dim_style()));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_lane_bar(frame: &mut Frame, area: Rect, app: &App) {
    let full = area.width >= 70;
    let mut spans = Vec::new();
    for (index, lane) in app.lanes.iter().enumerate() {
        let label = if full {
            format!(
                "{} {}",
                lane.lane.title(),
                lane.cards
                    .iter()
                    .map(|card| card.node_count())
                    .sum::<usize>()
            )
        } else {
            format!(
                "{}{}",
                short_lane(lane.lane),
                lane.cards
                    .iter()
                    .map(|card| card.node_count())
                    .sum::<usize>()
            )
        };
        let text = if index == app.cursor.lane {
            format!("[{label}]")
        } else {
            format!(" {label} ")
        };
        let style = if index == app.cursor.lane {
            Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            dim_style()
        };
        spans.push(Span::styled(text, style));
        if index + 1 < app.lanes.len() {
            spans.push(Span::raw(" "));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn short_lane(lane: Lane) -> &'static str {
    match lane {
        Lane::Backlog => "B",
        Lane::Active => "A",
        Lane::Review => "R",
        Lane::Blocked => "X",
        Lane::ToClean => "C",
    }
}

fn lane_meaning(lane: Lane) -> &'static str {
    match lane {
        Lane::Backlog => "no workspaces yet",
        Lane::Active => "no live or unpushed work",
        Lane::Review => "no merged or in-flight work",
        Lane::Blocked => "no waits or conflicts",
        Lane::ToClean => "no cleanup marks",
    }
}

fn render_lanes(frame: &mut Frame, area: Rect, app: &mut App, wide_terminal: bool) {
    if app.lanes.iter().all(|lane| lane.cards.is_empty()) {
        if app.has_filter() && board_has_cards(&app.board) {
            render_no_match(frame, area, app);
        } else {
            render_empty(frame, area);
        }
        return;
    }
    if wide_terminal {
        let columns = Layout::horizontal([Constraint::Ratio(1, 5); 5])
            .spacing(1)
            .split(area);
        for index in 0..app.lanes.len() {
            render_lane(frame, columns[index], app, index, true);
        }
    } else {
        let index = app.cursor.lane;
        render_lane(frame, area, app, index, false);
    }
}

fn board_has_cards(board: &Board) -> bool {
    board.lanes().iter().any(|lane| !lane.tasks.is_empty())
}

fn render_empty(frame: &mut Frame, area: Rect) {
    render_message(
        frame,
        area,
        vec![
            Line::from("No tasks yet.").alignment(Alignment::Center),
            Line::from("Press n to create one.").alignment(Alignment::Center),
        ],
    );
}

fn render_no_match(frame: &mut Frame, area: Rect, app: &App) {
    let label = if app.query.trim().is_empty() {
        app.filter.label().to_string()
    } else {
        app.query.trim().to_string()
    };
    render_message(
        frame,
        area,
        vec![
            Line::from(format!("No tasks match \"{}\"", truncate(&label, 24)))
                .alignment(Alignment::Center),
            Line::from("esc clears the filter").alignment(Alignment::Center),
        ],
    );
}

fn render_message(frame: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
    let height = (lines.len() as u16).min(area.height);
    let y = area.y + area.height.saturating_sub(height) / 2;
    frame.render_widget(
        Paragraph::new(lines),
        Rect::new(area.x, y, area.width, height),
    );
}

fn render_lane(frame: &mut Frame, area: Rect, app: &mut App, index: usize, wide_terminal: bool) {
    // A bordered card needs a real column to breathe; a compact stripe card
    // keeps five lanes legible in the 110-149 range. Narrow focused terminals
    // (below 70) keep the denser compact card they were designed with.
    let card_wide = area.width >= 29 && (wide_terminal || area.width >= 70);
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
    let selected = app.cursor.lane == index;
    let label = {
        let lane = &app.lanes[index];
        let cards: usize = lane.cards.iter().map(|card| card.node_count()).sum();
        let workspaces: usize = lane.cards.iter().map(|card| card.workspace_count()).sum();
        truncate(
            &format!("{} {} · {}ws", lane.lane.title(), cards, workspaces),
            area.width as usize,
        )
    };
    let mut header_style = title_style();
    if selected {
        header_style = header_style.add_modifier(Modifier::REVERSED);
    }
    frame.render_widget(Paragraph::new(Line::styled(label, header_style)), rows[0]);

    let height = rows[1].height as usize;
    let full_width = rows[1].width as usize;
    let (mut lines, mut cursor) = lane_lines(app, index, full_width as u16, card_wide);
    let mut scrollbar = lines.len() > height && full_width > 1;
    if scrollbar {
        let (narrow_lines, narrow_cursor) =
            lane_lines(app, index, (full_width - 1) as u16, card_wide);
        lines = narrow_lines;
        cursor = narrow_cursor;
        scrollbar = lines.len() > height;
    }
    let content = lines.len();
    let body_width = if scrollbar {
        full_width - 1
    } else {
        full_width
    };
    let body = Rect::new(rows[1].x, rows[1].y, body_width as u16, rows[1].height);
    let max_offset = content.saturating_sub(height);
    let mut offset = app.scroll[index].min(max_offset);
    let viewport_changed = app.viewports[index] != height;
    if selected && (app.reveal || viewport_changed) {
        offset = reveal(offset, cursor, height, content);
    }
    app.scroll[index] = offset;
    app.viewports[index] = height;
    app.content_heights[index] = content;
    frame.render_widget(
        Paragraph::new(lines).scroll((offset.min(u16::MAX as usize) as u16, 0)),
        body,
    );
    if scrollbar {
        let mut state = ScrollbarState::new(content)
            .position(offset)
            .viewport_content_length(height);
        let scroll_area = Rect::new(body.right(), body.y, 1, body.height);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .style(app.palette.fg(Color::DarkGray)),
            scroll_area,
            &mut state,
        );
    }
    if selected {
        app.reveal = false;
    }
}

const CHILD_INDENT: usize = 2;

fn lane_lines(
    app: &App,
    index: usize,
    width: u16,
    wide: bool,
) -> (Vec<Line<'static>>, Option<(usize, usize)>) {
    let lane = &app.lanes[index];
    if lane.cards.is_empty() {
        return (
            vec![Line::styled(
                truncate(&format!("({})", lane_meaning(lane.lane)), width as usize),
                dim_style(),
            )],
            None,
        );
    }
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cursor = None;
    for (card_index, root) in lane.cards.iter().enumerate() {
        let root_selected =
            app.cursor.lane == index && app.cursor.card == card_index && app.cursor.node == 0;
        let root_expanded = app.expanded.contains(&root.key);
        let start = lines.len();
        lines.extend(card_lines(
            root,
            width as usize,
            root_selected,
            root_expanded,
            if root_selected {
                app.cursor.child
            } else {
                None
            },
            wide,
            app.now,
            app.palette,
        ));
        if root_selected {
            cursor = Some((start, lines.len()));
        }
        if !root_expanded {
            continue;
        }
        let child_width = (width as usize).saturating_sub(CHILD_INDENT);
        for (child_index, child) in root.children().iter().enumerate() {
            let child_selected = app.cursor.lane == index
                && app.cursor.card == card_index
                && app.cursor.node == child_index + 1;
            let child_expanded = app.expanded.contains(&child.key);
            let child_start = lines.len();
            let child_lines = card_lines(
                child,
                child_width,
                child_selected,
                child_expanded,
                if child_selected {
                    app.cursor.child
                } else {
                    None
                },
                wide,
                app.now,
                app.palette,
            );
            lines.extend(indent_lines(child_lines, CHILD_INDENT));
            if child_selected {
                cursor = Some((child_start, lines.len()));
            }
        }
    }
    (lines, cursor)
}

#[allow(clippy::too_many_arguments)]
fn card_lines(
    card: &crate::view::Card,
    width: usize,
    selected: bool,
    expanded: bool,
    child: Option<usize>,
    wide: bool,
    now: i64,
    palette: crate::view::Palette,
) -> Vec<Line<'static>> {
    if width < 4 {
        return Vec::new();
    }
    if wide {
        card.lines(width, selected, expanded, child, now, palette)
    } else {
        card.compact_lines(width, selected, expanded, child, palette)
    }
}

fn indent_lines(mut lines: Vec<Line<'static>>, indent: usize) -> Vec<Line<'static>> {
    if indent == 0 {
        return lines;
    }
    let prefix = Span::raw(" ".repeat(indent));
    for line in &mut lines {
        line.spans.insert(0, prefix.clone());
    }
    lines
}

fn reveal(offset: usize, range: Option<(usize, usize)>, viewport: usize, content: usize) -> usize {
    let max_offset = content.saturating_sub(viewport);
    let mut offset = offset.min(max_offset);
    if viewport == 0 {
        return 0;
    }
    if let Some((start, end)) = range {
        if end - start >= viewport {
            offset = start.min(max_offset);
        } else if start < offset {
            offset = start;
        } else if end > offset + viewport {
            offset = end - viewport;
        }
    }
    offset.min(max_offset)
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App, detail_allowed: bool) {
    let (text, style) = match &app.mode {
        Mode::Help => ("j k scroll  esc close".to_string(), dim_style()),
        Mode::TaskInput { .. } | Mode::Assign { .. } | Mode::Parent { .. } => {
            ("enter confirm   esc cancel".to_string(), dim_style())
        }
        Mode::Filter { .. } => ("enter keep   esc cancel".to_string(), dim_style()),
        Mode::Normal => {
            if app.has_load_error() {
                ("r retry".to_string(), app.palette.error())
            } else if !app.status.is_empty() {
                if app.error {
                    (format!("! {}", app.status), app.palette.error())
                } else {
                    (app.status.clone(), Style::default())
                }
            } else if app.detail && !detail_allowed {
                ("detail needs 70x24  (i to close)".to_string(), dim_style())
            } else {
                (hint(app), dim_style())
            }
        }
    };
    frame.render_widget(Paragraph::new(Line::styled(text, style)), area);
}

fn hint(app: &App) -> String {
    let Some(card) = app.selected_card() else {
        return "n new  ? help  q quit".to_string();
    };
    if card.id().is_some() {
        if card.workspaces().is_empty() && card.children().is_empty() {
            "s seat  n new  i detail  ? help".to_string()
        } else if app.expanded.contains(&card.key) {
            "enter open  d mark  space collapse  i detail  ? help".to_string()
        } else {
            "enter open  d mark  space expand  i detail  ? help".to_string()
        }
    } else {
        "enter open  d mark  a assign  space expand  ? help".to_string()
    }
}

fn detail_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let Some(card) = app.selected_card() else {
        return vec![Line::styled("(no selection)", dim_style())];
    };
    let mut lines = Vec::new();
    let mut hints = vec!["enter open", "d mark"];
    if card.id().is_some() {
        if card.workspaces().is_empty() && card.children().is_empty() {
            hints.push("s seat");
        }
        hints.push("p parent");
    } else {
        hints.push("a assign");
    }
    lines.push(Line::styled(
        truncate(&hints.join("  "), width),
        dim_style(),
    ));

    if let Some(id) = card.id() {
        lines.push(Line::styled(
            truncate(&format!("#{id}"), width),
            dim_style(),
        ));
    }
    lines.push(Line::styled(truncate(card.title(), width), title_style()));
    if let Some(parent) = card.parent() {
        lines.push(Line::styled(
            truncate(&format!("child of {parent}"), width),
            dim_style(),
        ));
    }
    if let Some(task) = &card.task {
        let mut meta = Vec::new();
        if task.created > 0 {
            meta.push(format!("created {}", relative_age(app.now - task.created)));
        }
        if task.updated > 0 {
            meta.push(format!("updated {}", relative_age(app.now - task.updated)));
        }
        if !meta.is_empty() {
            lines.push(Line::styled(
                truncate(&meta.join(" · "), width),
                dim_style(),
            ));
        }
        let notes = task.notes.trim();
        if !notes.is_empty() {
            lines.push(Line::styled("notes", dim_style()));
            for line in wrap(notes, width, 4) {
                lines.push(Line::styled(line, dim_style()));
            }
        }
    }

    if !card.children().is_empty() {
        lines.push(Line::styled("children", dim_style()));
        for child in card.children() {
            let id = child.id().unwrap_or("?");
            lines.push(Line::styled(
                truncate(
                    &format!(
                        "↳ {id}  {}  ({} ws)",
                        child.title(),
                        child.workspaces().len()
                    ),
                    width,
                ),
                dim_style(),
            ));
        }
    }

    let selected = app
        .cursor
        .child
        .and_then(|index| card.workspaces().get(index));
    let workspaces: Vec<&Workspace> = match selected {
        Some(workspace) => vec![workspace],
        None => card.workspaces().iter().collect(),
    };
    for workspace in workspaces {
        let marker = if app.cursor.child.is_some() {
            "▸ "
        } else {
            ""
        };
        lines.push(Line::styled(
            truncate(&format!("{marker}{}", workspace.workspace), width),
            title_style(),
        ));
        let state = if workspace.wait.is_some() {
            "wait"
        } else {
            workspace.state.as_str()
        };
        let mut meta = vec![state.to_string()];
        if workspace.hold != Hold::Idle {
            meta.push(workspace.hold.as_str().to_string());
        }
        meta.push(relative_age(workspace.age));
        lines.push(Line::styled(
            truncate(&meta.join(" · "), width),
            dim_style(),
        ));
        if !workspace.description.trim().is_empty() {
            lines.push(Line::styled(
                truncate(workspace.description.trim(), width),
                dim_style(),
            ));
        }
        if let Some(session) = &workspace.session {
            if let Some(title) = session
                .title
                .as_deref()
                .filter(|title| !title.trim().is_empty())
            {
                let age = session
                    .updated
                    .filter(|updated| *updated > 0)
                    .map(|updated| format!(" {}", relative_age(app.now - updated)))
                    .unwrap_or_default();
                lines.push(Line::styled(
                    truncate(&format!("session \"{title}\"{age}"), width),
                    dim_style(),
                ));
            }
        }
        if let Some(wait) = &workspace.wait {
            let reason = if wait.reason.trim().is_empty() {
                "(no reason)"
            } else {
                wait.reason.trim()
            };
            lines.push(Line::styled(
                truncate(
                    &format!("wait: {reason}  {}", relative_age(wait.age)),
                    width,
                ),
                dim_style(),
            ));
        }
        if let Some(clean) = &workspace.clean {
            lines.push(Line::styled(
                truncate(&format!("clean {}", relative_age(clean.age)), width),
                dim_style(),
            ));
        }
        if !workspace.path.as_os_str().is_empty() {
            lines.push(Line::styled(
                truncate(&format!("path {}", workspace.path.display()), width),
                dim_style(),
            ));
        }
    }
    lines
}

fn render_detail(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title("detail");
    let inner = block.inner(area);
    let width = inner.width as usize;
    let mut lines = detail_lines(app, width);
    lines.truncate(inner.height as usize);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_help(frame: &mut Frame, area: Rect, help_scroll: &mut usize) {
    let width = 64.min(area.width);
    let height = 24.min(area.height).max(4);
    let rect = centered_rect(width, height, area);
    frame.render_widget(Clear, rect);
    let lines = help_lines();
    let inner_height = height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(inner_height);
    let scroll = (*help_scroll).min(max_scroll);
    *help_scroll = scroll;
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((scroll.min(u16::MAX as usize) as u16, 0))
            .block(Block::default().borders(Borders::ALL).title("keys")),
        rect,
    );
}

fn help_lines() -> Vec<Line<'static>> {
    vec![
        Line::raw("  enter   open the selected conversation's seat"),
        Line::raw("  d       mark the task (or this workspace) for cleanup"),
        Line::raw("  n       create a backlog task"),
        Line::raw("  s       start a seat for a task with no workspace"),
        Line::raw("  a       assign the selected ungrouped workspace"),
        Line::raw("  p       set or clear the selected task's parent"),
        Line::raw("  /       filter cards"),
        Line::raw("  f       cycle filter presets"),
        Line::raw("  space   expand or collapse the selected card"),
        Line::raw("  j k     move within a lane"),
        Line::raw("  h l     move between lanes"),
        Line::raw("  tab     cycle lanes"),
        Line::raw("  1-5     jump to a lane"),
        Line::raw("  g g     first card"),
        Line::raw("  G       last card"),
        Line::raw("  ctrl-d  half page down"),
        Line::raw("  ctrl-u  half page up"),
        Line::raw("  pgdn    page down"),
        Line::raw("  pgup    page up"),
        Line::raw("  o       cycle sort"),
        Line::raw("  i       toggle the detail pane"),
        Line::raw("  r       refresh now"),
        Line::raw("  ?       close this help"),
        Line::raw("  esc     clear filter or close detail"),
        Line::raw("  q       quit"),
        Line::styled(
            "  tasks group workspaces; lanes are derived, never stored",
            dim_style(),
        ),
    ]
}

fn render_filter(frame: &mut Frame, area: Rect, text: &str) {
    let rect = centered_rect(60, 3, area);
    frame.render_widget(Clear, rect);
    let line = Line::from(vec![
        Span::raw(text.to_string()),
        Span::styled("▏", dim_style()),
    ]);
    frame.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::ALL).title("filter")),
        rect,
    );
}

fn render_input(frame: &mut Frame, area: Rect, text: &str) {
    let rect = centered_rect(60, 3, area);
    frame.render_widget(Clear, rect);
    let line = Line::from(vec![
        Span::raw(text.to_string()),
        Span::styled("▏", dim_style()),
    ]);
    frame.render_widget(
        Paragraph::new(line).block(
            Block::default()
                .borders(Borders::ALL)
                .title("new task title"),
        ),
        rect,
    );
}

fn render_assign(frame: &mut Frame, area: Rect, app: &App, workspace: &str, selected: usize) {
    let tasks = app.open_tasks();
    let height = (tasks.len() as u16 + 2).clamp(3, 16);
    let rect = centered_rect(60, height, area);
    frame.render_widget(Clear, rect);
    let lines: Vec<Line> = if tasks.is_empty() {
        vec![Line::styled("(no open tasks)", dim_style())]
    } else {
        let visible = height.saturating_sub(2) as usize;
        let offset = if selected >= visible {
            selected + 1 - visible
        } else {
            0
        };
        tasks
            .iter()
            .enumerate()
            .skip(offset)
            .take(visible)
            .map(|(index, (id, title))| {
                let style = if index == selected {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                Line::styled(format!("{id}  {title}"), style)
            })
            .collect()
    };
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("assign {workspace} to a task")),
        ),
        rect,
    );
}

fn render_parent(frame: &mut Frame, area: Rect, app: &App, child: &str, selected: usize) {
    let parents = app.open_parents(child);
    let current = app.selected_card().and_then(|card| card.parent());
    let height = (parents.len() as u16 + 3).clamp(3, 16);
    let rect = centered_rect(60, height, area);
    frame.render_widget(Clear, rect);
    let visible = height.saturating_sub(2) as usize;
    let offset = if selected >= visible {
        selected + 1 - visible
    } else {
        0
    };
    let mut rows: Vec<Line> = Vec::new();
    for index in offset..=parents.len().min(offset + visible - 1) {
        let (label, is_current) = if index == 0 {
            ("— clear parent —".to_string(), current.is_none())
        } else {
            let (id, title) = parents[index - 1];
            (format!("{id}  {title}"), current == Some(id))
        };
        let marker = if is_current { "* " } else { "  " };
        let style = if index == selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        rows.push(Line::styled(format!("{marker}{label}"), style));
    }
    frame.render_widget(
        Paragraph::new(rows).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("parent of {child}")),
        ),
        rect,
    );
}

fn render_too_small(frame: &mut Frame, area: Rect) {
    let rect = centered_rect(34, 4, area);
    frame.render_widget(Clear, rect);
    let lines = vec![
        Line::from("Terminal too small").alignment(Alignment::Center),
        Line::from("need at least 50x12").alignment(Alignment::Center),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title("rookery board"),
        ),
        rect,
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width, height)
}

#[cfg(test)]
mod tests {
    use crate::app::App;
    use crate::model::{Board, Clean, Hold, Session, State, Task, Wait, Workspace};
    use crate::view::{CardKey, Palette};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    const NOW: i64 = 1_700_000_000;

    fn ws(name: &str, state: State, hold: Hold) -> Workspace {
        Workspace {
            workspace: name.to_string(),
            state,
            hold,
            ..Workspace::default()
        }
    }

    fn golden_board() -> Board {
        let mut workspaces = Vec::new();
        let mut tasks = Vec::new();

        tasks.push(Task {
            id: "plan".into(),
            title: "Plan the next release".into(),
            status: "open".into(),
            created: NOW - 3 * 86_400,
            updated: NOW - 2 * 86_400,
            ..Task::default()
        });

        let mut cache_fix = ws("ws-cache-fix", State::Merged, Hold::Live);
        cache_fix.task = Some("cache".into());
        cache_fix.age = 4 * 3_600;
        let mut cache_test = ws("ws-cache-test", State::Unpushed, Hold::Idle);
        cache_test.task = Some("cache".into());
        cache_test.age = 2 * 3_600;
        workspaces.extend([cache_fix, cache_test]);
        tasks.push(Task {
            id: "cache".into(),
            title: "Fix the board cache".into(),
            status: "open".into(),
            workspaces: vec!["ws-cache-fix".into(), "ws-cache-test".into()],
            created: NOW - 86_400,
            updated: NOW - 4 * 3_600,
            ..Task::default()
        });

        let mut loose = ws("ws-loose", State::Unpushed, Hold::Idle);
        loose.description = "Loose experiment".into();
        loose.age = 3_600;
        workspaces.push(loose);

        let mut polish = ws("ws-polish", State::Merged, Hold::Idle);
        polish.task = Some("polish".into());
        polish.age = 5_400;
        polish.session = Some(Session {
            title: Some("Board polish".into()),
            updated: Some(NOW - 1_800),
        });
        workspaces.push(polish);
        tasks.push(Task {
            id: "polish".into(),
            title: "Polish the cards".into(),
            status: "open".into(),
            workspaces: vec!["ws-polish".into()],
            created: NOW - 2 * 86_400,
            updated: NOW - 1_800,
            ..Task::default()
        });

        let mut design = ws("ws-design", State::Empty, Hold::Idle);
        design.task = Some("design".into());
        design.age = 3 * 86_400;
        design.wait = Some(Wait {
            reason: "design review".into(),
            age: 3 * 86_400,
            days: 3,
        });
        workspaces.push(design);
        tasks.push(Task {
            id: "design".into(),
            title: "Wait for the design review".into(),
            status: "open".into(),
            workspaces: vec!["ws-design".into()],
            created: NOW - 4 * 86_400,
            updated: NOW - 3 * 86_400,
            ..Task::default()
        });

        let mut old = ws("ws-old", State::Unpushed, Hold::Idle);
        old.task = Some("old".into());
        old.age = 4 * 3_600;
        old.clean = Some(Clean { age: 2 * 3_600 });
        workspaces.push(old);
        tasks.push(Task {
            id: "old".into(),
            title: "Clean the old seat".into(),
            status: "open".into(),
            workspaces: vec!["ws-old".into()],
            created: NOW - 5 * 86_400,
            updated: NOW - 2 * 3_600,
            ..Task::default()
        });

        Board { workspaces, tasks }
    }

    fn many_backlog_board(count: usize) -> Board {
        let tasks = (0..count)
            .map(|index| Task {
                id: format!("task-{index}"),
                title: format!("Task {index:02}"),
                status: "open".into(),
                created: NOW,
                updated: NOW,
                ..Task::default()
            })
            .collect();
        Board {
            workspaces: Vec::new(),
            tasks,
        }
    }

    fn tree_board() -> Board {
        let mut lead = ws("ws-lead", State::Merged, Hold::Live);
        lead.task = Some("lead".into());
        lead.age = 5_400;
        let mut worker_a = ws("ws-worker-a", State::Unpushed, Hold::Idle);
        worker_a.task = Some("worker-a".into());
        worker_a.age = 3_600;
        let mut worker_b = ws("ws-worker-b", State::Merged, Hold::Idle);
        worker_b.task = Some("worker-b".into());
        worker_b.age = 7_200;
        Board {
            workspaces: vec![lead, worker_a, worker_b],
            tasks: vec![
                Task {
                    id: "lead".into(),
                    title: "Lead the release".into(),
                    status: "open".into(),
                    workspaces: vec!["ws-lead".into()],
                    created: NOW - 5 * 86_400,
                    updated: NOW - 5_400,
                    ..Task::default()
                },
                Task {
                    id: "worker-a".into(),
                    title: "Draft the notes".into(),
                    status: "open".into(),
                    parent: Some("lead".into()),
                    workspaces: vec!["ws-worker-a".into()],
                    created: NOW - 4 * 86_400,
                    updated: NOW - 3_600,
                    ..Task::default()
                },
                Task {
                    id: "worker-b".into(),
                    title: "Check the build".into(),
                    status: "open".into(),
                    parent: Some("lead".into()),
                    workspaces: vec!["ws-worker-b".into()],
                    created: NOW - 4 * 86_400,
                    updated: NOW - 7_200,
                    ..Task::default()
                },
            ],
        }
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn render_text(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| super::render(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..height {
            let mut line = String::new();
            for x in 0..width {
                line.push_str(buffer[(x, y)].symbol());
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out
    }

    fn golden_path(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("golden")
            .join(name)
    }

    fn assert_golden(name: &str, app: &mut App, width: u16, height: u16) {
        let actual = render_text(app, width, height);
        let path = golden_path(name);
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &actual).unwrap();
            return;
        }
        let expected = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("missing golden {}: {err}", path.display()));
        assert_eq!(
            actual, expected,
            "\n--- actual ---\n{actual}\n--- expected ---\n{expected}"
        );
    }

    #[test]
    fn golden_100x30() {
        let mut app = App::new_at(golden_board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char(' '));
        assert_golden("board-100x30.txt", &mut app, 100, 30);
    }

    #[test]
    fn golden_120x40() {
        let mut app = App::new_at(golden_board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char(' '));
        assert_golden("board-120x40.txt", &mut app, 120, 40);
    }

    #[test]
    fn golden_160x50() {
        let mut app = App::new_at(golden_board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char(' '));
        assert_golden("board-160x50.txt", &mut app, 160, 50);
    }

    #[test]
    fn golden_160x50_tree() {
        let mut app = App::new_at(tree_board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char(' '));
        assert_golden("board-160x50-tree.txt", &mut app, 160, 50);
    }

    #[test]
    fn golden_120x40_tree() {
        let mut app = App::new_at(tree_board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char(' '));
        assert_golden("board-120x40-tree.txt", &mut app, 120, 40);
    }

    #[test]
    fn a_root_keeps_its_children_and_selection_across_refreshes() {
        let mut app = App::new_at(tree_board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char(' '));
        assert!(app.expanded.contains(&CardKey::Task("lead".into())));

        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.cursor.child, Some(0));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.cursor.node, 1);
        assert_eq!(app.selected_task().unwrap().id.as_deref(), Some("worker-a"));

        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.cursor.child, Some(0));
        assert_eq!(app.selected_child().unwrap().workspace, "ws-worker-a");

        let board = tree_board();
        app.set_board_at(board, NOW);
        assert_eq!(app.cursor.node, 1);
        assert_eq!(app.cursor.child, Some(0));
        assert_eq!(app.selected_task().unwrap().id.as_deref(), Some("worker-a"));
    }

    #[test]
    fn collapse_returns_the_selection_to_the_root() {
        let mut app = App::new_at(tree_board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.cursor.node, 1);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.cursor.node, 0);
        press(&mut app, KeyCode::Char(' '));
        assert!(app.expanded.is_empty());
        assert_eq!(app.cursor.node, 0);
    }

    #[test]
    fn detail_names_the_parent_and_the_children() {
        let mut app = App::new_at(tree_board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('i'));
        let root = render_text(&mut app, 160, 50);
        assert!(root.contains("children"));
        assert!(root.contains("Draft the notes"));

        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        let child = render_text(&mut app, 160, 50);
        assert!(child.contains("child of lead"));
    }

    #[test]
    fn filter_keeps_a_matching_child_under_its_root() {
        let mut app = App::new_at(tree_board(), NOW);
        press(&mut app, KeyCode::Char('/'));
        for ch in "draft".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        press(&mut app, KeyCode::Enter);
        let text = render_text(&mut app, 160, 50);
        assert!(text.contains("Lead the release"));
        assert!(text.contains("Draft the notes"));
    }

    #[test]
    fn golden_200x60() {
        let mut app = App::new_at(golden_board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char(' '));
        assert_golden("board-200x60.txt", &mut app, 200, 60);
    }

    #[test]
    fn golden_80x24() {
        let mut app = App::new_at(golden_board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char(' '));
        assert_golden("board-80x24.txt", &mut app, 80, 24);
    }

    #[test]
    fn golden_60x16() {
        let mut app = App::new_at(golden_board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char(' '));
        assert_golden("board-60x16.txt", &mut app, 60, 16);
    }

    #[test]
    fn golden_120x40_detail() {
        let mut app = App::new_at(golden_board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('i'));
        assert_golden("board-120x40-detail.txt", &mut app, 120, 40);
    }

    #[test]
    fn golden_143x40_detail() {
        let mut app = App::new_at(golden_board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('i'));
        assert_golden("board-143x40-detail.txt", &mut app, 143, 40);
    }

    #[test]
    fn golden_160x50_detail() {
        let mut app = App::new_at(golden_board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('i'));
        assert_golden("board-160x50-detail.txt", &mut app, 160, 50);
    }

    #[test]
    fn golden_help_120x40() {
        let mut app = App::new_at(golden_board(), NOW);
        press(&mut app, KeyCode::Char('?'));
        assert_golden("help-120x40.txt", &mut app, 120, 40);
    }

    #[test]
    fn too_small_40x12() {
        let mut app = App::new_at(golden_board(), NOW);
        assert_golden("too-small-40x12.txt", &mut app, 40, 12);
    }

    #[test]
    fn empty_board() {
        let mut app = App::new_at(Board::default(), NOW);
        let text = render_text(&mut app, 100, 24);
        assert!(text.contains("0 cards"));
        assert!(text.contains("No tasks yet."));
    }

    #[test]
    fn load_error_keeps_the_board_and_offers_retry() {
        let mut app = App::new_at(golden_board(), NOW);
        app.set_load_error("boom");
        let text = render_text(&mut app, 120, 40);
        assert!(text.contains("! boom"));
        assert!(text.contains("r retry"));
        assert!(text.contains("Fix the board"));
    }

    #[test]
    fn detail_needs_room_at_60x16() {
        let mut app = App::new_at(golden_board(), NOW);
        press(&mut app, KeyCode::Char('i'));
        let text = render_text(&mut app, 60, 16);
        assert!(text.contains("detail needs 70x24"));
    }

    #[test]
    fn help_overlay() {
        let mut app = App::new_at(golden_board(), NOW);
        press(&mut app, KeyCode::Char('?'));
        let text = render_text(&mut app, 100, 24);
        assert!(text.contains("open the selected conversation's seat"));
        assert!(text.contains("filter cards"));
        assert!(text.contains("expand or collapse the selected card"));
        press(&mut app, KeyCode::PageDown);
        let bottom = render_text(&mut app, 100, 24);
        assert!(bottom.contains("lanes are derived, never stored"));
    }

    #[test]
    fn no_color_keeps_the_layout_without_foreground_colors() {
        let mut app = App::new_at(golden_board(), NOW);
        app.palette = Palette::PLAIN;
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| super::render(frame, &mut app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert!(buffer.content().iter().all(|cell| cell.fg == Color::Reset));
    }

    #[test]
    fn no_match_names_the_filter() {
        let mut app = App::new_at(golden_board(), NOW);
        press(&mut app, KeyCode::Char('/'));
        for ch in "zzz".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        press(&mut app, KeyCode::Enter);
        let text = render_text(&mut app, 120, 40);
        assert!(text.contains("No tasks match \"zzz\""));
        assert!(text.contains("esc clears the filter"));
    }

    #[test]
    fn golden_120x40_filter() {
        let mut app = App::new_at(golden_board(), NOW);
        press(&mut app, KeyCode::Char('/'));
        for ch in "cache".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        press(&mut app, KeyCode::Enter);
        assert_golden("board-120x40-filter.txt", &mut app, 120, 40);
    }

    #[test]
    fn golden_120x40_no_match() {
        let mut app = App::new_at(golden_board(), NOW);
        press(&mut app, KeyCode::Char('/'));
        for ch in "zzz".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        press(&mut app, KeyCode::Enter);
        assert_golden("board-120x40-no-match.txt", &mut app, 120, 40);
    }

    #[test]
    fn help_scrolls_on_a_short_terminal() {
        let mut app = App::new_at(golden_board(), NOW);
        press(&mut app, KeyCode::Char('?'));
        let top = render_text(&mut app, 60, 16);
        assert!(top.contains("enter"));
        press(&mut app, KeyCode::PageDown);
        press(&mut app, KeyCode::PageDown);
        let bottom = render_text(&mut app, 60, 16);
        assert!(bottom.contains("quit"));
    }

    #[test]
    fn scroll_reveals_the_selected_card() {
        let mut app = App::new_at(many_backlog_board(30), NOW);
        let first = render_text(&mut app, 60, 16);
        assert!(first.contains("Task 00"));

        press(&mut app, KeyCode::Char('G'));
        let last = render_text(&mut app, 60, 16);
        assert!(last.contains("Task 29"));
        assert!(!last.contains("Task 00"));

        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('g'));
        let first_again = render_text(&mut app, 60, 16);
        assert!(first_again.contains("Task 00"));
    }

    #[test]
    fn manual_scroll_keeps_the_cursor_and_j_returns() {
        let mut app = App::new_at(many_backlog_board(30), NOW);
        render_text(&mut app, 60, 16);
        press(&mut app, KeyCode::PageDown);
        let scrolled = render_text(&mut app, 60, 16);
        assert_ne!(app.scroll[0], 0);
        assert!(!scrolled.contains("Task 00"));
        assert_eq!(app.cursor.card, 0);

        press(&mut app, KeyCode::Char('j'));
        let after = render_text(&mut app, 60, 16);
        assert!(after.contains("Task 01"));
    }
}
