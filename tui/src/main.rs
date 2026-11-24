// The Rookery board. Reads the CLI's JSON streams on a short timer, writes
// only through the CLI, and never touches jj, the task store, or the base.
mod app;
mod cli;
mod model;
mod ui;

use app::{Action, App};
use cli::Cli;
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use model::Board;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const REFRESH: Duration = Duration::from_secs(2);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::system();
    let mut app = App::new(Board::default());
    match cli.load() {
        Ok(board) => app.set_board(board),
        Err(err) => app.set_load_error(err),
    }

    install_panic_hook();
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal, &mut app, &cli);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result?;
    Ok(())
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    cli: &Cli,
) -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    let loader = cli.clone();
    thread::spawn(move || {
        loop {
            thread::sleep(REFRESH);
            if tx.send(loader.load()).is_err() {
                break;
            }
        }
    });

    loop {
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
            Action::Refresh => reload(app, cli),
            action => {
                let Some(args) = action.args() else {
                    continue;
                };
                match cli.run(&args) {
                    Ok(out) => {
                        let label = action.label().unwrap_or_default();
                        let detail = out.lines().next().unwrap_or_default().trim();
                        if detail.is_empty() {
                            app.set_status(label);
                        } else {
                            app.set_status(format!("{label} {detail}"));
                        }
                        reload(app, cli);
                    }
                    Err(err) => app.set_error(err),
                }
            }
        }
    }
}

fn reload(app: &mut App, cli: &Cli) {
    match cli.load() {
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
