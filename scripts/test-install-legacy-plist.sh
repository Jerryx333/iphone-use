#!/usr/bin/env bash
# Settings were PHONE_REMOTE_* before the rename to IPHONE_USE_*. An upgrade
# rebuilds the daemon plist from the one already installed, so the installer's
# readers must find every old key, prefer a new one, and never borrow an old
# key for a name outside the IPHONE_USE_ prefix. The functions are extracted
# from install.sh and auto-update.sh and run against throwaway plists.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/iphone-use-legacy-plist.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

pass=0
ok_t() { pass=$((pass + 1)); printf 'ok %d - %s\n' "$pass" "$1"; }
bad()  { printf 'not ok - %s\n' "$1" >&2; exit 1; }

READERS="$(sed -n '/^plist_env_get_from() {$/,/^}$/p;/^plist_env_get() {$/,/^}$/p;/^env_or_existing() {$/,/^}$/p' "$ROOT/install.sh")"
DAEMON_ENV="$(sed -n '/^daemon_env() {/,/^}$/p' "$ROOT/scripts/auto-update.sh")"
[ -n "$READERS" ] || bad "plist readers not found in install.sh"
[ -n "$DAEMON_ENV" ] || bad "daemon_env not found in auto-update.sh"

cat > "$TMP/old.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>EnvironmentVariables</key><dict>
<key>PHONE_REMOTE_HOST</key><string>0.0.0.0</string>
<key>PHONE_REMOTE_PORT</key><string>45432</string>
<key>PHONE_REMOTE_PASSWORD</key><string>old-password</string>
<key>PHONE_REMOTE_AGENT_TOKEN</key><string>old-token</string>
<key>PHONE_REMOTE_WDA_URL</key><string>http://127.0.0.1:8100</string>
<key>IPHONE_USE_UDID</key><string>new-udid</string>
<key>PHONE_REMOTE_UDID</key><string>old-udid</string>
</dict></dict></plist>
PLIST

read_key() {  # key -> what the installer carries forward
    env -i HOME="$TMP" PATH=/usr/bin:/bin bash -c "$READERS
PLIST_DST='$TMP/old.plist'; OLD_PLIST='$TMP/none.plist'
env_or_existing '$1'"
}

[ "$(read_key IPHONE_USE_PORT)" = 45432 ] || bad "old PORT not carried forward"
[ "$(read_key IPHONE_USE_PASSWORD)" = old-password ] || bad "old PASSWORD not carried forward"
[ "$(read_key IPHONE_USE_AGENT_TOKEN)" = old-token ] || bad "old AGENT_TOKEN not carried forward"
ok_t "an old plist's settings are carried forward under the new names"

[ "$(read_key IPHONE_USE_UDID)" = new-udid ] || bad "the old key won over the new one"
ok_t "a new key wins over the old one"

[ -z "$(read_key WDA_URL)" ] || bad "WDA_URL borrowed PHONE_REMOTE_WDA_URL"
[ -z "$(read_key IPHONE_USE_MISSING)" ] || bad "a missing key read as non-empty"
ok_t "names outside the prefix and missing keys stay empty"

got="$(env -i HOME="$TMP" PATH=/usr/bin:/bin PHONE_REMOTE_PORT=1 bash -c "$READERS
$(sed -n '/^# Installs made before the rename set PHONE_REMOTE_/,/^unset _legacy$/p' "$ROOT/install.sh")
PLIST_DST='$TMP/old.plist'; OLD_PLIST='$TMP/none.plist'
env_or_existing IPHONE_USE_PORT")"
[ "$got" = 1 ] || bad "an old environment variable did not override the plist (got $got)"
ok_t "an old environment variable still overrides the plist"

# The Self Runner gateway settings (docs/design/self-runner-remote.md §14) live
# only in the plist; an upgrade that dropped them would silently stop routing
# the daemon through phone-gateway.
CARRY="$(sed -n '/^for ENV_KEY in \\$/,/^done$/{p;/^done$/q;}' "$ROOT/install.sh")"
case "$CARRY" in *IPHONE_USE_AGENT_TOKEN*) ;; *) bad "allow-list loop not found in install.sh" ;; esac
cat > "$TMP/gateway.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>EnvironmentVariables</key><dict>
<key>IPHONE_USE_SELFRUNNER_GATEWAY</key><string>http://127.0.0.1:7382</string>
<key>IPHONE_USE_SELFRUNNER_GATEWAY_TOKEN_FILE</key><string>/x/client-token</string>
</dict></dict></plist>
PLIST
got="$(env -i HOME="$TMP" PATH=/usr/bin:/bin bash -c "$READERS
append_plist_env() { [ -z \"\$2\" ] || printf '%s=%s\n' \"\$1\" \"\$2\"; }
PLIST_DST='$TMP/gateway.plist'; OLD_PLIST='$TMP/none.plist'
$CARRY")"
[ "$got" = "IPHONE_USE_SELFRUNNER_GATEWAY=http://127.0.0.1:7382
IPHONE_USE_SELFRUNNER_GATEWAY_TOKEN_FILE=/x/client-token" ] \
    || bad "an upgrade dropped the Self Runner gateway settings (got '$got')"
ok_t "an upgrade carries the Self Runner gateway settings forward"

got="$(DAEMON_PLIST="$TMP/old.plist" bash -c "$DAEMON_ENV
daemon_env")"
[ "$got" = "45432 old-token 0.0.0.0" ] || bad "auto-update read '$got' from an old plist"
ok_t "auto-update reads port, token and host from an old plist"

printf '1..%d\n' "$pass"
