# AGENTS.md

Repository guidance for AI agents working on `ocws`.

## Scope

`ocws` is a jj workspace seat manager for OpenCode agents in herdr. The core is
a bash `writeShellApplication`; the Home Manager module wraps it. The board and
the agent/terminal backend abstraction are planned, not present. Do not
introduce them while fixing the shell.

## Layout

- `packages/ocws.nix`: the command, its runtime inputs, and the environment
  contract (`OCWS_*`)
- `nix/module.nix`: `programs.ocws` options, the configured wrapper, the GC
  service and timer
- `tests/board.sh`: the behavioral test
- `flake.nix`: packages, module export, checks, dev shell, formatting

## Rules

- The public tree is sanitized: no fleet hostnames, private paths, incident
  details, tokens, or notification URLs. Comments describe current behavior;
  leave out the history of how it got there.
- One concern per change.
- `OCWS_*` names, the `ocws` command, the eight-field `ls --counts` order, the
  board headings, and `seat --json` field names are consumer contracts. Change
  them only with a compatibility path and a test.
- The package bundles jj through `runtimeInputs`. Consumers that follow their
  own nixpkgs get that nixpkgs's jj; do not assume the version in this flake is
  the one at runtime.
- Never rename or migrate a user's base directory, marker files, or workspace
  names in a mechanical change.

## Validation

```sh
nix fmt
nix flake check
nix build .#ocws
bash tests/board.sh
```

`nix flake check` builds `checks.ocws-board`, which is the same test. CI runs
all of the above on every push and pull request.
