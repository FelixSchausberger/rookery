use clap::{Parser, Subcommand};
use rookery_core::board::Board;
use rookery_core::config::Config;
use rookery_core::error::{Error, Result};
use rookery_core::lifecycle::{self, Launch, Line};
use rookery_core::seat::{self, HerdrOptions};
use rookery_core::task::Store;
use rookery_core::util::{now_secs, xdg_cache};
use serde::Serialize;
use std::io::{IsTerminal, Write};
use std::os::unix::process::CommandExt;
use std::process::Command;

const USAGE: &str = "\
Usage:
  rookery <task> [-- <opencode args>...]   create a workspace and run opencode there
  rookery tab <task> [-- <opencode args>]  create a workspace in a new zellij tab
  rookery herdr [--json] [--no-focus] [--workspace ID] [--wait[=SECONDS]] [--task ID] [<task>] [-- <opencode args>]
  rookery seat [--json]
  rookery title <text...>
  rookery ls [--counts|--board|--json] [--refresh]
  rookery task add [--id ID] [--notes TEXT] <title...>
  rookery task assign [--move] <task> <workspace>
  rookery task list [--json]
  rookery task done <task>
  rookery task note <task> <text...>
  rookery task archive <task>
  rookery clean [--current|<workspace>] [--unmark]
  rookery clean --apply
  rookery open <workspace|task>
  rookery wait <task> [reason...]
  rookery go <task>
  rookery done [--force] <task>
  rookery rm [--force] <task>
  rookery gc
  rookery reap [--dry-run]

Environment: ROOKERY_BASE, ROOKERY_WS_PREFIX, ROOKERY_TRUNK, ROOKERY_V2,
ROOKERY_GC_AGE, ROOKERY_GC_EMPTY_AGE, ROOKERY_GC_SESSION_AGE,
ROOKERY_WAIT_STALE_DAYS, ROOKERY_TASK_STALE_DAYS, ROOKERY_LIVE_EXCLUDE,
ROOKERY_BOARD_JOBS, ROOKERY_NO_MAINT, ROOKERY_TASK_ASSIGN,
ROOKERY_SESSION_ARCHIVE, ROOKERY_AGENT_BIN,
ROOKERY_HOOK_{PRE,POST}_{CREATE,DELETE}.
The OCWS_* names are read when the ROOKERY_* name is unset.
";

#[derive(Parser)]
#[command(
    name = "rookery",
    version,
    about = "A rookery of jj workspaces for coding agents",
    disable_help_subcommand = true,
    allow_external_subcommands = true
)]
struct Cli {
    /// Run the opencode2 entry point instead of opencode.
    #[arg(long = "v2", global = true)]
    v2: bool,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a workspace in a new zellij tab.
    Tab {
        #[arg(required = true)]
        task: String,
        #[arg(last = true)]
        extra: Vec<String>,
    },
    /// Start an agent seat in a new herdr tab.
    Herdr {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        no_focus: bool,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(
            long,
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "180"
        )]
        wait: Option<u64>,
        #[arg(long)]
        task: Option<String>,
        task_name: Option<String>,
        #[arg(last = true)]
        extra: Vec<String>,
    },
    /// Print the calling agent's herdr workspace, tab and pane.
    Seat {
        #[arg(long)]
        json: bool,
    },
    /// Claim the current seat from the session title.
    Title {
        #[arg(required = true, trailing_var_arg = true)]
        text: Vec<String>,
    },
    /// Classify every workspace.
    #[command(alias = "list")]
    Ls {
        #[arg(long)]
        counts: bool,
        #[arg(long)]
        board: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        refresh: bool,
    },
    /// Group workspaces under a stable task id.
    Task {
        #[command(subcommand)]
        command: TaskCmd,
    },
    /// Mark or sweep a workspace for cleanup.
    Clean {
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        unmark: bool,
        #[arg(long)]
        current: bool,
        target: Option<String>,
    },
    /// Focus a workspace's seat.
    Open { target: String },
    /// Park a workspace so gc keeps it.
    Wait {
        task: String,
        #[arg(trailing_var_arg = true)]
        reason: Vec<String>,
    },
    /// Release a parked workspace.
    Go { task: String },
    /// Remove a finished workspace.
    Done {
        #[arg(long)]
        force: bool,
        task: String,
    },
    /// Remove a workspace.
    #[command(alias = "forget")]
    Rm {
        #[arg(long)]
        force: bool,
        task: String,
    },
    /// Reclaim empty and merged workspaces past the age gate.
    Gc,
    /// Export and delete conversations whose workspace is gone.
    Reap {
        #[arg(long)]
        dry_run: bool,
    },
    #[command(external_subcommand)]
    External(Vec<String>),
}

#[derive(Subcommand)]
enum TaskCmd {
    Add {
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        notes: Option<String>,
        #[arg(required = true, trailing_var_arg = true)]
        title: Vec<String>,
    },
    Assign {
        #[arg(long = "move")]
        move_workspace: bool,
        task: String,
        workspace: String,
    },
    List {
        #[arg(long)]
        json: bool,
    },
    Done {
        task: String,
    },
    Note {
        task: String,
        #[arg(required = true, trailing_var_arg = true)]
        text: Vec<String>,
    },
    Archive {
        task: String,
    },
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            std::process::ExitCode::from(err.exit_code() as u8)
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    // `allow_external_subcommands` turns any unknown word into a task name, so
    // `rookery help` used to create a workspace named help before it could say
    // anything. Handle it before Config::load, which needs a jj repository.
    if let Some(Cmd::External(args)) = &cli.command {
        if args.first().map(String::as_str) == Some("help") {
            print!("{USAGE}");
            return Ok(());
        }
    }
    if cli.command.is_none() {
        eprint!("{USAGE}");
        std::process::exit(2);
    }
    let config = Config::load(cli.v2)?;
    let mut warnings: Vec<String> = Vec::new();
    match cli.command.expect("checked above") {
        Cmd::Tab { task, extra } => {
            let launch = lifecycle::create(&config, &task, &extra, true, &mut warnings)?;
            print_warnings(&warnings);
            launch_process(launch)
        }
        Cmd::External(args) => {
            let (task, extra) = external_args(&args)?;
            let launch = lifecycle::create(&config, &task, &extra, false, &mut warnings)?;
            print_warnings(&warnings);
            launch_process(launch)
        }
        Cmd::Herdr {
            json,
            no_focus,
            workspace,
            wait,
            task,
            task_name,
            extra,
        } => {
            let options = HerdrOptions {
                focus: !no_focus,
                workspace,
                wait_secs: wait.unwrap_or(0),
                task_id: task,
                task: task_name,
                extra,
            };
            match seat::herdr(&config, &options) {
                Ok(seat) => {
                    if json {
                        print_json(&seat)?;
                    }
                    Ok(())
                }
                Err(err) => herdr_fail(err),
            }
        }
        Cmd::Seat { json } => {
            let info = seat::seat(&config)?;
            if json {
                print_json(&info)
            } else {
                println!(
                    "{}\t{}\t{}\t{}",
                    info.task, info.herdr_tab, info.herdr_pane, info.agent_status
                );
                Ok(())
            }
        }
        Cmd::Title { text } => {
            let text = text.join(" ");
            if let Some(message) = seat::title(&config, &text, &mut warnings)? {
                println!("{message}");
            }
            print_warnings(&warnings);
            Ok(())
        }
        Cmd::Ls {
            counts,
            board,
            json,
            refresh,
        } => {
            let mode = if counts {
                0
            } else if board {
                1
            } else if json {
                2
            } else {
                3
            };
            if mode == 3 {
                let output = rookery_core::jj::run_in(None, &["workspace", "list"], true)?;
                if !output.status.success() {
                    let detail = String::from_utf8_lossy(&output.stderr);
                    return Err(Error::message(format!(
                        "rookery: cannot list jj workspaces: {}",
                        detail.trim()
                    )));
                }
                std::io::stdout()
                    .write_all(&output.stdout)
                    .map_err(|err| Error::message(format!("rookery: {err}")))?;
                println!();
            }
            let want_meta = mode == 2;
            let board = Board::load(&config, refresh, want_meta)?;
            match mode {
                0 => print!("{}", board.render_counts(config.wait_stale_days)),
                1 => print!("{}", board.render_board()),
                2 => print!("{}", board.render_json()),
                _ => print!(
                    "{}",
                    board.render_table(&config.trunk, config.wait_stale_days)
                ),
            }
            Ok(())
        }
        Cmd::Task { command } => run_task(&config, command),
        Cmd::Clean {
            apply,
            unmark,
            current,
            target,
        } => {
            if apply {
                if unmark || current || target.is_some() {
                    return Err(Error::usage(
                        "rookery clean: --apply takes no workspace or flag",
                    ));
                }
                let lines = lifecycle::clean_apply(&config, &mut warnings)?;
                print_lines(&lines);
                print_warnings(&warnings);
                return Ok(());
            }
            if current && target.is_some() {
                return Err(Error::usage("rookery clean: --current takes no workspace"));
            }
            let target = if current {
                None
            } else {
                Some(target.ok_or_else(|| Error::usage("rookery clean: a workspace is required"))?)
            };
            let message = lifecycle::clean_mark(&config, target.as_deref(), unmark, current)?;
            println!("{message}");
            print_warnings(&warnings);
            Ok(())
        }
        Cmd::Open { target } => {
            let message = seat::open(&config, &target)?;
            println!("{message}");
            Ok(())
        }
        Cmd::Wait { task, reason } => {
            let message = lifecycle::wait(&config, &task, &reason.join(" "))?;
            println!("{message}");
            Ok(())
        }
        Cmd::Go { task } => {
            let message = lifecycle::go(&config, &task)?;
            println!("{message}");
            Ok(())
        }
        Cmd::Done { force, task } => {
            let message = lifecycle::done(&config, &task, force, &mut warnings)?;
            println!("{message}");
            print_warnings(&warnings);
            Ok(())
        }
        Cmd::Rm { force, task } => {
            let message = lifecycle::rm(&config, &task, force, &mut warnings)?;
            println!("{message}");
            print_warnings(&warnings);
            Ok(())
        }
        Cmd::Gc => {
            let lines = lifecycle::gc(&config, true, &mut warnings)?;
            print_lines(&lines);
            print_warnings(&warnings);
            Ok(())
        }
        Cmd::Reap { dry_run } => {
            let lines = lifecycle::reap(&config, dry_run)?;
            print_lines(&lines);
            print_warnings(&warnings);
            Ok(())
        }
    }
}

fn run_task(config: &Config, command: TaskCmd) -> Result<()> {
    let store = Store::new(&config.base);
    match command {
        TaskCmd::Add { id, notes, title } => {
            let title = title.join(" ");
            let id = rookery_core::ops::task_add(
                config,
                &title,
                id.as_deref(),
                notes.as_deref().unwrap_or(""),
            )?;
            println!("rookery: task '{id}' added ({title})");
            Ok(())
        }
        TaskCmd::Assign {
            move_workspace,
            task,
            workspace,
        } => {
            let message =
                rookery_core::ops::task_assign(config, &task, &workspace, move_workspace)?;
            println!("{message}");
            Ok(())
        }
        TaskCmd::List { json } => {
            let store = store.read()?;
            if json {
                for task in &store.tasks {
                    println!("{}", serde_json::to_string(task).unwrap_or_default());
                }
            } else {
                for task in &store.tasks {
                    println!(
                        "  {:<24} {:<8} {} {}",
                        task.id,
                        task.status,
                        task.workspaces.len(),
                        task.title
                    );
                }
            }
            Ok(())
        }
        TaskCmd::Done { task } => {
            let lines = rookery_core::ops::task_done(config, &task)?;
            print_lines(&lines);
            Ok(())
        }
        TaskCmd::Note { task, text } => {
            let message = rookery_core::ops::task_note(config, &task, &text.join(" "))?;
            println!("{message}");
            Ok(())
        }
        TaskCmd::Archive { task } => {
            store.archive(&task)?;
            println!("rookery: task '{task}' archived");
            Ok(())
        }
    }
}

fn external_args(args: &[String]) -> Result<(String, Vec<String>)> {
    let Some((task, rest)) = args.split_first() else {
        return Err(Error::usage("rookery: a task is required"));
    };
    if rest.is_empty() {
        return Ok((task.clone(), Vec::new()));
    }
    if rest[0] == "--" {
        return Ok((task.clone(), rest[1..].to_vec()));
    }
    Err(Error::usage(format!(
        "rookery: unexpected argument '{}'",
        rest[0]
    )))
}

fn launch_process(launch: Launch) -> Result<()> {
    if launch.zellij {
        if std::env::var_os("ZELLIJ_SESSION_NAME").is_none() {
            return Err(Error::message(
                "rookery: 'tab' must run inside a zellij session",
            ));
        }
        let mut command = Command::new("zellij");
        command
            .args([
                "action",
                "new-tab",
                "--name",
                &launch.name,
                "--cwd",
                &launch.dir.to_string_lossy(),
                "--",
                &launch.program,
            ])
            .args(&launch.args);
        let status = command
            .status()
            .map_err(|err| Error::message(format!("rookery: cannot run zellij: {err}")))?;
        if !status.success() {
            return Err(Error::message("rookery: zellij could not create the tab"));
        }
        return Ok(());
    }
    std::env::set_current_dir(&launch.dir).map_err(|err| {
        Error::message(format!(
            "rookery: cannot enter '{}': {err}",
            launch.dir.display()
        ))
    })?;
    let err = Command::new(&launch.program).args(&launch.args).exec();
    Err(Error::message(format!(
        "rookery: cannot run {}: {err}",
        launch.program
    )))
}

fn print_lines(lines: &[Line]) {
    for line in lines {
        if line.stderr {
            eprintln!("{}", line.text);
        } else {
            println!("{}", line.text);
        }
    }
}

fn print_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("{warning}");
    }
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    match serde_json::to_string(value) {
        Ok(body) => {
            println!("{body}");
            Ok(())
        }
        Err(err) => Err(Error::message(format!(
            "rookery: cannot render JSON: {err}"
        ))),
    }
}

fn herdr_fail(err: Error) -> ! {
    if matches!(&err, Error::Invalid(_) | Error::Usage(_)) {
        eprintln!("{err}");
        std::process::exit(err.exit_code());
    }
    let text = err.to_string();
    let text = text.strip_prefix("rookery herdr: ").unwrap_or(&text);
    let message = format!("rookery herdr: {text}");
    if std::io::stdin().is_terminal() {
        eprintln!("{message}");
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    } else {
        let dir = xdg_cache("rookery");
        let _ = std::fs::create_dir_all(&dir);
        let line = format!("{} {message}\n", now_secs());
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("seat.log"))
            .and_then(|mut file| file.write_all(line.as_bytes()));
    }
    std::process::exit(1);
}
