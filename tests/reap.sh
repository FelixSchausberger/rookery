#!/usr/bin/env bash
# Fixture test for the session reaper: a conversation whose working directory
# is gone is exported and deleted, a live workspace or a subdirectory of one is
# skipped, and a failed export keeps the whole tree. The fake opencode records
# every export and delete, so the export-before-delete order is observable.
set -euo pipefail

fail() {
	printf 'FAIL: %s\n' "$*" >&2
	exit 1
}

command -v rookery >/dev/null 2>&1 || fail "rookery not on PATH"
command -v jq >/dev/null 2>&1 || fail "jq not on PATH"
command -v jj >/dev/null 2>&1 || fail "jj not on PATH"
command -v sqlite3 >/dev/null 2>&1 || fail "sqlite3 not on PATH"

work=$(mktemp -d)
trap 'chmod -R u+w "$work" 2>/dev/null; rm -rf "$work"' EXIT
export HOME="$work/home"
export XDG_CACHE_HOME="$work/cache"
export XDG_DATA_HOME="$work/data"
export ROOKERY_BASE="$work/base"
export ROOKERY_SESSION_ARCHIVE="$work/archive"
export FAKE_OPENCODE_LOG="$work/opencode.log"
mkdir -p "$HOME" "$XDG_CACHE_HOME" "$XDG_DATA_HOME" "$ROOKERY_BASE" "$work/bin"
: >"$FAKE_OPENCODE_LOG"

bash_bin="$(command -v bash)"
cat >"$work/bin/opencode" <<EOF
#!$bash_bin
set -euo pipefail
log="\${FAKE_OPENCODE_LOG:?}"
case "\${1:-} \${2:-}" in
  "session export")
    id="\${3:-}"
    printf 'export %s\n' "\$id" >>"\$log"
    if [ "\$id" = "ses_fail_child" ]; then
      echo "export refused" >&2
      exit 1
    fi
    printf '{"id":"%s"}\n' "\$id"
    ;;
  "session delete")
    printf 'delete %s\n' "\${3:-}" >>"\$log"
    ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$work/bin/opencode"
export PATH="$work/bin:$PATH"

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
jj workspace add "$ROOKERY_BASE/ws-alpha" --name alpha -r main@origin -m "alpha: started from main@origin" >/dev/null 2>&1

mkdir -p "$XDG_DATA_HOME/opencode"
sqlite3 "$XDG_DATA_HOME/opencode/opencode.db" \
	"create table session_v2 (
	   id text primary key,
	   parent_id text,
	   directory text,
	   title text,
	   slug text,
	   time_updated integer,
	   time_archived integer
	 );
	 insert into session_v2 values
	   ('ses_dead', null, '$ROOKERY_BASE/ws-dead', 'Dead conversation', 'dead', 1000, null),
	   ('ses_dead_child', 'ses_dead', '$ROOKERY_BASE/ws-dead', null, 'child', 1000, null),
	   ('ses_live', null, '$ROOKERY_BASE/ws-alpha', 'Live conversation', 'live', 2000, null),
	   ('ses_fail', null, '$ROOKERY_BASE/ws-fail', 'Fail conversation', 'fail', 3000, null),
	   ('ses_fail_child', 'ses_fail', '$ROOKERY_BASE/ws-fail', null, 'fail-child', 3000, null);"

# --- dry run -------------------------------------------------------------
out=$(rookery reap --dry-run)
printf '%s' "$out" | grep -Fq "would export and delete 'ses_dead' (Dead conversation)" || fail "dry run did not list the dead conversation: $out"
printf '%s' "$out" | grep -Fq "would export and delete 'ses_fail' (Fail conversation)" || fail "dry run did not list the failing conversation: $out"
[ ! -s "$FAKE_OPENCODE_LOG" ] || fail "dry run invoked the agent: $(cat "$FAKE_OPENCODE_LOG")"
[ ! -e "$ROOKERY_SESSION_ARCHIVE" ] || fail "dry run created the archive"

# --- the real sweep ------------------------------------------------------
out=$(rookery reap)
printf '%s' "$out" | grep -Fq "exported and deleted 'ses_dead' (Dead conversation)" || fail "dead conversation was not reaped: $out"
printf '%s' "$out" | grep -Fq "keeping 'ses_fail'" || fail "failed export did not keep its tree: $out"
printf '%s' "$out" | grep -Fq "1 exported and deleted, 1 kept" || fail "summary is wrong: $out"

grep -Fqx "export ses_dead" "$FAKE_OPENCODE_LOG" || fail "root export missing"
grep -Fqx "export ses_dead_child" "$FAKE_OPENCODE_LOG" || fail "child export missing"
grep -Fqx "delete ses_dead" "$FAKE_OPENCODE_LOG" || fail "root delete missing"
grep -Fqx "export ses_fail" "$FAKE_OPENCODE_LOG" || fail "failing tree root was not attempted"
grep -Fqx "export ses_fail_child" "$FAKE_OPENCODE_LOG" || fail "failing tree child was not attempted"
! grep -Fqx "delete ses_fail" "$FAKE_OPENCODE_LOG" || fail "a tree with a failed export was deleted"
! grep -Fq "ses_live" "$FAKE_OPENCODE_LOG" || fail "a live workspace conversation was touched"

[ -f "$ROOKERY_SESSION_ARCHIVE/ses_dead/ses_dead-dead.json" ] || fail "root export missing from archive"
[ -f "$ROOKERY_SESSION_ARCHIVE/ses_dead/ses_dead_child-child.json" ] || fail "child export missing from archive"
[ ! -f "$ROOKERY_SESSION_ARCHIVE/ses_fail/ses_fail_child-fail-child.json" ] || fail "a failed export was archived"
[ -f "$ROOKERY_SESSION_ARCHIVE/ses_fail/ses_fail-fail.json" ] || fail "the successful part of the failing tree was not archived"

# --- a host without OpenCode --------------------------------------------
mv "$XDG_DATA_HOME/opencode/opencode.db" "$work/opencode.db"
out=$(rookery reap)
printf '%s' "$out" | grep -Fq "no session database" || fail "missing database was not reported: $out"

printf 'Rookery reap fixture: ok\n'
