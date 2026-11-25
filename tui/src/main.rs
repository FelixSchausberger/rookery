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
    let (result_tx, result_rx) = mpsc::channel();
    let (request_tx, request_rx) = mpsc::channel();
    let loader = Arc::clone(&backend);
    thread::spawn(move || load_loop(loader, request_rx, result_tx, REFRESH));

    loop {
        app.tick(now_secs());
        terminal.draw(|frame| ui::render(frame, app))?;

        while let Ok(result) = result_rx.try_recv() {
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
            Action::Refresh => request_refresh(&request_tx),
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
                        request_refresh(&request_tx);
                    }
                    Err(err) => app.set_error(err),
                }
            }
        }
    }
}

/// One loader thread serves the timer and explicit refreshes. A request wakes
/// the two-second wait; requests that pile up while a load is running coalesce
/// into the next pass. The input loop never blocks on jj.
fn load_loop(
    backend: Arc<dyn Backend>,
    requests: mpsc::Receiver<()>,
    results: mpsc::Sender<Result<Board, String>>,
    refresh: Duration,
) {
    loop {
        let requested = requests.recv_timeout(refresh).is_ok();
        if requested {
            while requests.try_recv().is_ok() {}
        }
        if results.send(backend.load()).is_err() {
            break;
        }
    }
}

fn request_refresh(requests: &mpsc::Sender<()>) {
    let _ = requests.send(());
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fake {
        loads: Arc<AtomicUsize>,
    }

    impl Backend for Fake {
        fn load(&self) -> Result<Board, String> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            Ok(Board::default())
        }

        fn open(&self, _target: &str) -> Result<String, String> {
            unreachable!()
        }

        fn mark_task(&self, _task: &str) -> Result<String, String> {
            unreachable!()
        }

        fn mark_workspace(&self, _workspace: &str) -> Result<String, String> {
            unreachable!()
        }

        fn start_seat(&self, _task: &str) -> Result<(), String> {
            unreachable!()
        }

        fn add_task(&self, _title: &str) -> Result<String, String> {
            unreachable!()
        }

        fn assign(&self, _task: &str, _workspace: &str) -> Result<String, String> {
            unreachable!()
        }

        fn set_parent(&self, _child: &str, _parent: Option<&str>) -> Result<String, String> {
            unreachable!()
        }
    }

    #[test]
    fn a_request_wakes_the_loader_and_dropping_the_receiver_stops_it() {
        let loads = Arc::new(AtomicUsize::new(0));
        let backend: Arc<dyn Backend> = Arc::new(Fake {
            loads: Arc::clone(&loads),
        });
        let (request_tx, request_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            load_loop(backend, request_rx, result_tx, Duration::from_millis(20));
        });

        request_tx.send(()).unwrap();
        let result = result_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(result.is_ok());
        assert_eq!(loads.load(Ordering::SeqCst), 1);

        drop(request_tx);
        drop(result_rx);
        handle.join().unwrap();
    }
}
