# AGENTS.md

Repository guidance for AI agents working on `rookery`.

## Scope

`rookery` is a jj workspace manager for coding agents. The core is a bash
`writeShellApplication`; the Home Manager module wraps it. The current seat
path targets herdr and OpenCode; the board is exposed as NDJSON through
`ls --json` and `task list --json`, and `rookery-tui` renders it. The
agent/terminal backend abstraction is planned, not present; do not introduce it
while fixing the shell.

## Layout

- `packages/rookery.nix`: the command, its runtime inputs, and the environment
  contract (`ROOKERY_*`)
- `nix/module.nix`: `programs.rookery` options (including `tui.enable`), the
  configured wrapper, the GC service and timer
- `tui/`: the board client, a standalone Rust crate with its own lock file
- `tests/board.sh`: the classification and cache test
- `tests/tasks.sh`: the task layer, cleanup and seat test
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
  path; do not remove them before v0.3.0.
- The TUI consumes the CLI only: `ls --json`, `task list --json`, and the
  write verbs. Never read the task store or the jj store from a client, and
  never call herdr or jj from it. Lanes live in `tui/src/model.rs` and are
  derived from the two streams.
- Task writes go through `store_mutate`; read paths stay lock-free because the
  document is replaced atomically. A clean marker is a request, not a stored
  lane.
- The package bundles jj through `runtimeInputs`. Consumers that follow their
  own nixpkgs get that nixpkgs's jj; do not assume the version in this flake is
  the one at runtime.
- Never rename or migrate a user's base directory, marker files, or workspace
  names in a mechanical change.

## Validation

```sh
nix fmt
nix flake check
nix build .#rookery
nix build .#rookery-tui
bash tests/board.sh
bash tests/tasks.sh
cargo test --manifest-path tui/Cargo.toml
cargo clippy --manifest-path tui/Cargo.toml --all-targets -- -D warnings
```

`nix flake check` builds `checks.rookery-board`, `checks.rookery-tasks`, and
`checks.rookery-tui` (the crate's tests). CI runs all of the above on every push
and pull request.
