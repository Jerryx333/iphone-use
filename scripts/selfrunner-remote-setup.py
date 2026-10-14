#!/usr/bin/env python3
"""Self Runner 外网控制的 Mac 端部署：由一份本机参数文件生成 launchd plist 与 cloudflared 配置。

生成（或核对）这些文件，私人值全部来自参数文件，仓库里不留：

  <LaunchAgents>/<前缀>.phone-gateway.plist        phone-gateway serve（局域网 + WSS 入口）
  <LaunchAgents>/<前缀>.cloudflared-phone.plist    cloudflared 隧道常驻
  <LaunchAgents>/<前缀>.selfrunner-autoresign.plist 每 30 分钟检查签名并自动重签
  <cloudflared>/phone-gateway.yml                  隧道 ingress：<域名> → WSS 入口
  <autoresign 路径>                                 scripts/selfrunner-autoresign 的副本
  daemon plist 的环境变量                           外部托管模式（§14）：WDA/MJPEG 指向转发端口

用法：
  selfrunner-remote-setup.py check                 与现网逐项比较，一致返回 0
  selfrunner-remote-setup.py render --out DIR      只把生成结果写进 DIR（核对用）
  selfrunner-remote-setup.py apply [--dry-run] [--load]
      写入现网位置（原文件先备份为 .bak-时间戳）；--load 时重新加载有变化的 launchd 服务。

参数文件默认 ~/.config/iphone-use/selfrunner-remote.env（SELFRUNNER_REMOTE_CONFIG 可改），
只读 SR_ 开头的 KEY=VALUE 行，不执行；同名环境变量优先。字段与新机步骤见
docs/selfrunner-remote-setup.md。机密（隧道凭据、配对密钥、锁屏密码）不经过本脚本。
"""

import argparse
import datetime
import difflib
import ipaddress
import os
import pathlib
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile

REPO = pathlib.Path(__file__).resolve().parent.parent
HOME = pathlib.Path.home()
DEFAULT_CONFIG = HOME / ".config/iphone-use/selfrunner-remote.env"

REQUIRED = ("SR_LABEL_PREFIX", "SR_DEVICE_UDID", "SR_TEAM_ID", "SR_WS_HOSTNAME", "SR_TUNNEL_ID")
DEFAULTS = {
    "SR_BUNDLE_ID": "dev.iphoneuse.selfrunner.spike",
    "SR_GW_BIND": "0.0.0.0:7381",
    "SR_GW_HTTP": "127.0.0.1:7382",
    "SR_WS_BIND": "127.0.0.1:7383",
    "SR_RUNNER_FORWARD": "127.0.0.1:18100",
    "SR_MJPEG_FORWARD": "127.0.0.1:19100",
    "SR_GATEWAY_BIN": "~/.iphone-use/bin/phone-gateway",
    "SR_CLOUDFLARED_BIN": "",
    "SR_CLOUDFLARED_DIR": "~/.cloudflared",
    "SR_LAUNCH_AGENTS": "~/Library/LaunchAgents",
    "SR_LOG_DIR": "~/Library/Logs",
    "SR_AUTORESIGN_BIN": "~/.local/bin/selfrunner-autoresign",
    "SR_AUTORESIGN_STATE": "~/.iphone-use/selfrunner-autoresign",
    "SR_SRC": "~/.iphone-use/selfrunner-src",
    "SR_DAEMON_PLIST": "~/Library/LaunchAgents/com.leeguoo.iphone-use.plist",
}
LINE = re.compile(r"^\s*(SR_[A-Z0-9_]+)=(.*)$")


def die(message):
    sys.exit(f"selfrunner-remote-setup: {message}")


def expand(value):
    return str(HOME / value[2:]) if value.startswith("~/") else value


def load_config(path):
    values = {}
    if path.is_file():
        for raw in path.read_text(encoding="utf-8").splitlines():
            match = LINE.match(raw)
            if not match:
                continue
            value = match.group(2).strip()
            if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
                value = value[1:-1]
            values[match.group(1)] = value
    elif "SELFRUNNER_REMOTE_CONFIG" in os.environ:
        die(f"参数文件不存在：{path}")
    for key, value in os.environ.items():
        if key.startswith("SR_"):
            values[key] = value
    for key, value in DEFAULTS.items():
        values.setdefault(key, value)
    missing = [key for key in REQUIRED if not values.get(key)]
    if missing:
        die(f"缺少参数 {', '.join(missing)}（参数文件：{path}）")
    if not values["SR_CLOUDFLARED_BIN"]:
        values["SR_CLOUDFLARED_BIN"] = shutil.which("cloudflared") or "/opt/homebrew/bin/cloudflared"
    for key, value in values.items():
        values[key] = expand(value)
    validate(values)
    return values


def validate(v):
    checks = {
        "SR_LABEL_PREFIX": r"[A-Za-z0-9][A-Za-z0-9.-]*[A-Za-z0-9]",
        "SR_DEVICE_UDID": r"[0-9A-Fa-f-]{20,40}",
        "SR_TEAM_ID": r"[A-Z0-9]{10}",
        "SR_BUNDLE_ID": r"[A-Za-z0-9][A-Za-z0-9.-]*",
        "SR_WS_HOSTNAME": r"(?=.{1,253}$)([A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?\.)+[A-Za-z]{2,63}",
        "SR_TUNNEL_ID": r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
    }
    for key, pattern in checks.items():
        if not re.fullmatch(pattern, v[key]):
            die(f"{key} 格式不对")
    for key in ("SR_GW_BIND", "SR_GW_HTTP", "SR_WS_BIND", "SR_RUNNER_FORWARD", "SR_MJPEG_FORWARD"):
        host, sep, port = v[key].rpartition(":")
        try:
            ip = ipaddress.ip_address(host)
        except ValueError:
            ip = None
        if not sep or ip is None or not port.isdigit() or not 0 < int(port) < 65536:
            die(f"{key} 应为 IP:端口")
        if key != "SR_GW_BIND" and not ip.is_loopback:
            die(f"{key} 必须是回环地址")


def launch_agent(v, name):
    return pathlib.Path(v["SR_LAUNCH_AGENTS"]) / f"{v['SR_LABEL_PREFIX']}.{name}.plist"


def gateway_plist(v):
    log = f"{v['SR_LOG_DIR']}/phone-gateway.log"
    return {
        "Label": f"{v['SR_LABEL_PREFIX']}.phone-gateway",
        "ProgramArguments": [
            v["SR_GATEWAY_BIN"], "serve",
            "--bind", v["SR_GW_BIND"],
            "--http", v["SR_GW_HTTP"],
            "--runner-forward", v["SR_RUNNER_FORWARD"],
            "--mjpeg-forward", v["SR_MJPEG_FORWARD"],
            "--ws-bind", v["SR_WS_BIND"],
        ],
        "RunAtLoad": True,
        "KeepAlive": True,
        "ThrottleInterval": 10,
        "WorkingDirectory": str(HOME),
        "StandardOutPath": log,
        "StandardErrorPath": log,
    }


def cloudflared_yml_path(v):
    return pathlib.Path(v["SR_CLOUDFLARED_DIR"]) / "phone-gateway.yml"


def cloudflared_plist(v):
    log = f"{v['SR_LOG_DIR']}/cloudflared-phone.log"
    return {
        "Label": f"{v['SR_LABEL_PREFIX']}.cloudflared-phone",
        "ProgramArguments": [
            v["SR_CLOUDFLARED_BIN"], "tunnel", "--no-autoupdate",
            "--config", str(cloudflared_yml_path(v)), "run",
        ],
        "RunAtLoad": True,
        "KeepAlive": True,
        "ThrottleInterval": 10,
        "WorkingDirectory": str(HOME),
        "StandardOutPath": log,
        "StandardErrorPath": log,
    }


def autoresign_plist(v):
    state = v["SR_AUTORESIGN_STATE"]
    return {
        "Label": f"{v['SR_LABEL_PREFIX']}.selfrunner-autoresign",
        "ProgramArguments": [v["SR_AUTORESIGN_BIN"]],
        "RunAtLoad": True,
        "StartInterval": 1800,
        "StandardOutPath": f"{state}/launchd.out",
        "StandardErrorPath": f"{state}/launchd.err",
    }


def cloudflared_yml(v):
    tunnel = v["SR_TUNNEL_ID"]
    return (
        "# phone-gateway 专用（Self Runner 外网控制）；不要与其他隧道的 config.yml 混用。\n"
        f"tunnel: {tunnel}\n"
        f"credentials-file: {v['SR_CLOUDFLARED_DIR']}/{tunnel}.json\n"
        "no-autoupdate: true\n"
        "ingress:\n"
        f"  - hostname: {v['SR_WS_HOSTNAME']}\n"
        f"    service: http://{v['SR_WS_BIND']}\n"
        "  - service: http_status:404\n"
    )


def daemon_env(v):
    # §14 外部托管：daemon 不管 WDA 生命周期，经 gateway 的转发端口访问手机上的 runner。
    return {
        "IPHONE_USE_UDID": v["SR_DEVICE_UDID"],
        "IPHONE_USE_WDA_MANAGED": "false",
        "IPHONE_USE_WDA_URL": f"http://{v['SR_RUNNER_FORWARD']}",
        "IPHONE_USE_WDA_MJPEG_URL": f"http://{v['SR_MJPEG_FORWARD']}",
        "IPHONE_USE_SELFRUNNER_GATEWAY": f"http://{v['SR_GW_HTTP']}",
    }


def targets(v):
    """[(现网路径, 种类, 内容)]；daemon 只改环境变量的几个键。"""
    items = [
        (launch_agent(v, "phone-gateway"), "plist", gateway_plist(v)),
        (launch_agent(v, "cloudflared-phone"), "plist", cloudflared_plist(v)),
        (launch_agent(v, "selfrunner-autoresign"), "plist", autoresign_plist(v)),
        (cloudflared_yml_path(v), "text", cloudflared_yml(v)),
        (pathlib.Path(v["SR_AUTORESIGN_BIN"]), "script", (REPO / "scripts/selfrunner-autoresign").read_text()),
    ]
    daemon = pathlib.Path(v["SR_DAEMON_PLIST"])
    if daemon.is_file():
        with daemon.open("rb") as handle:
            current = plistlib.load(handle)
        merged = dict(current)
        merged["EnvironmentVariables"] = {**current.get("EnvironmentVariables", {}), **daemon_env(v)}
        items.append((daemon, "daemon", merged))
    return items


def serialize(kind, content):
    if kind in ("plist", "daemon"):
        return plistlib.dumps(content)
    return content.encode()


def read_live(path, kind):
    if not path.is_file():
        return None
    if kind in ("plist", "daemon"):
        with path.open("rb") as handle:
            return plistlib.load(handle)
    return path.read_text()


def yaml_body(text):
    return [line for line in text.splitlines() if line.strip() and not line.lstrip().startswith("#")]


def same(kind, live, wanted):
    if live is None:
        return False
    if kind == "text":
        return yaml_body(live) == yaml_body(wanted)
    return live == wanted


def describe(kind, live, wanted):
    """差异只打印键名和非密钥值；daemon 的环境变量只列被改的键。"""
    if live is None:
        return "  （现网没有此文件）"
    if kind == "daemon":
        env_live = live.get("EnvironmentVariables", {})
        return "\n".join(
            f"  {key}: {env_live.get(key, '（无）')} → {value}"
            for key, value in daemon_env_keys(wanted).items()
            if env_live.get(key) != value
        )
    if kind == "plist":
        lines = []
        for key in sorted(set(live) | set(wanted)):
            if live.get(key) != wanted.get(key):
                lines.append(f"  {key}: {live.get(key, '（无）')!r} → {wanted.get(key, '（无）')!r}")
        return "\n".join(lines)
    return "".join(
        "  " + line for line in difflib.unified_diff(
            live.splitlines(True), wanted.splitlines(True), "现网", "生成", n=1)
    )


def daemon_env_keys(merged):
    env = merged.get("EnvironmentVariables", {})
    return {key: env[key] for key in daemon_env_template() if key in env}


def daemon_env_template():
    return ("IPHONE_USE_UDID", "IPHONE_USE_WDA_MANAGED", "IPHONE_USE_WDA_URL",
            "IPHONE_USE_WDA_MJPEG_URL", "IPHONE_USE_SELFRUNNER_GATEWAY")


def cmd_check(v):
    differ = 0
    for path, kind, wanted in targets(v):
        live = read_live(path, kind)
        if same(kind, live, wanted):
            print(f"一致  {path}")
        else:
            differ += 1
            print(f"不同  {path}")
            detail = describe(kind, live, wanted)
            if detail:
                print(detail.rstrip())
    if not pathlib.Path(v["SR_DAEMON_PLIST"]).is_file():
        print(f"缺少  {v['SR_DAEMON_PLIST']}（先用 install.sh 装好 daemon）")
        differ += 1
    return 1 if differ else 0


def cmd_render(v, out):
    out = pathlib.Path(out)
    for path, kind, wanted in targets(v):
        dest = out / path.relative_to(path.anchor)
        if kind == "daemon":
            # daemon plist 含 IPHONE_USE_PASSWORD 等密钥，只写本脚本负责的几个键。
            dest = dest.with_name(f"{dest.stem}.selfrunner-env.plist")
            wanted = {"EnvironmentVariables": daemon_env(v)}
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_bytes(serialize(kind, wanted))
        if kind == "script":
            dest.chmod(0o755)
        print(dest)
    return 0


def launchctl(*args):
    return subprocess.run(["/bin/launchctl", *args], check=False).returncode


def cmd_apply(v, dry_run, load):
    stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    changed = []
    for path, kind, wanted in targets(v):
        live = read_live(path, kind)
        if same(kind, live, wanted) and not (kind == "text" and live != wanted):
            print(f"不变  {path}")
            continue
        print(f"{'将写' if dry_run else '写入'}  {path}")
        detail = describe(kind, live, wanted)
        if detail:
            print(detail.rstrip())
        changed.append((path, kind))
        if dry_run:
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        if path.exists():
            shutil.copy2(path, path.with_name(f"{path.name}.bak-{stamp}"))
        mode = 0o755 if kind == "script" else (0o600 if kind == "daemon" else 0o644)
        fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.")
        with os.fdopen(fd, "wb") as handle:
            handle.write(serialize(kind, wanted))
        os.chmod(tmp, mode)
        os.replace(tmp, path)
    state = pathlib.Path(v["SR_AUTORESIGN_STATE"])
    if not state.is_dir():
        print(f"{'将建' if dry_run else '建立'}  {state}/（launchd 日志目录）")
        if not dry_run:
            state.mkdir(parents=True)
    src = pathlib.Path(v["SR_SRC"])
    if not src.exists():
        print(f"{'将建' if dry_run else '建立'}  {src}（自动重签用的固定 worktree，detached HEAD）")
        if not dry_run:
            subprocess.run(["git", "-C", str(REPO), "worktree", "add", "--detach", str(src), "HEAD"], check=True)
    creds = pathlib.Path(v["SR_CLOUDFLARED_DIR"]) / f"{v['SR_TUNNEL_ID']}.json"
    if not creds.is_file():
        print(f"缺少  {creds}：从备份恢复隧道凭据（权限 600），或重新 cloudflared tunnel create")
    if load and not dry_run:
        uid = os.getuid()
        for path, kind in changed:
            if kind not in ("plist", "daemon"):
                continue
            label = plistlib.loads(path.read_bytes())["Label"]
            launchctl("bootout", f"gui/{uid}/{label}")
            if launchctl("bootstrap", f"gui/{uid}", str(path)) != 0:
                print(f"加载失败  {label}", file=sys.stderr)
    elif changed and not dry_run:
        print("未重新加载 launchd；需要时加 --load 再跑一次，或手动 launchctl bootout/bootstrap。")
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("check")
    render = sub.add_parser("render")
    render.add_argument("--out", required=True)
    apply = sub.add_parser("apply")
    apply.add_argument("--dry-run", action="store_true")
    apply.add_argument("--load", action="store_true")
    args = parser.parse_args()
    v = load_config(pathlib.Path(os.environ.get("SELFRUNNER_REMOTE_CONFIG") or DEFAULT_CONFIG).expanduser())
    if args.command == "check":
        return cmd_check(v)
    if args.command == "render":
        return cmd_render(v, args.out)
    return cmd_apply(v, args.dry_run, args.load)


if __name__ == "__main__":
    sys.exit(main())
