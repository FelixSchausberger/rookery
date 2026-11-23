# ocws

`ocws` gives every agent its own jj workspace in one repository. It creates the
workspace, claims it with a change description, starts OpenCode rooted there,
classifies every workspace from jj state, and reclaims the finished ones. It
also drives herdr seats: a new workspace, a new herdr tab, and a lookup from an
OpenCode session back to its pane.

The name is historical. This is not a general terminal or agent abstraction: the
seat path targets herdr and OpenCode, and the classifier targets jj. A board and
a wider set of agent and terminal backends are planned, not present.

This project is the extracted workspace manager of a personal NixOS fleet. The
fleet pins it as a flake input and configures it through the bundled Home
Manager module.

## Requirements

- Linux (the liveness probe reads `/proc`), bash, git
- jj 0.38 or newer (`ocws` uses the `root` field of `jj workspace list`)
- sqlite3 for the OpenCode session probe; without it, liveness falls back to
  processes and timestamps
- herdr and OpenCode for `ocws herdr`, `ocws seat`, and `ocws title`

## Install

As a flake input:

```nix
{
  inputs.ocws.url = "github:FelixSchausberger/ocws";
  inputs.ocws.inputs.nixpkgs.follows = "nixpkgs";
}
```

and in a Home Manager module:

```nix
{
  imports = [inputs.ocws.homeManagerModules.default];

  programs.ocws = {
    enable = true;
    base = "${config.home.homeDirectory}/.local/share/ocws";
    workspacePrefix = "ws-";
    trunk = "main@origin";
  };
}
```

The module installs the configured binary, which carries the options as
environment defaults. A runtime `OCWS_*` variable still wins.

Without Nix, run the package definition with `nix build` or copy
`packages/ocws.nix` and its runtime dependencies.

## Commands

| command                                              | effect                                                         |
| ---------------------------------------------------- | -------------------------------------------------------------- |
| `ocws <task> [-- <opencode args>]`                   | create a workspace and start OpenCode there                    |
| `ocws herdr [--json] [--no-focus] [--wait] [<task>]` | create a workspace in a new herdr tab and start OpenCode       |
| `ocws tab <task>`                                    | create a workspace in a new zellij tab                         |
| `ocws seat [--json]`                                 | print the calling agent's herdr workspace, tab, and pane       |
| `ocws title <text...>`                               | claim the current seat: describe, rename the workspace and tab |
| `ocws ls [--counts\|--board] [--refresh]`            | classify every workspace                                       |
| `ocws wait <task> [reason...]`                       | park a workspace; gc keeps it until `ocws go`                  |
| `ocws go <task>`                                     | release a parked workspace                                     |
| `ocws done [--force] <task>`                         | remove a finished workspace                                    |
| `ocws rm [--force] <task>`                           | remove a workspace                                             |
| `ocws gc`                                            | reclaim empty and merged workspaces idle past the age gate     |

`ocws ls --counts` prints eight integers in this order: total, empty, merged,
unpushed, in-flight, conflict, stale, waitold. The order is a consumer contract.

## Workspace states

A workspace is classified by the commits reachable from its `@`:

- `empty`: nothing outside the trunk carries content
- `merged`: off-trunk content is fully absorbed into the trunk
- `unpushed`: off-trunk content with no remote bookmark
- `in-flight`: off-trunk content whose bookmark is pushed (an open PR)
- `conflict`: `@` or an off-trunk commit carries a conflict

Absorption is a content test, not an ancestry test: a squash-merge lands the
same bytes under a different commit, so the draft is finished even though it is
not an ancestor of the trunk. That is what lets `ocws done` clean up after a
squash-merged PR.

The board cache is keyed on the jj operation id, the trunk commit, and the
workspace list, so a describe, push, fetch, workspace add, forget, or rename
invalidates it. Liveness, session recency, and wait markers are read fresh on
every board; they never come from the cache.

## Liveness and GC

`ocws gc` removes empty and merged workspaces that sit idle past a gate and
have no live process. It never removes:

- a workspace with unpushed, in-flight, or conflicted work
- a workspace with a process rooted in it
- a workspace with an unarchived OpenCode session updated within the session
  age window
- a parked workspace (`ocws wait`), whatever its state

Per-project MCP and LSP daemons inherit the spawning session's cwd and outlive
it, so the process probe excludes known auxiliary binaries and accepts extra
patterns through `OCWS_LIVE_EXCLUDE`. A process whose command line cannot be
read counts as live; `ocws` never kills.

Deletion goes through `rip`, so a mistaken reclaim is restorable.

## Configuration

The module options cover the base directory, workspace prefix, trunk, gc gates
and schedule, and the four lifecycle hooks. Every one maps to an environment
variable:

| option            | variable               | default                                                                    |
| ----------------- | ---------------------- | -------------------------------------------------------------------------- |
| `base`            | `OCWS_BASE`            | sibling of the workspace, then `$XDG_DATA_HOME/ocws`, then `/tmp/opencode` |
| `workspacePrefix` | `OCWS_WS_PREFIX`       | `ws-`                                                                      |
| `trunk`           | `OCWS_TRUNK`           | `main@origin`                                                              |
| `gc.age`          | `OCWS_GC_AGE`          | `86400`                                                                    |
| `gc.emptyAge`     | `OCWS_GC_EMPTY_AGE`    | `21600`                                                                    |
| `gc.sessionAge`   | `OCWS_GC_SESSION_AGE`  | `604800`                                                                   |
| `waitStaleDays`   | `OCWS_WAIT_STALE_DAYS` | `14`                                                                       |
| `gc.onCalendar`   |                        | `daily`                                                                    |
| `hooks.*`         | `OCWS_HOOK_*`          | empty                                                                      |

`gc.enable` installs an `ocws-gc` systemd user service and timer. The service
exits cleanly when `gc.repo` is absent, so a host whose checkout is optional
stays green. Consumers that already own a unit named `ocws-gc` should leave
`gc.enable` off and call `<finalPackage>/bin/ocws gc` themselves.

## Environment

- `OCWS_V2=1` runs the `opencode2` entry point instead of `opencode`
- `OCWS_BOARD_JOBS` (default 8, clamped 1-32) bounds the board's parallel work
- `OCWS_LIVE_EXCLUDE` adds a `grep -E` pattern to the liveness exclusion
- `OCWS_HOOK_{PRE,POST}_{CREATE,DELETE}` run around create and remove, with
  `OCWS_TASK`, `OCWS_DIR`, `OCWS_ROOT`, and `OCWS_BASE` exported

## Development

```sh
nix develop
nix fmt
nix flake check
bash tests/board.sh
```

`tests/board.sh` builds a throwaway jj repository with a local git origin and
exercises classification, the cache, liveness, sessions, wait markers, the
prefix seam, and gc. It never touches a real workspace or the user's cache.

## License

MIT. See `LICENSE`.
