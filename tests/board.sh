#!/usr/bin/env bash
# Fixture test for the Rookery board: classification, cache invalidation and
# the freshness of liveness, session and wait state. Runs the command on PATH
# against a throwaway jj repository with a local git origin; nothing touches
# the real fleet or the user's cache.
#
# The cache key covers the jj state only. Everything that can change without a
# jj operation (processes, recent sessions, wait markers) must show up on the
# next board even when the cached classification is reused, which is what the
# live/session/wait cases assert.
set -euo pipefail

fail() {
	printf 'FAIL: %s\n' "$*" >&2
	exit 1
}

# section <board> <header> prints the rows under a board header.
section() {
	printf '%s\n' "$1" | awk -v h="$2" 'substr($0, 1, 2) == "  " && index($0, h) { f = 1; next } f && /^$/ { exit } f'
}

has() { section "$1" "$2" | grep -Fq -- "$3" || fail "$3 missing from '$2'"; }
has_not() { if section "$1" "$2" | grep -Fq -- "$3"; then fail "$3 unexpectedly in '$2'"; fi; }

command -v rookery >/dev/null 2>&1 || fail "rookery not on PATH"
command -v jj >/dev/null 2>&1 || fail "jj not on PATH"
command -v sqlite3 >/dev/null 2>&1 || fail "sqlite3 not on PATH"

work=$(mktemp -d)
trap 'chmod -R u+w "$work" 2>/dev/null; rm -rf "$work"' EXIT
export HOME="$work/home"
export XDG_CACHE_HOME="$work/cache"
export XDG_DATA_HOME="$work/data"
export ROOKERY_BASE="$work/base"
# The fixture follows the package default so the default is what the main
# flow exercises; the override is checked separately at the end.
ws_prefix="${ROOKERY_WS_PREFIX:-ws-}"
mkdir -p "$HOME" "$XDG_CACHE_HOME" "$XDG_DATA_HOME" "$ROOKERY_BASE"

# A git origin with a main branch, fetched into a colocated jj repository.
git init -q "$work/origin"
(
	cd "$work/origin"
	echo base >file.txt
	git add file.txt
	git -c user.name=t -c user.email=t@t commit -q -m base
	git branch -M main
)

mkdir "$work/repo"
cd "$work/repo"
jj git init --colocate . >/dev/null 2>&1
jj config set --repo user.name "rookery test" >/dev/null 2>&1
jj config set --repo user.email "rookery@example.invalid" >/dev/null 2>&1
git remote add origin "$work/origin"
jj git fetch >/dev/null 2>&1
jj bookmark track main@origin >/dev/null 2>&1

# A repository with only the primary workspace must classify too. jj renders
# an empty working-copy name for the sole primary, so the target has to come
# from the workspace list rather than the working-copy template.
[ "$(rookery ls --counts --refresh)" = "0 0 0 0 0 0 0 0" ] || fail "fresh repo counts"
rookery ls --board | grep -Fq "rookery board -" || fail "fresh repo board"

jj workspace add "$ROOKERY_BASE/${ws_prefix}alpha" --name alpha -r main@origin -m "alpha: started" >/dev/null 2>&1
jj workspace add "$ROOKERY_BASE/${ws_prefix}beta" --name beta -r main@origin -m "beta: started" >/dev/null 2>&1

# alpha carries unpushed content; beta stays an empty workspace on main.
(
	cd "$ROOKERY_BASE/${ws_prefix}alpha"
	echo alpha >a.txt
	jj describe -m "alpha: first" >/dev/null 2>&1
)

warm=$(rookery ls --board)
fresh=$(rookery ls --board --refresh)
[ "$warm" = "$fresh" ] || fail "warm board differs from --refresh"
serial=$(ROOKERY_BOARD_JOBS=1 rookery ls --board --refresh)
[ "$serial" = "$fresh" ] || fail "serial board differs from the parallel board"

has "$fresh" "IN PROGRESS (unpushed)" "alpha: first"
has "$fresh" "RECLAIMABLE" "beta"
counts=$(rookery ls --counts)
[ "$(printf '%s' "$counts" | wc -w)" -eq 8 ] || fail "counts is not 8 integers: $counts"
[ "$counts" = "2 1 0 1 0 0 1 0" ] || fail "unexpected counts: $counts"
[ "$(rookery ls --counts --refresh)" = "$counts" ] || fail "counts differ on --refresh"

# A description change moves the workspace target, so the cache must miss.
(
	cd "$ROOKERY_BASE/${ws_prefix}alpha"
	jj describe -m "alpha: second" >/dev/null 2>&1
)
warm=$(rookery ls --board)
has "$warm" "IN PROGRESS (unpushed)" "alpha: second"
if section "$warm" "IN PROGRESS (unpushed)" | grep -Fq "alpha: first"; then
	fail "stale description survived the cache"
fi

# A remote bookmark makes the same work in-flight.
(
	cd "$ROOKERY_BASE/${ws_prefix}alpha"
	jj bookmark create alpha-work -r @ >/dev/null 2>&1
	jj git push --bookmark alpha-work >/dev/null 2>&1
)
warm=$(rookery ls --board)
has "$warm" "IN REVIEW (open PR)" "alpha"
has_not "$warm" "IN PROGRESS (unpushed)" "alpha"

# Landing the same content on main@origin marks the draft absorbed.
(
	cd "$work/origin"
	echo alpha >a.txt
	git add a.txt
	git -c user.name=t -c user.email=t@t commit -q -m "land alpha"
)
jj git fetch >/dev/null 2>&1
warm=$(rookery ls --board)
has "$warm" "RECLAIMABLE" "alpha"
has_not "$warm" "IN REVIEW (open PR)" "alpha"

# Wait markers are read fresh, never cached.
rookery wait alpha "blocked on review" >/dev/null
warm=$(rookery ls --board)
has "$warm" "PARKED (waiting on the outside world)" "parked: blocked on review"
rookery go alpha >/dev/null
warm=$(rookery ls --board)
has_not "$warm" "PARKED (waiting on the outside world)" "alpha"

# A live process is read fresh, never cached.
(
	cd "$ROOKERY_BASE/${ws_prefix}beta"
	exec sleep 30
) &
sleeper=$!
sleep 0.3
warm=$(rookery ls --board)
has "$warm" "CLAIMED (session active)" "beta"
kill "$sleeper" 2>/dev/null || true
wait "$sleeper" 2>/dev/null || true
warm=$(rookery ls --board)
has "$warm" "RECLAIMABLE" "beta"

# A recent opencode session is read fresh, never cached.
mkdir -p "$XDG_DATA_HOME/opencode"
sqlite3 "$XDG_DATA_HOME/opencode/opencode.db" \
	"create table session_v2 (directory text, time_archived integer, time_updated integer);
	insert into session_v2 values ('$ROOKERY_BASE/${ws_prefix}beta', null, $(($(date +%s) * 1000)));"
warm=$(rookery ls --board)
has "$warm" "CLAIMED (session active)" "beta"
sqlite3 "$XDG_DATA_HOME/opencode/opencode.db" "update session_v2 set time_updated = 0;"
warm=$(rookery ls --board)
has "$warm" "RECLAIMABLE" "beta"

# A workspace add moves the list in the key.
jj workspace add "$ROOKERY_BASE/${ws_prefix}gamma" --name gamma -r main@origin -m "gamma: started" >/dev/null 2>&1
warm=$(rookery ls --board)
has "$warm" "RECLAIMABLE" "gamma"

# A forget removes it again.
jj workspace forget gamma >/dev/null 2>&1
warm=$(rookery ls --board)
has_not "$warm" "RECLAIMABLE" "gamma"

# A corrupt cache is a miss, not a wrong board.
expected=$(rookery ls --board --refresh)
cache=$(find "$XDG_CACHE_HOME/rookery" -name 'board-*' ! -name '*.lock' | head -1)
[ -n "$cache" ] || fail "no board cache was written"
printf 'garbage\n' >"$cache"
[ "$(rookery ls --board)" = "$expected" ] || fail "corrupt cache changed the board"

# An unwritable cache directory is a miss, not a failure or stderr noise. Drop
# the lock too: an existing lock file stays writable through a read-only
# directory and would hide the open failure this guards against.
rm -f "$cache" "$cache.lock"
chmod 500 "$XDG_CACHE_HOME/rookery"
if ! out=$(rookery ls --board 2>"$work/unwritable.err"); then
	fail "unwritable cache failed the board"
fi
[ "$out" = "$expected" ] || fail "unwritable cache changed the board"
[ -s "$work/unwritable.err" ] && fail "unwritable cache wrote to stderr"
chmod 700 "$XDG_CACHE_HOME/rookery"
[ "$(rookery ls --board)" = "$expected" ] || fail "board did not recover after the cache became writable"

# Two concurrent cold scans agree and leave a valid cache behind.
rm -f "$cache"
rookery ls --board --refresh >"$work/a" &
first=$!
rookery ls --board --refresh >"$work/b" &
second=$!
wait "$first"
wait "$second"
cmp -s "$work/a" "$work/b" || fail "concurrent boards differ"
[ "$(rookery ls --board)" = "$(cat "$work/a")" ] || fail "cache after concurrent scans is wrong"

# gc still classifies through the per-workspace path and keeps live work.
(
	cd "$ROOKERY_BASE/${ws_prefix}alpha"
	echo more >>a.txt
	jj describe -m "alpha: more" >/dev/null 2>&1
)
rookery gc 2>&1 | grep -Fq "keeping 'alpha'" || fail "gc did not keep alpha's unpushed work"

# The table rendering is unchanged and still works.
rookery ls | grep -Fq "Workspace states:" || fail "table rendering missing"

# Creation honors the workspace prefix. A fake opencode ends the launch, so
# the created directory is the observable: the default prefix and a consumer
# override are both checked.
mkdir -p "$work/bin"
printf '#!%s\nexit 0\n' "$(command -v bash)" >"$work/bin/opencode"
chmod +x "$work/bin/opencode"
(
	cd "$work/repo"
	PATH="$work/bin:$PATH" rookery default-seat >/dev/null 2>&1
)
[ -d "$ROOKERY_BASE/${ws_prefix}default-seat" ] || fail "default workspace prefix was not applied"
(
	cd "$work/repo"
	PATH="$work/bin:$PATH" ROOKERY_WS_PREFIX=nixos-ws- rookery override-seat >/dev/null 2>&1
)
[ -d "$ROOKERY_BASE/nixos-ws-override-seat" ] || fail "workspace prefix override was not applied"

# The pre-rename command is installed and resolves to the same binary.
command -v ocws >/dev/null 2>&1 || fail "ocws alias not installed"
[ "$(ocws ls --counts)" = "$(rookery ls --counts)" ] || fail "ocws alias differs"

# The pre-rename environment names are read when the new names are unset.
# The fixture base is fixed, so the created directory proves the old prefix
# was honored rather than the default.
(
	cd "$work/repo"
	PATH="$work/bin:$PATH" ROOKERY_WS_PREFIX='' OCWS_WS_PREFIX=ocws-ws- \
		ocws legacy-seat >/dev/null 2>&1
)
[ -d "$ROOKERY_BASE/ocws-ws-legacy-seat" ] || fail "OCWS_WS_PREFIX fallback was not applied"

printf 'Rookery board fixture: ok\n'
