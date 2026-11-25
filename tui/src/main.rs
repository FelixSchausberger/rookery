// The Rookery board. Reads the core's board on a short timer, writes only
// through the core, and never touches jj, herdr or the task store itself.
mod app;
mod backend;
mod model;
mod ui;
mod view;

use app::{Action, App};
use backend::{Backend, CoreBackend};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use model::Board;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use rookery_core::util::now_secs;
use std::io;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const REFRESH: Duration = Duration::from_secs(2);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let backend: Arc<dyn Backend> = Arc::new(CoreBackend::default());
    let mut app = App::new(Board::default());
    match backend.load() {
        Ok(board) => app.set_board(board),
        Err(err) => app.set_load_error(err),
    }

    install_panic_hook();
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend_terminal = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend_terminal)?;

    let result = run(&mut terminal, &mut app, backend);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result?;
    Ok(())
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    backend: Arc<dyn Backend>,
) -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    let loader = Arc::clone(&backend);
    thread::spawn(move || loop {
        thread::sleep(REFRESH);
        if tx.send(loader.load()).is_err() {
            break;
        }
    });

    loop {
        app.tick(now_secs());
        terminal.draw(|frame| ui::render(frame, app))?;

        while let Ok(result) = rx.try_recv() {
            match result {
                Ok(board) => app.set_board(board),
                Err(err) => app.set_load_error(err),
            }
        }

        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let Some(action) = app.handle_key(key) else {
            continue;
        };
        match action {
            Action::Quit => return Ok(()),
            Action::Refresh => reload(app, &backend),
            action => {
                let result = match &action {
                    Action::Open(target) => backend.open(target),
                    Action::MarkTask(task) => backend.mark_task(task),
                    Action::MarkWorkspace(workspace) => backend.mark_workspace(workspace),
                    Action::StartSeat(task) => backend.start_seat(task).map(|()| String::new()),
                    Action::AddTask(title) => backend.add_task(title),
                    Action::AssignTask { task, workspace } => backend.assign(task, workspace),
                    Action::SetParent { child, parent } => {
                        backend.set_parent(child, parent.as_deref())
                    }
                    Action::Quit | Action::Refresh => unreachable!(),
                };
                match result {
                    Ok(out) => {
                        let label = action.label().unwrap_or_default();
                        let detail = out.lines().next().unwrap_or_default().trim();
                        if detail.is_empty() {
                            app.set_status(label);
                        } else {
                            app.set_status(format!("{label} {detail}"));
                        }
                        reload(app, &backend);
                    }
                    Err(err) => app.set_error(err),
                }
            }
        }
    }
}

fn reload(app: &mut App, backend: &Arc<dyn Backend>) {
    match backend.load() {
        Ok(board) => app.set_board(board),
        Err(err) => app.set_load_error(err),
    }
}

/// A panic while the terminal is in raw mode would leave the pane unusable;
/// restore it before the original hook prints the panic.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original(info);
    }));
}
