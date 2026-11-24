// Rendering only: the five derived lanes as cards, the lane headers, the
// status line, and the modal prompts.
use crate::app::{App, HINTS, Mode};
use crate::view::{LaneModel, truncate};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width < 50 || area.height < 12 {
        render_too_small(frame, area);
        return;
    }
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
    let cards: usize = app.lanes.iter().map(|lane| lane.cards.len()).sum();
    let workspaces: usize = app
        .lanes
        .iter()
        .flat_map(|lane| &lane.cards)
        .map(|card| card.workspaces().len())
        .sum();
    let line = Line::from(vec![
        Span::styled(
            "rookery board",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("   {cards} cards   {workspaces} workspaces")),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_lanes(frame: &mut Frame, area: Rect, app: &App) {
    let wide = area.width >= 110;
    let columns = Layout::horizontal([Constraint::Ratio(1, 5); 5])
        .spacing(1)
        .split(area);
    for (index, lane) in app.lanes.iter().enumerate() {
        render_lane(frame, columns[index], app, lane, index, wide);
    }
}

fn render_lane(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    lane: &LaneModel,
    index: usize,
    wide: bool,
) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
    let selected = app.cursor.lane == index;
    let workspaces: usize = lane.cards.iter().map(|card| card.workspaces().len()).sum();
    let label = truncate(
        &format!(
            "{} {} · {}ws",
            lane.lane.title(),
            lane.cards.len(),
            workspaces
        ),
        area.width as usize,
    );
    let mut header_style = Style::default().add_modifier(Modifier::BOLD);
    if selected {
        header_style = header_style.add_modifier(Modifier::REVERSED);
    }
    frame.render_widget(Paragraph::new(Line::styled(label, header_style)), rows[0]);

    let (lines, cursor) = lane_lines(app, lane, index, rows[1].width, wide);
    let height = rows[1].height as usize;
    let scroll = match cursor {
        Some((start, end)) if height > 0 && end - start >= height => start,
        Some((_, end)) if height > 0 && end > height => end - height,
        _ => 0,
    };
    frame.render_widget(
        Paragraph::new(lines).scroll((scroll.min(u16::MAX as usize) as u16, 0)),
        rows[1],
    );
}

/// Build the visible rows of one lane. The selected card is expanded, so the
/// cursor range covers its full card and the lane scrolls by lines, not cards.
fn lane_lines(
    app: &App,
    lane: &LaneModel,
    index: usize,
    width: u16,
    wide: bool,
) -> (Vec<Line<'static>>, Option<(usize, usize)>) {
    if lane.cards.is_empty() {
        return (
            vec![Line::styled(
                "(no tasks)",
                Style::default().add_modifier(Modifier::DIM),
            )],
            None,
        );
    }
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cursor = None;
    for (card_index, card) in lane.cards.iter().enumerate() {
        let selected = app.cursor.lane == index && app.cursor.card == card_index;
        let start = lines.len();
        let child = if selected { app.cursor.child } else { None };
        let card_lines = if wide {
            card.lines(width as usize, selected, child, app.now)
        } else {
            card.compact_lines(width as usize, selected, child, app.now)
        };
        lines.extend(card_lines);
        if selected {
            cursor = Some((start, lines.len()));
        }
    }
    (lines, cursor)
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
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

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

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn render_text(app: &App, width: u16, height: u16) -> String {
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

    fn assert_golden(name: &str, app: &App, width: u16, height: u16) {
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
    fn golden_120x40() {
        let mut app = App::new_at(golden_board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        assert_golden("board-120x40.txt", &app, 120, 40);
    }

    #[test]
    fn golden_160x50() {
        let mut app = App::new_at(golden_board(), NOW);
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('l'));
        assert_golden("board-160x50.txt", &app, 160, 50);
    }

    #[test]
    fn too_small_40x12() {
        let app = App::new_at(golden_board(), NOW);
        assert_golden("too-small-40x12.txt", &app, 40, 12);
    }

    #[test]
    fn empty_board() {
        let app = App::new_at(Board::default(), NOW);
        let text = render_text(&app, 100, 24);
        assert!(text.contains("0 cards"));
        assert!(text.contains("(no tasks)"));
    }

    #[test]
    fn help_overlay() {
        let mut app = App::new_at(golden_board(), NOW);
        press(&mut app, KeyCode::Char('?'));
        let text = render_text(&app, 100, 24);
        assert!(text.contains("open the selected conversation's seat"));
        assert!(text.contains("lanes are derived, never stored"));
    }
}
