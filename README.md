# Rookery

`rookery` gives every agent its own jj workspace in one repository. It creates
the workspace, claims it with a change description, starts OpenCode rooted
there, classifies every workspace from jj state, and reclaims the finished
ones. It also drives herdr seats: a new workspace, a new herdr tab, and a
lookup from an OpenCode session back to its pane. Tasks group workspaces that
belong to one piece of work, the JSON board feeds a TUI client, and
`rookery-tui` renders that board.

A rookery is a colony of nests in one place: one base directory, one workspace
per task, one agent per workspace. A rook is also a piece on a board, which is
what the board view is.

This project is the extracted workspace manager of a personal NixOS fleet. The
fleet pins it as a flake input and configures it through the bundled Home
Manager module.

## Requirements

- Linux (the liveness probe reads `/proc`), bash for hooks and the agent
  launch, git
- jj 0.38 or newer (`rookery` uses the `root` field of `jj workspace list`)
- direnv for the `.envrc` approval on a new workspace
- herdr and OpenCode for `rookery herdr`, `rookery seat`, and `rookery title`

## Install

As a flake input:

```nix
{
  inputs.rookery.url = "github:FelixSchausberger/rookery";
  inputs.rookery.inputs.nixpkgs.follows = "nixpkgs";
}
```

and in a Home Manager module:

```nix
{
  imports = [inputs.rookery.homeManagerModules.default];

  programs.rookery = {
    enable = true;
    base = "${config.home.homeDirectory}/.local/share/ocws";
    workspacePrefix = "ws-";
    trunk = "main@origin";
    tui.enable = true;
  };
}
```

The module installs the configured binary under both names, `rookery` and the
pre-rename `ocws`, which carries the options as environment defaults. A runtime
`ROOKERY_*` variable still wins. `OCWS_*` is read as a fallback when the new
name is unset.

Without Nix, build the workspace with `cargo build --release -p rookery-cli`
and `cargo build --release -p rookery-tui`. The v0.4.0 shell remains as
`rookery-legacy` for one release; the module installs only the Rust CLI.

## Commands

| command                                                             | effect                                                         |
| ------------------------------------------------------------------- | -------------------------------------------------------------- |
| `rookery <task> [-- <opencode args>]`                               | create a workspace and start OpenCode there                    |
| `rookery herdr [--json] [--no-focus] [--wait] [--task ID] [<task>]` | create a workspace in a new herdr tab and start OpenCode       |
| `rookery tab <task>`                                                | create a workspace in a new zellij tab                         |
| `rookery seat [--json]`                                             | print the calling agent's herdr workspace, tab, and pane       |
| `rookery title <text...>`                                           | claim the current seat: describe, rename the workspace and tab |
| `rookery ls [--counts\|--board\|--json] [--refresh]`                | classify every workspace                                       |
| `rookery task add [--id ID] [--notes TEXT] <title...>`              | create a backlog task                                          |
| `rookery task assign [--move] <task> <workspace>`                   | group an existing workspace under a task                       |
| `rookery task list [--json]`                                        | list tasks and their workspaces                                |
| `rookery task done <task>`                                          | lead-only: mark the workspaces, set the task done              |
| `rookery task note <task> <text...>`                                | append a decision or follow-up to the task's notes             |
| `rookery task archive <task>`                                       | archive a task whose workspaces are gone                       |
| `rookery clean [--current\|<workspace>]`                            | mark a workspace for the cleanup sweep                         |
| `rookery clean --unmark [--current\|<workspace>]`                   | clear the cleanup mark                                         |
| `rookery clean --apply`                                             | remove marked workspaces that are safe to remove               |
| `rookery open <workspace\|task>`                                    | focus a workspace's seat                                       |
| `rookery wait <task> [reason...]`                                   | park a workspace; gc keeps it until `rookery go`               |
| `rookery go <task>`                                                 | release a parked workspace                                     |
| `rookery done [--force] <task>`                                     | remove a finished workspace                                    |
| `rookery rm [--force] <task>`                                       | remove a workspace                                             |
| `rookery gc`                                                        | reclaim empty and merged workspaces idle past the age gate     |

`rookery ls --counts` prints eight integers in this order: total, empty, merged,
unpushed, in-flight, conflict, stale, waitold. The order is a consumer contract.
`rookery ls --json` prints one JSON object per workspace (NDJSON) with `task`,
`workspace`, `path`, `state`, `hold`, `description`, `session` (title and last
update), `wait` (reason and age), `clean` (age), and `age` (the workspace
target's commit age). Task ids are null for ungrouped workspaces.

## Workspace states

A workspace is classified by the commits reachable from its `@`:

- `empty`: nothing outside the trunk carries content
- `merged`: off-trunk content is fully absorbed into the trunk
- `unpushed`: off-trunk content with no remote bookmark
- `in-flight`: off-trunk content whose bookmark is pushed (an open PR)
- `conflict`: `@` or an off-trunk commit carries a conflict

Absorption is a content test, not an ancestry test: a squash-merge lands the
same bytes under a different commit, so the draft is finished even though it is
not an ancestor of the trunk. That is what lets `rookery done` clean up after a
squash-merged PR.

The board cache is keyed on the jj operation id, the trunk commit, and the
workspace list, so a describe, push, fetch, workspace add, forget, or rename
invalidates it. Liveness, session recency, wait markers, and clean markers are
read fresh on every board; they never come from the cache.

## Tasks and cleanup

A task groups zero or more workspaces under one stable id, stored in
`ROOKERY_BASE/tasks.json`. Writes serialize on `tasks.json.lock` and replace the
document atomically, so read paths (`ls --json`, `task list --json`) take no
lock. A workspace belongs to at most one task; `task assign` refuses to move a
grouped workspace unless `--move` is given, and only an open task accepts
workspaces. A workspace with no task is still a row on the board, with
`task: null`. `rookery title` follows a rename, and every removal path drops
the workspace from its task.

The task status is the lifecycle authority. `open` (the default) holds every
grouped workspace out of `gc` and out of `clean --apply`, so a worker that
finishes early cannot lose its workspace before the lead has integrated the
result. `done` is the lead's decision: `task done` marks every grouped
workspace and releases the hold, and `clean --apply` may then remove the ones
that are empty or merged. `archived` is hidden from the board. A done task
whose last workspace is removed archives itself, and an open task stays visible
with zero workspaces as a backlog item.

`task done` refuses to run from a workspace grouped under the task: a worker
cannot finish its own task. Run it from the lead's ungrouped workspace or from
the primary checkout. An open task that has not been touched for
`ROOKERY_TASK_STALE_DAYS` (default 14) surfaces in `gc` output once, so a lead
that disappeared cannot hold workspaces silently; `task note` resets the
clock. Workspaces with unpushed, in-flight, or conflicted work
are still marked, but `clean --apply` keeps them and names the reason, so the
task cannot archive until that work is merged or deliberately discarded.

`rookery task note <id> <text...>` appends a decision or follow-up to the
task's durable notes; that is where a lead records a deferred follow-up instead
of leaving a workspace behind.

`rookery clean` writes a `<workspace>.clean` marker beside the wait marker. The
mark is a person's request, not a stored state: a task's lane is always derived
from its workspaces. `rookery clean --apply` removes a marked workspace only
when it classifies as `empty` or `merged`, no process is rooted in its
directory, and it holds no unpushed, in-flight, or conflicted work. The mark is
consent for the session-age hold that `gc` otherwise applies, but never consent
to close an open task. Every removal path clears both markers.

`rookery open` focuses an existing seat through the CLI: a workspace name
resolves directly, and a task id resolves to a live grouped workspace or the
first one jj still lists. The verb is backend-neutral; the herdr-specific
lookup stays inside the command.

## Board TUI

`rookery-tui` is the one board: tasks and standalone workspaces as cards in
five derived lanes. A card carries the title, task id, workspace count,
aggregate state, session, wait/clean markers and age; the selected card
expands in place with one row per workspace.

- Backlog: no workspaces. Active: a live or unpushed workspace. Review: every
  workspace is merged, empty, or in flight. Blocked: a conflict or a wait
  marker. To Clean: every workspace is marked. Archived tasks are hidden.
- At 110 columns and wider the five lanes share the width. Below that one lane
  is focused and a lane bar keeps all five counts visible; cards are compact
  below 70 columns. The header counts live, session, wait, conflict and stale
  workspaces and the time since the last successful refresh.
- Keys: `h`/`l` and `Tab`/`Shift-Tab` switch lanes, `1`-`5` jump to a lane,
  `j`/`k` move between cards, `Space` expands or collapses a card, `Enter`
  opens the selected task or workspace seat, `d` marks the task or workspace
  for cleanup, `s` starts a seat for the task, `n` creates a backlog task, `a`
  assigns an ungrouped workspace, `gg`/`G` jump to the first or last card,
  `Ctrl-d`/`Ctrl-u` scroll half a page, `PgDn`/`PgUp` scroll a page, `i` toggles
  the detail pane, `/` filters live, `f` cycles the filter presets, `o` cycles
  the sort, `r` refreshes, `?` opens the key help, `Esc` clears the filter or
  closes the detail pane, and `q` quits.
- The detail pane shows the selected task's notes and timestamps and each
  workspace's state, hold, session, wait, clean marker and path. It is a right
  pane at 140 columns, a bottom pane at 70x24, and a footer hint when neither
  fits.
- `/` matches titles, ids, notes, workspace names, descriptions and session
  titles. `f` cycles all, attention, live, unpushed and reclaimable; `o`
  cycles attention, title and updated. `Esc` restores the selection from before
  the filter. A no-match board says what was filtered.
- `NO_COLOR` keeps the layout and selection without foreground colors.
- It uses `rookery-core` in process: the same board read model and the same
  actions as the CLI. It never reads the task store, the jj store, or the base
  itself, and it never calls herdr or jj directly.
- The CLI's `ls --json` and `task list --json` remain the public NDJSON
  contract for other clients.
- The board refreshes every two seconds and after every action. A failed load
  keeps the last board, shows `! <error>` in the header, and `r` retries.

Install it with `programs.rookery.tui.enable = true`, or build it with
`nix build .#rookery-tui`. The client is terminal-agnostic; a herdr popup, a
zellij pane, or a plain terminal only decide where it runs.

## Liveness and GC

`rookery gc` removes empty and merged workspaces that sit idle past a gate and
have no live process. It never removes:

- a workspace with unpushed, in-flight, or conflicted work
- a workspace grouped under an open task, whatever its state
- a workspace with a process rooted in it
- a workspace with an unarchived OpenCode session updated within the session
  age window
- a parked workspace (`rookery wait`), whatever its state

An unreadable task store makes `gc` skip the pass and `clean --apply` keep every
marked workspace: unknown never means empty.

Per-project MCP and LSP daemons inherit the spawning session's cwd and outlive
it, so the process probe excludes known auxiliary binaries and accepts extra
patterns through `ROOKERY_LIVE_EXCLUDE`. A process whose command line cannot be
read counts as live; `rookery` never kills.

Deletion goes through `rip`, so a mistaken reclaim is restorable.

## Configuration

The module options cover the base directory, workspace prefix, trunk, gc gates
and schedule, and the four lifecycle hooks. Every one maps to an environment
variable:

| option            | variable                  | default                                                                    |
| ----------------- | ------------------------- | -------------------------------------------------------------------------- |
| `base`            | `ROOKERY_BASE`            | sibling of the workspace, then `$XDG_DATA_HOME/ocws`, then `/tmp/opencode` |
| `workspacePrefix` | `ROOKERY_WS_PREFIX`       | `ws-`                                                                      |
| `trunk`           | `ROOKERY_TRUNK`           | `main@origin`                                                              |
| `gc.age`          | `ROOKERY_GC_AGE`          | `86400`                                                                    |
| `gc.emptyAge`     | `ROOKERY_GC_EMPTY_AGE`    | `21600`                                                                    |
| `gc.sessionAge`   | `ROOKERY_GC_SESSION_AGE`  | `604800`                                                                   |
| `waitStaleDays`   | `ROOKERY_WAIT_STALE_DAYS` | `14`                                                                       |
| `gc.onCalendar`   |                           | `daily`                                                                    |
| `hooks.*`         | `ROOKERY_HOOK_*`          | empty                                                                      |

`gc.enable` installs a `rookery-gc` systemd user service and timer. The service
exits cleanly when `gc.repo` is absent, so a host whose checkout is optional
stays green. Consumers that already run their own GC pass should leave
`gc.enable` off and call `<finalPackage>/bin/rookery gc` themselves.

`tui.enable` installs `rookery-tui`, and `tui.package` overrides the package.
The TUI has no environment counterpart.

## Environment

- `ROOKERY_V2=1` runs the `opencode2` entry point instead of `opencode`
- `ROOKERY_TASK_STALE_DAYS` (default 14) surfaces an open task that has not
  been touched; `task note` resets the clock
- `ROOKERY_BOARD_JOBS` (default 8, clamped 1-32) bounds the board's parallel work
- `ROOKERY_LIVE_EXCLUDE` adds a `grep -E` pattern to the liveness exclusion
- `ROOKERY_HOOK_{PRE,POST}_{CREATE,DELETE}` run around create and remove, with
  `ROOKERY_TASK`, `ROOKERY_DIR`, `ROOKERY_ROOT`, and `ROOKERY_BASE` exported

The `OCWS_*` names remain accepted for one release. They are read only when the
matching `ROOKERY_*` name is unset, so set the new name when both matter.

## Migration from ocws

Version 0.2.0 renames the project from `ocws` to Rookery. The command `ocws`
stays installed as an alias to `rookery`, and the `OCWS_*` environment names
stay readable as fallbacks. The base directory default remains
`$XDG_DATA_HOME/ocws` and existing workspace directories, wait markers, the
`ls --counts` order, and the `seat --json` fields are unchanged, so an existing
fleet keeps classifying and reclaiming as before.

## Development

```sh
nix develop
nix fmt
nix flake check
nix build .#rookery
nix build .#rookery-tui
nix build .#rookery-legacy
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
bash tests/board.sh
bash tests/tasks.sh
```

`tests/board.sh` builds a throwaway jj repository with a local git origin and
exercises classification, the cache, liveness, sessions, wait markers, the
prefix seam, gc, the `ocws` alias, and the `OCWS_*` fallback. It never touches a
real workspace or the user's cache.

`tests/tasks.sh` adds the task layer against the same kind of fixture and a fake
herdr: the NDJSON board, store writes and concurrent writers, maintenance across
rename and removal, cleanup marks and the sweep, seat focus, task seats, and
corrupt-store handling.

The workspace tests cover classification, the cache key, absorption, the task
store's atomicity, the lifecycle safety checks, lane derivation, key handling,
and the `TestBackend` buffer snapshots. `checks.rookery-parity` runs the same
fixture against the v0.4.0 shell and the Rust CLI.

## License

MIT. See `LICENSE`.
