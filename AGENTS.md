# AGENTS.md

Repository guidance for AI agents working on `rookery`.

## Scope

`rookery` is a jj workspace manager for coding agents. The core is a Rust
Cargo workspace (`core/`), and the CLI (`cli/`) and the board (`tui/`) are
frontends over it. The CLI keeps the text and JSON consumer contract; the TUI
links the core and renders its read model. The v0.4.0 bash implementation
remains as `rookery-legacy` for one release as the parity oracle and rollback;
it is frozen. The agent/terminal backend abstraction is planned, not present;
do not introduce it beyond the `SeatBackend` and `SessionProvider` traits.

## Layout

- `core/`: rookery-core, the library: config, jj, tasks, lifecycle, seats,
  the board read model and the derived lanes
- `cli/`: the `rookery` binary (clap)
- `tui/`: the board, a workspace member that depends on `rookery-core`
- `packages/rookery.nix`: the v0.4.0 shell, shipped as `rookery-legacy`
- `nix/module.nix`: `programs.rookery` options, the configured CLI and TUI
  wrappers, and the GC service
- `tests/board.sh`, `tests/tasks.sh`, `tests/reap.sh`: behavioral tests for the
  shipping CLI
- `tests/parity.sh`: the legacy shell and the Rust CLI on the same fixtures
- `flake.nix`: packages, module export, checks, dev shell, formatting

## Rules

- The public tree is sanitized: no fleet hostnames, private paths, incident
  details, tokens, or notification URLs. Comments describe current behavior;
  leave out the history of how it got there.
- One concern per change.
- `ROOKERY_*` names, the `rookery` command, the eight-field `ls --counts`
  order, the board headings, the `seat --json` field names, the `ls --json`
  field names and their nullability, and the `tasks.json` document shape are
  consumer contracts. Change them only with a compatibility path and a test.
  The `ocws` command alias and the `OCWS_*` fallbacks are the v0.2.0 migration
  path; keep them until the separate compatibility cleanup.
- The CLI and TUI are two frontends over `rookery-core`. The TUI never reads
  the task store or the jj store directly, and never calls herdr or jj; it
  calls the core, which calls the backends. Lanes are derived in
  `core/src/board.rs`.
- Task writes go through `Store::mutate`; read paths stay lock-free because the
  document is replaced atomically. A clean marker is a request, not a stored
  lane.
- The reaper exports every member of a session tree and validates each export
  before deleting the root. One failed export keeps the whole tree. Only a
  directory that no longer exists is a candidate; a live workspace and a
  subdirectory of one are never touched. The agent entry point is the same one
  the seats use (`ROOKERY_V2`/`ROOKERY_AGENT_BIN`).
- The task status is the lifecycle authority: `open` holds its workspaces out
  of `gc` and `clean --apply`, `done` releases them, `archived` hides the task.
  `task done` must refuse a caller inside a grouped workspace; only the lead
  finishes a task. Keep the marker-then-status write order so a crash leaves an
  open task that a retry can finish.
- The package wraps jj, rip, direnv and the other runtime inputs on PATH.
  Consumers that follow their own nixpkgs get that nixpkgs's jj; do not assume
  the version in this flake is the one at runtime.
- Never rename or migrate a user's base directory, marker files, or workspace
  names in a mechanical change.
- `rookery-legacy` is frozen: it exists for parity and rollback, not for
  features or fixes.

## Validation

```sh
nix fmt
nix flake check
nix build .#rookery
nix build .#rookery-tui
nix build .#rookery-legacy
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
bash tests/reap.sh
```

`nix flake check` builds `checks.rookery-board`, `checks.rookery-tasks`,
`checks.rookery-reap`, `checks.rookery-tui`, and `checks.rookery-parity` (the
shell oracle against the Rust CLI). CI runs all of the above on every push and
pull request.
