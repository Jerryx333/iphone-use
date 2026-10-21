#!/usr/bin/env bash
# selfrunner-watchdog only calls for a person when one is needed: a problem must
# show on two probes, the same problem is not repeated until the reminder is
# due, a notification the command failed to deliver is retried on the next run,
# recovery is reported once, and nothing is probed while auto-resign reinstalls.
# The real script runs against a fake gateway/daemon and a recording notifier.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/selfrunner-watchdog.XXXXXX")"
SERVER=""
trap 'if [ -n "$SERVER" ]; then kill "$SERVER" 2>/dev/null; wait "$SERVER" 2>/dev/null || true; fi; rm -rf "$TMP"' EXIT

pass=0
ok_t() { pass=$((pass + 1)); printf 'ok %d - %s\n' "$pass" "$1"; }
bad()  { printf 'not ok - %s\n' "$1" >&2; exit 1; }

cat > "$TMP/fake.py" <<'PY'
import http.server, json, pathlib, sys
root = pathlib.Path(sys.argv[1])

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, code, body):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):  # the daemon: alive, but this probe has no password
        self.reply(401, {"error": "unauthorized"})

    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length") or 0))
        if self.headers.get("Authorization") != "Bearer test-token":
            return self.reply(401, {})
        spec = json.loads((root / "spec.json").read_text())
        if self.path == "/v1/status":
            return self.reply(200, {"transport": spec["transport"]})
        if self.path == "/v1/command":
            counter = root / "count"
            n = int(counter.read_text()) if counter.exists() else 0
            counter.write_text(str(n + 1))
            result = spec["results"][min(n, len(spec["results"]) - 1)]
            if result is None:
                return self.reply(200, {"ok": False, "error": {"code": "timeout"}})
            return self.reply(200, {"ok": True, "result": result})
        self.reply(404, {})

server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
(root / "port").write_text(str(server.server_address[1]))
server.serve_forever()
PY
python3 -I "$TMP/fake.py" "$TMP" &
SERVER=$!
for _ in $(seq 50); do [ -s "$TMP/port" ] && break; sleep 0.1; done
[ -s "$TMP/port" ] || bad "fake gateway did not start"
PORT="$(cat "$TMP/port")"
CLOSED="$(python3 -I -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"

mkdir -p "$TMP/gw" "$TMP/bin" "$TMP/autoresign"
printf 'test-token\n' > "$TMP/gw/client-token"
cat > "$TMP/bin/osascript" <<'SH'
#!/bin/sh
cat >/dev/null
SH
cat > "$TMP/notify" <<SH
#!/bin/sh
printf '%s|%s|%s|%s\n' "\$SR_NOTIFY_EVENT" "\$1" "\$2" "\${SR_NOTIFY_EXTRA:-}" >> "$TMP/sent"
[ ! -f "$TMP/notify_rc" ] || exit "\$(cat "$TMP/notify_rc")"
SH
chmod +x "$TMP/bin/osascript" "$TMP/notify"

GOOD='{"xctest_ready":true,"needs_passcode":false,"needs_relaunch":false,"runner_core":{"running":true}}'
PASSCODE='{"xctest_ready":false,"needs_passcode":true,"needs_relaunch":false,"runner_core":{"running":false}}'
RELAUNCH='{"xctest_ready":false,"needs_passcode":false,"needs_relaunch":true,"runner_core":{"running":false}}'
API_OFF='{"xctest_ready":true,"needs_passcode":false,"needs_relaunch":false,"runner_core":{"running":false}}'
phone() {  # transport, then one command result per probe (the last repeats)
  local transport="$1"; shift
  local IFS=,
  printf '{"transport":"%s","results":[%s]}\n' "$transport" "$*" > "$TMP/spec.json"
  rm -f "$TMP/count"
}
watchdog() {  # KEY=VALUE arguments override the environment, the rest go to the script
  local arg vars=() args=()
  for arg in "$@"; do
    case "$arg" in *=*) vars+=("$arg") ;; *) args+=("$arg") ;; esac
  done
  # A config file that does not exist: the real one must never reach a test.
  # /bin/bash, as launchd runs it: 3.2 on macOS rejects what Homebrew bash 5 accepts.
  env SELFRUNNER_REMOTE_CONFIG="$TMP/no-such.env" PHONE_GATEWAY_HOME="$TMP/gw" \
    SR_GW_HTTP="127.0.0.1:$PORT" SR_DAEMON_URL="http://127.0.0.1:$PORT" \
    SR_WATCHDOG_STATE="$TMP/state" SR_AUTORESIGN_STATE="$TMP/autoresign" \
    SR_NOTIFY_CMD="$TMP/notify" SR_NOTIFY_EXTRA=passed SR_WATCHDOG_CONFIRM_SECS=0 \
    PATH="$TMP/bin:$PATH" ${vars[@]+"${vars[@]}"} \
    /bin/bash "$ROOT/scripts/selfrunner-watchdog" ${args[@]+"${args[@]}"}
}
sent() { [ -f "$TMP/sent" ] && wc -l < "$TMP/sent" | tr -d ' ' || echo 0; }
last() { tail -n 1 "$TMP/sent"; }

phone online "$GOOD"
watchdog
[ "$(sent)" = 0 ] || bad "a healthy phone produced a notification"
[ "$(cat "$TMP/state/state")" = "ok 0 0" ] || bad "healthy state not recorded"
ok_t "a healthy phone is left alone"

phone online "$PASSCODE"
watchdog
[ "$(sent)" = 1 ] || bad "a stopped session was not reported"
case "$(last)" in needs_start\|*点\ Start*\|passed) ;; *) bad "unexpected notification: $(last)" ;; esac
ok_t "a stopped session asks for Start, with SR_NOTIFY_ settings passed through"

watchdog
[ "$(sent)" = 1 ] || bad "the same problem was reported twice"
read -r p s a < "$TMP/state/state"
printf '%s %s %s\n' "$p" "$s" "$((a - 7 * 3600))" > "$TMP/state/state"
watchdog
[ "$(sent)" = 2 ] || bad "no reminder after the reminder interval"
ok_t "the same problem is repeated only when the reminder is due"

phone online "$RELAUNCH"
watchdog
[ "$(sent)" = 3 ] || bad "a changed problem was not reported"
case "$(last)" in needs_relaunch\|*上滑关掉*) ;; *) bad "unexpected notification: $(last)" ;; esac
ok_t "a different problem is reported at once"

phone online "$GOOD"
watchdog
[ "$(sent)" = 4 ] || bad "recovery was not reported"
case "$(last)" in recovered\|*) ;; *) bad "unexpected notification: $(last)" ;; esac
watchdog
[ "$(sent)" = 4 ] || bad "recovery was reported twice"
ok_t "recovery is reported once"

phone online "$PASSCODE" "$GOOD"
watchdog SR_WATCHDOG_CONFIRM_SECS=1
[ "$(sent)" = 4 ] || bad "a problem gone on the second probe was reported"
[ "$(cat "$TMP/state/state")" = "ok 0 0" ] || bad "a passing blip was recorded"
ok_t "a problem must still be there on the confirming probe"

phone online "$PASSCODE"
printf '1\n' > "$TMP/notify_rc"
watchdog
[ "$(sent)" = 5 ] || bad "the failing notifier was not called"
rm -f "$TMP/notify_rc"
watchdog
[ "$(sent)" = 6 ] || bad "an undelivered notification was not retried"
watchdog
[ "$(sent)" = 6 ] || bad "a delivered notification was repeated"
ok_t "an undelivered notification is retried on the next run"

mkdir "$TMP/autoresign/lock.d"
phone online "$RELAUNCH"
watchdog
[ "$(sent)" = 6 ] || bad "probed while auto-resign was reinstalling"
rmdir "$TMP/autoresign/lock.d"
ok_t "nothing is probed while auto-resign reinstalls the app"

expect_event() {  # event, then the watchdog's extra env
  local event="$1"; shift
  rm -rf "$TMP/state" "$TMP/sent"
  watchdog "$@"
  case "$(last)" in "$event|"*) ;; *) bad "expected $event, got: $(last)" ;; esac
}
phone offline "$GOOD"; expect_event offline
phone online "$API_OFF"; expect_event runner_api_off
phone online null; expect_event unresponsive
phone online "$GOOD"; expect_event gateway_down SR_GW_HTTP="127.0.0.1:$CLOSED"
expect_event daemon_down SR_DAEMON_URL="http://127.0.0.1:$CLOSED"
ok_t "offline, Runner API off, no answer, gateway down and daemon down are told apart"

rm -rf "$TMP/state" "$TMP/sent"
phone online "$PASSCODE"
[ "$(watchdog --dry-run | tail -n 1)" = "将会通知：Self Runner 需要处理｜$(printf '%s' '自动化会话停了：请在手机上打开 Self Runner 点 Start，按提示输一次密码（远程做不到）。')" ] \
  || bad "dry run did not say what it would send"
[ "$(sent)" = 0 ] && [ ! -f "$TMP/state/state" ] || bad "dry run sent or recorded something"
ok_t "--dry-run reports without sending or recording"

rm -f "$TMP/sent"
watchdog --test-notify
case "$(last)" in test\|*\|passed) ;; *) bad "test notification not sent: $(last)" ;; esac
printf '1\n' > "$TMP/notify_rc"
if watchdog --test-notify; then bad "a failed test notification exited 0"; fi
rm -f "$TMP/notify_rc"
ok_t "--test-notify sends through the notifier and reports its failure"

printf '1..%d\n' "$pass"
