#!/usr/bin/env bash
# Call with bash or zsh; notification credentials belong to the caller.
set -eu
usage() {
    cat <<'EOF'
Usage: PUSH_CMD='<notification shell command>' [GATEWAY=path] [ROUNDS=3] bash scripts/research-banner-occlusion.sh
       bash scripts/research-banner-occlusion.sh --push-cmd '<command>' [--gateway path] [--rounds N]
Requires a paired, running phone-gateway and an unlocked phone with banners enabled.
PUSH_CMD is executed by bash; this script contains no notification account or key.
EOF
}
PUSH_CMD=${PUSH_CMD:-}
GATEWAY=${GATEWAY:-target/release/phone-gateway}
ROUNDS=${ROUNDS:-3}
while [ "$#" -gt 0 ]; do
    case "$1" in
        --push-cmd|--gateway|--rounds)
            [ "$#" -ge 2 ] || { usage >&2; exit 64; }
            case "$1" in
                --push-cmd) PUSH_CMD=$2 ;;
                --gateway) GATEWAY=$2 ;;
                --rounds) ROUNDS=$2 ;;
            esac
            shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; exit 64 ;;
    esac
done
[ -n "$PUSH_CMD" ] || { usage >&2; exit 64; }
export PUSH_CMD GATEWAY ROUNDS
python3 -I - <<'PY'
import json
import math
import os
import subprocess
import sys
import time

try:
    rounds = int(os.environ['ROUNDS'])
    if rounds < 1:
        raise ValueError()
except ValueError:
    print('ROUNDS must be a positive integer', file=sys.stderr)
    sys.exit(64)


def command(action, args, timeout=30):
    try:
        response = subprocess.run(
            [os.environ['GATEWAY'], 'cmd', action, '--args', json.dumps(args),
             '--deadline-ms', str(max(1, int(timeout * 1000)))],
            capture_output=True, text=True, timeout=timeout)
        if response.stderr:
            print(response.stderr.rstrip(), file=sys.stderr)
        payload = json.loads(response.stdout)
        if not isinstance(payload, dict):
            raise ValueError('response is not an object')
        return payload
    except (OSError, ValueError, subprocess.TimeoutExpired) as error:
        print(f'{action}: {type(error).__name__}', file=sys.stderr)
        return {'ok': False, 'error': {'code': 'command_failed'}}


def code(response):
    return 'ok' if response.get('ok') is True else (response.get('error') or {}).get('code', 'unknown')


def nodes(response):
    return (response.get('result') or {}).get('nodes', [])


def frame(node):
    f = node.get('frame') or {}
    values = [f.get(k) for k in ('x', 'y', 'w', 'h')]
    if any(isinstance(v, bool) or not isinstance(v, (int, float)) or not math.isfinite(v) for v in values):
        return None
    return values


def screen_size(response):
    # AX root is the screen-sized application frame, in points (not PNG pixels).
    for node in nodes(response):
        f = frame(node)
        if node.get('depth') == 0 and f and f[0] == 0 and f[1] == 0 and f[2] > 0 and f[3] > 0:
            return f[2], f[3]
    return None


def banner_seen(response, width, height):
    return response.get('ok') is True and any(
        n.get('label') == '通知' and f and f[2] > 0 and f[3] > 0
        and 0 <= f[0] < width and f[1] < height / 3 and f[1] + f[3] > 0
        for n in nodes(response) for f in [frame(n)])


def wait_banner_gone():
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        system = command('read', {'target': 'system', 'max_depth': 100},
                         timeout=max(0.001, deadline - time.monotonic()))
        if system.get('ok') is True and not any(n.get('label') == '通知' for n in nodes(system)):
            return True
        time.sleep(min(0.2, max(0, deadline - time.monotonic())))
    return False


rows = []
for round_number in range(1, rounds + 1):
    banner = 'not_checked'
    if round_number > 1 and not wait_banner_gone():
        response = {'ok': False, 'error': {'code': 'banner_clear_timeout'}}
    else:
        response = command('launch', {'bundle_id': 'com.apple.Preferences'})
    if response.get('ok') is True:
        response = command('tap', {'label': '通用', 'index': 0, 'expect_label': '关于本机'})
    if response.get('ok') is True:
        tree = command('read', {})
        size = screen_size(tree) if tree.get('ok') is True else None
        if size is None:
            response = {'ok': False, 'error': {'code': 'screen_size_unknown'}}
        else:
            pushed = subprocess.run(['bash', '-c', os.environ['PUSH_CMD']],
                                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            if pushed.returncode:
                response = {'ok': False, 'error': {'code': 'push_failed'}}
            else:
                deadline = time.monotonic() + 10
                banner = 'banner_not_seen'
                while time.monotonic() < deadline:
                    system = command('read', {'target': 'system', 'max_depth': 100},
                                     timeout=max(0.001, deadline - time.monotonic()))
                    if banner_seen(system, *size):
                        banner = 'seen'
                        break
                    time.sleep(min(0.2, max(0, deadline - time.monotonic())))
                response = command('tap', {'identifier': 'BackButton', 'expect_label': '通用'})
    occlusion = (response.get('result') or {}).get('occlusion') or {}
    if response.get('ok') is not True:
        outcome = 'fail'
    elif banner != 'seen' or not occlusion:
        outcome = 'not_exercised'
    elif occlusion.get('cleared_by') not in (None, 'none'):
        outcome = 'pass'
    else:
        outcome = 'fail'
    rows.append((round_number, response.get('ok') is True, code(response),
                 response.get('ms', '-'), occlusion.get('cleared_by', '-'), banner, outcome))
    print(f'round {round_number}: ok={rows[-1][1]} code={rows[-1][2]} banner={banner} outcome={outcome}',
          file=sys.stderr, flush=True)

print('round\tok\tcode\tms\tocclusion.cleared_by\tbanner\toutcome')
for row in rows:
    print('\t'.join(str(v).lower() if isinstance(v, bool) else str(v) for v in row))
counts = {outcome: sum(row[6] == outcome for row in rows)
          for outcome in ('pass', 'not_exercised', 'fail')}
print('summary: ' + ' '.join(f'{outcome}={count}' for outcome, count in counts.items()))
sys.exit(1 if counts['fail'] else 2 if counts['not_exercised'] == rounds else 0)
PY
