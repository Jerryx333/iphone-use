#!/usr/bin/env bash
# Extract only the registration function; never touch a real Codex config.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/iphone-use-codex-mcp.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
FN="$(sed -n '/^register_codex_mcp() {$/,/^}$/p' "$ROOT/install.sh")"
[ -n "$FN" ] || { echo 'function not found' >&2; exit 1; }
mkdir -p "$TMP/home" "$TMP/bin" "$TMP/empty"
export LOG="$TMP/args" GET_RC=1 GET_OUTPUT='' ADD_RC=0
cat > "$TMP/bin/codex" <<'MOCK'
#!/bin/bash
printf '<%s>\n' "$@" >> "$LOG"
case "$2" in
    get) printf '%s' "$GET_OUTPUT"; exit "$GET_RC" ;;
    add) exit "$ADD_RC" ;;
    *) exit 99 ;;
esac
MOCK
chmod +x "$TMP/bin/codex"
pass=0
ok_t() { pass=$((pass + 1)); printf 'ok %d - %s\n' "$pass" "$1"; }
bad() { printf 'not ok - %s\n' "$1" >&2; exit 1; }
run() {
    : > "$LOG"
    HOME="$TMP/home" PATH="$1" /bin/bash -euc "
        ok() { printf 'OK %s\\n' \"\$*\"; }
        warn() { printf 'WARN %s\\n' \"\$*\"; }
        $FN
        register_codex_mcp \"\$@\"
    " test "$MCP" "$NAME" "${2:-}"
}
MCP="$TMP/home/Applications/An App.app/Contents/MacOS/iphone-use-mcp"
NAME=iphone-use
out="$(run "$TMP/empty")"
[ -z "$out" ] && [ ! -s "$LOG" ] || bad 'missing codex must be silent'
ok_t 'no codex silently skips'
check_args() {
    printf '<%s>\n' mcp get "$NAME" --json mcp add "$NAME" "$@" -- "$MCP" > "$TMP/expected"
    diff -u "$TMP/expected" "$LOG" || bad 'incorrect arguments'
}
out="$(run "$TMP/bin:/usr/bin:/bin")"
check_args
[[ "$out" == *'Start a new Codex session'* ]] || bad 'missing success hint'
ok_t 'fresh default registration preserves binary path as one argument'
NAME=iphone-use-office
out="$(run "$TMP/bin:/usr/bin:/bin" office)"
check_args --env PHONE_REMOTE_INSTANCE=office
ok_t 'instance registration passes --env'
GET_RC=0 GET_OUTPUT="{\"command\":\"$MCP\"}"
out="$(run "$TMP/bin:/usr/bin:/bin")"
! grep -q '<add>' "$LOG" || bad 'same command was added again'
[[ "$out" == *'OK Codex already has'* ]] || bad 'missing existing success'
ok_t 'same command is retained'
GET_OUTPUT='{"command":"/some/other/server"}'
out="$(run "$TMP/bin:/usr/bin:/bin")"
! grep -q '<add>' "$LOG" || bad 'different command overwritten'
[[ "$out" == *'WARN'*'another command; left it unchanged'* ]] || bad 'missing conflict warning'
ok_t 'different command is retained with warning'
GET_RC=1 ADD_RC=1
for instance in '' office; do
    out="$(run "$TMP/bin:/usr/bin:/bin" "$instance")" || bad 'add failure interrupted installer'
    [[ "$out" == *'WARN'*'codex mcp add'* ]] || bad 'missing manual command'
    [[ "$out" == *"\"$MCP\""* ]] || bad 'manual binary path not quoted'
    if [ -n "$instance" ]; then
        [[ "$out" == *'--env "PHONE_REMOTE_INSTANCE=office"'* ]] || bad 'manual command lost instance'
    fi
done
ok_t 'add failures warn with manual commands and return zero'
printf '1..%d\n' "$pass"
