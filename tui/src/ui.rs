// Rendering only: the five derived lanes, the task expansion for the selected
// row, the status line, and the two modal prompts.
use crate::app::{App, HINTS, Mode};
use crate::model::LaneView;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(area);

    render_title(frame, chunks[0], app);
    render_lanes(frame, chunks[1], app);
    render_footer(frame, chunks[2], app);

    match &app.mode {
        Mode::Help => render_help(frame, area),
        Mode::TaskInput { text } => render_input(frame, area, text),
        Mode::Assign {
            workspace,
            selected,
        } => render_assign(frame, area, app, workspace, *selected),
        Mode::Normal => {}
    }
}

fn render_title(frame: &mut Frame, area: Rect, app: &App) {
    let tasks: usize = app.lanes.iter().map(|lane| lane.tasks.len()).sum();
    let workspaces: usize = app
        .lanes
        .iter()
        .flat_map(|lane| &lane.tasks)
        .map(|task| task.workspaces.len())
        .sum();
    let line = Line::from(vec![
        Span::styled(
            "rookery board",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("   {tasks} tasks   {workspaces} workspaces")),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_lanes(frame: &mut Frame, area: Rect, app: &App) {
    let columns = Layout::horizontal([Constraint::Ratio(1, 5); 5]).split(area);
    for (index, lane) in app.lanes.iter().enumerate() {
        render_lane(frame, columns[index], app, lane, index);
    }
}

fn render_lane(frame: &mut Frame, area: Rect, app: &App, lane: &LaneView, index: usize) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
    let selected = app.cursor.lane == index;
    let mut header_style = Style::default().add_modifier(Modifier::BOLD);
    if selected {
        header_style = header_style.add_modifier(Modifier::REVERSED);
    }
    frame.render_widget(
        Paragraph::new(Line::styled(
            format!("{} ({})", lane.lane.title(), lane.tasks.len()),
            header_style,
        )),
        rows[0],
    );

    let (lines, cursor_line) = lane_lines(app, lane, index);
    let height = rows[1].height as usize;
    let scroll = match cursor_line {
        Some(line) if height > 0 && line + 1 > height => line + 1 - height,
        _ => 0,
    };
    frame.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), rows[1]);
}

/// Build the visible rows of one lane. Only the selected task expands, so the
/// board stays dense with many tasks while a task's conversations are one
/// keystroke away. Returns the line index of the cursor for scrolling.
fn lane_lines(app: &App, lane: &LaneView, index: usize) -> (Vec<Line<'static>>, Option<usize>) {
    if lane.tasks.is_empty() {
        return (
            vec![Line::styled(
                "(empty)",
                Style::default().add_modifier(Modifier::DIM),
            )],
            None,
        );
    }
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cursor_line = None;
    for (task_index, task) in lane.tasks.iter().enumerate() {
        let is_selected = app.cursor.lane == index && app.cursor.task == task_index;
        let count = task.workspaces.len();
        let label = if count == 0 {
            task.title.clone()
        } else {
            format!("{} ({count})", task.title)
        };
        let mut style = Style::default();
        if is_selected {
            style = style.add_modifier(Modifier::BOLD);
            if app.cursor.child.is_none() {
                style = style.add_modifier(Modifier::REVERSED);
                cursor_line = Some(lines.len());
            }
        }
        lines.push(Line::styled(label, style));

        if !is_selected {
            continue;
        }
        for (child_index, workspace) in task.workspaces.iter().enumerate() {
            let last = child_index + 1 == count;
            let glyph = if last { "└" } else { "├" };
            let stem = if last { " " } else { "│" };
            let state = if workspace.wait.is_some() {
                "wait".to_string()
            } else {
                workspace.state.as_str().to_string()
            };
            let hold = workspace.hold.as_str().to_string();
            let selected_child = app.cursor.child == Some(child_index);
            let name_style = if selected_child {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default().add_modifier(Modifier::DIM)
            };
            lines.push(Line::styled(
                format!("{glyph} {}", workspace.workspace),
                name_style,
            ));
            if selected_child {
                cursor_line = Some(lines.len() - 1);
            }
            lines.push(Line::styled(
                format!("{stem}   {state}  {hold}"),
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
    }
    (lines, cursor_line)
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let hints = match app.mode {
        Mode::Normal => HINTS,
        Mode::Help => "any key closes",
        Mode::TaskInput { .. } | Mode::Assign { .. } => "enter confirm   esc cancel",
    };
    let (text, style) = if app.status.is_empty() {
        (
            hints.to_string(),
            Style::default().add_modifier(Modifier::DIM),
        )
    } else if app.error {
        (format!("! {}", app.status), Style::default().fg(Color::Red))
    } else {
        (app.status.clone(), Style::default())
    };
    frame.render_widget(Paragraph::new(Line::styled(text, style)), area);
}

fn render_help(frame: &mut Frame, area: Rect) {
    let rect = centered_rect(64, 18, area);
    frame.render_widget(Clear, rect);
    let lines = vec![
        Line::raw(""),
        Line::raw("  enter   open the selected conversation's seat"),
        Line::raw("  d       mark the task (or this workspace) for cleanup"),
        Line::raw("  n       create a backlog task"),
        Line::raw("  s       start a seat for the task"),
        Line::raw("  a       assign the selected ungrouped workspace"),
        Line::raw("  j k     move within a lane"),
        Line::raw("  h l     move between lanes"),
        Line::raw("  r       refresh now"),
        Line::raw("  ?       close this help"),
        Line::raw("  q       quit"),
        Line::raw(""),
        Line::styled(
            "  tasks group workspaces; lanes are derived, never stored",
            Style::default().add_modifier(Modifier::DIM),
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("keys")),
        rect,
    );
}

fn render_input(frame: &mut Frame, area: Rect, text: &str) {
    let rect = centered_rect(60, 3, area);
    frame.render_widget(Clear, rect);
    let line = Line::from(vec![
        Span::raw(text.to_string()),
        Span::styled("▏", Style::default().add_modifier(Modifier::DIM)),
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
        vec![Line::styled(
            "(no open tasks)",
            Style::default().add_modifier(Modifier::DIM),
        )]
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
    use crate::model::{Board, Clean, Hold, State, Task, Workspace};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn workspace(name: &str, state: &str, hold: &str) -> Workspace {
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

    fn task(id: &str, title: &str, workspaces: Vec<&str>) -> Task {
        Task {
            id: id.to_string(),
            title: title.to_string(),
            status: "open".to_string(),
            workspaces: workspaces.into_iter().map(str::to_string).collect(),
            ..Task::default()
        }
    }

    fn render(app: &App, width: u16, height: u16) -> String {
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

    fn assert_board(app: &App, width: u16, height: u16, expected: &str) {
        let actual = render(app, width, height);
        assert_eq!(
            actual, expected,
            "\n--- actual ---\n{actual}\n--- expected ---\n{expected}"
        );
    }

    #[test]
    fn empty_board() {
        assert_board(
            &App::new(Board::default()),
            100,
            4,
            concat!(
                "rookery board   0 tasks   0 workspaces\n",
                "Backlog (0)         Active (0)          Review (0)          Blocked (0)         To Clean (0)\n",
                "(empty)             (empty)             (empty)             (empty)             (empty)\n",
                "enter open  d done  n new  s seat  a assign  r refresh  ? help  q quit\n",
            ),
        );
    }

    #[test]
    fn all_five_lanes() {
        assert_board(
            &App::new(five_lane_board()),
            120,
            4,
            concat!(
                "rookery board   5 tasks   4 workspaces\n",
                "Backlog (1)             Active (1)              Review (1)              Blocked (1)             To Clean (1)\n",
                "Later task              Active task (1)         Review task (1)         Blocked task (1)        Clean task (1)\n",
                "enter open  d done  n new  s seat  a assign  r refresh  ? help  q quit\n",
            ),
        );
    }

    #[test]
    fn expanded_task_with_two_conversations() {
        let mut app = App::new(two_conversation_board());
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('l'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_board(
            &app,
            120,
            8,
            concat!(
                "rookery board   1 tasks   2 workspaces\n",
                "Backlog (0)             Active (1)              Review (0)              Blocked (0)             To Clean (0)\n",
                "(empty)                 Active task (2)         (empty)                 (empty)                 (empty)\n",
                "                        ├ ws-first\n",
                "                        │   unpushed  live\n",
                "                        └ ws-second\n",
                "                            merged  -\n",
                "enter open  d done  n new  s seat  a assign  r refresh  ? help  q quit\n",
            ),
        );
    }

    #[test]
    fn help_overlay() {
        let app = App::new(Board::default());
        let mut app = app;
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('?'),
            crossterm::event::KeyModifiers::NONE,
        ));
        let text = render(&app, 100, 24);
        assert!(text.contains("open the selected conversation's seat"));
        assert!(text.contains("lanes are derived, never stored"));
    }

    fn five_lane_board() -> Board {
        let mut backlog = task("later", "Later task", vec![]);
        backlog.title = "Later task".into();
        let mut active = workspace("ws-active", "unpushed", "live");
        active.task = Some("active".into());
        let mut review = workspace("ws-review", "merged", "-");
        review.task = Some("review".into());
        let mut blocked = workspace("ws-blocked", "empty", "-");
        blocked.task = Some("blocked".into());
        blocked.wait = Some(crate::model::Wait::default());
        let mut clean = workspace("ws-clean", "merged", "-");
        clean.task = Some("clean".into());
        clean.clean = Some(Clean::default());
        Board {
            workspaces: vec![active, review, blocked, clean],
            tasks: vec![
                backlog,
                task("active", "Active task", vec!["ws-active"]),
                task("review", "Review task", vec!["ws-review"]),
                task("blocked", "Blocked task", vec!["ws-blocked"]),
                task("clean", "Clean task", vec!["ws-clean"]),
            ],
        }
    }

    fn two_conversation_board() -> Board {
        let mut first = workspace("ws-first", "unpushed", "live");
        first.task = Some("active".into());
        let mut second = workspace("ws-second", "merged", "-");
        second.task = Some("active".into());
        Board {
            workspaces: vec![first, second],
            tasks: vec![task("active", "Active task", vec!["ws-first", "ws-second"])],
        }
    }
}
