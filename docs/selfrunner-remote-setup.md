# Self Runner 外网控制：新 Mac / 新手机检查清单

本页把 Self Runner 远程控制（设计见 `design/self-runner-remote.md` §8、§13–15）在一台新 Mac 或一台新手机上从零装起来的步骤收在一处。仓库里只有通用脚本和本清单，所有私人值放在仓库外：

| 层 | 放哪里 | 内容 |
|---|---|---|
| 通用 | 本仓库 | `scripts/selfrunner-remote-setup.py`（生成 plist 与 cloudflared 配置）、`scripts/selfrunner-autoresign`（自动重签）、`scripts/selfrunner-watchdog`（掉线通知）、本清单 |
| 参数 | `~/.config/iphone-use/selfrunner-remote.env`（权限 600，自己另存一份私有备份） | 设备 UDID、Team ID、域名、隧道 UUID、launchd label 前缀等 |
| 机密 | 只在本机文件或钥匙串，另在密码管理器里备份 | 隧道凭据 json、手机锁屏密码 |

运行时只读本机文件和钥匙串，不依赖密码管理器解锁。gateway 的 PSK、token 和证书不需要备份：换 Mac 时重新 `pair` 并下发到手机即可。

## 参数文件

每行一个 `KEY=VALUE`，只认 `SR_` 开头的键；值可以加引号，`~/` 开头会展开；不支持行尾注释，也不会被当成 shell 执行。同名环境变量优先于文件。示例（全是占位值）：

```sh
# ~/.config/iphone-use/selfrunner-remote.env
SR_LABEL_PREFIX=com.example                 # launchd label 前缀，生成 com.example.phone-gateway 等
SR_DEVICE_UDID=00000000-0000000000000000    # xcrun devicectl list devices 的 Identifier
SR_HARDWARE_MODEL=D73AP                     # 手机 hardwareModel，见「新手机」第 3 步；不填按 iPhone 14 Pro
SR_TEAM_ID=ABCDE12345                       # Xcode 里 Apple ID 的 Team ID（免费账号也有）
SR_WS_HOSTNAME=phone.example.com            # Cloudflare 上指向隧道的域名
SR_TUNNEL_ID=00000000-0000-0000-0000-000000000000
SR_LAN_HOST=192.168.1.10                    # Mac 在家里局域网的地址，pair 时用
```

上面的 `#` 注释只为说明，真实文件里请把注释单独写一行。可选键和默认值：

| 键 | 默认值 |
|---|---|
| `SR_BUNDLE_ID` | `dev.iphoneuse.selfrunner.spike` |
| `SR_GW_BIND` / `SR_GW_HTTP` / `SR_WS_BIND` | `0.0.0.0:7381` / `127.0.0.1:7382` / `127.0.0.1:7383` |
| `SR_RUNNER_FORWARD` / `SR_MJPEG_FORWARD` | `127.0.0.1:18100` / `127.0.0.1:19100` |
| `SR_GATEWAY_BIN` | `~/.iphone-use/bin/phone-gateway` |
| `SR_CLOUDFLARED_BIN` | `PATH` 里的 `cloudflared`，否则 `/opt/homebrew/bin/cloudflared` |
| `SR_CLOUDFLARED_DIR` | `~/.cloudflared` |
| `SR_AUTORESIGN_BIN` / `SR_AUTORESIGN_STATE` | `~/.local/bin/selfrunner-autoresign` / `~/.iphone-use/selfrunner-autoresign` |
| `SR_WATCHDOG_BIN` / `SR_WATCHDOG_STATE` | `~/.local/bin/selfrunner-watchdog` / `~/.iphone-use/selfrunner-watchdog` |
| `SR_WATCHDOG_INTERVAL` | `1800`（秒，至少 300） |
| `SR_WATCHDOG_CONFIRM_SECS` / `SR_WATCHDOG_REMIND_HOURS` | `60` / `6`（见「掉线通知」） |
| `SR_NOTIFY_CMD` | 空：只发 Mac 本机通知（见「掉线通知」） |
| `SR_SRC` | `~/.iphone-use/selfrunner-src`（自动重签用的固定 worktree） |
| `SR_DAEMON_PLIST` / `SR_DAEMON_URL` | `~/Library/LaunchAgents/com.leeguoo.iphone-use.plist` / `http://127.0.0.1:8787` |

## 脚本会生成什么

`scripts/selfrunner-remote-setup.py` 有三个子命令：

- `check`：与现网逐项比较（plist 按内容比较，cloudflared 配置忽略注释），全部一致返回 0；
- `render --out DIR`：只把生成结果写进 `DIR`，用来人工核对；daemon 只写它负责的几个环境变量，不复制含密码的 plist；
- `apply [--dry-run] [--load]`：写入现网位置，原文件先备份为 `.bak-时间戳`；`--load` 时对有变化的 launchd 服务 bootout + bootstrap。

| 文件 | 内容 |
|---|---|
| `<前缀>.phone-gateway.plist` | `phone-gateway serve --bind … --http … --runner-forward … --mjpeg-forward … --ws-bind …`，常驻，日志 `~/Library/Logs/phone-gateway.log` |
| `<前缀>.cloudflared-phone.plist` | `cloudflared tunnel --no-autoupdate --config <cloudflared 目录>/phone-gateway.yml run`，常驻，日志 `~/Library/Logs/cloudflared-phone.log` |
| `<前缀>.selfrunner-autoresign.plist` | 每 30 分钟运行一次 `selfrunner-autoresign` |
| `<前缀>.selfrunner-watchdog.plist` | 每 `SR_WATCHDOG_INTERVAL` 秒（默认 30 分钟）运行一次 `selfrunner-watchdog` |
| `phone-gateway.yml` | 隧道 UUID、凭据路径，ingress `<SR_WS_HOSTNAME>` → `http://<SR_WS_BIND>`，其余 404 |
| `selfrunner-autoresign` | `scripts/selfrunner-autoresign` 的副本，运行时读同一份参数文件 |
| `selfrunner-watchdog` | `scripts/selfrunner-watchdog` 的副本，运行时读同一份参数文件 |
| daemon plist 的环境变量 | `IPHONE_USE_UDID`、`IPHONE_USE_RUNNER_MANAGED=false`、`IPHONE_USE_RUNNER_URL` / `IPHONE_USE_RUNNER_MJPEG_URL` 指向两个转发端口、`IPHONE_USE_SELFRUNNER_GATEWAY` 指向 gateway HTTP（§14）；其余键原样保留 |

另外：缺少 `SR_SRC` 时用当前仓库的 HEAD 建一个 detached worktree；缺少隧道凭据时只提示，不创建。`~/.cloudflared/config.yml` 和账号里的其他隧道不会被碰。

## 新 Mac

1. 装 Xcode 并在 Settings → Accounts 登录 Apple ID；装 `xcodegen`、`cloudflared`（`brew install xcodegen cloudflared`）、Python 3，以及 rustup 和 `aarch64-apple-ios` target。
2. clone 本仓库，按 README 用 `install.sh` 装好 daemon（会生成 `com.leeguoo.iphone-use.plist`）。
3. 构建 gateway 并放到固定位置：

   ```sh
   heavy_cargo=./scripts/heavy-cargo
   [ ! -x ~/.local/bin/heavy-cargo ] || heavy_cargo=~/.local/bin/heavy-cargo
   "$heavy_cargo" build --release -p phone-gateway
   install -m 755 target/release/phone-gateway ~/.iphone-use/bin/phone-gateway
   ```

4. 从私有备份恢复参数文件到 `~/.config/iphone-use/selfrunner-remote.env`，`chmod 600`。
5. 恢复隧道凭据到 `~/.cloudflared/<SR_TUNNEL_ID>.json`（`chmod 600`）。`cloudflared tunnel run` 只要这个 json，不需要 `cert.pem`。没有备份时重新建隧道：

   ```sh
   cloudflared tunnel login                       # 选域名所在的账号，得到 cert.pem
   cloudflared tunnel create <隧道名>              # 打印新 UUID，写出同名 json
   cloudflared tunnel route dns <隧道名> <SR_WS_HOSTNAME>
   ```

   然后把新 UUID 填进 `SR_TUNNEL_ID`。账号里如果已有别的隧道，`login` 会覆盖本机 `cert.pem`，先备份旧的。
6. 生成 gateway 配对：`~/.iphone-use/bin/phone-gateway pair --host <SR_LAN_HOST> --port 7381 --ws-url wss://<SR_WS_HOSTNAME>`。新 Mac 上的 PSK、证书和 `device_id` 都是新的，手机必须重新导入（见「新手机」第 6 步）。
7. 把手机锁屏密码存进登录钥匙串（交互输入，不写在命令行；account 是新的 `device_id`）：

   ```sh
   security add-generic-password -s phone-gateway.passcode -a "$(cat ~/.config/phone-gateway/device_id)" -w
   ```

8. 先预览再写入，然后核对：

   ```sh
   python3 scripts/selfrunner-remote-setup.py apply --dry-run
   python3 scripts/selfrunner-remote-setup.py apply --load
   python3 scripts/selfrunner-remote-setup.py check
   ```

9. 手机端按下一节走完后，验收（见文末「验收」）。

## 新手机（或换了 Mac 的老手机）

1. 设置 → 隐私与安全性 → 开发者模式，打开并重启；插线后在手机上信任这台 Mac。
2. App Store 安装 LocalDevVPN，打开 VPN 并保持开启（不能同时开别的 VPN）。
3. 换了机型时，读出 hardwareModel 写进 `SR_HARDWARE_MODEL`，并删掉旧机型的 DDI 让它重新抽取：

   ```sh
   xcrun devicectl device info details --device <UDID> --json-output /tmp/d.json >/dev/null
   python3 -c 'import json; print(json.load(open("/tmp/d.json"))["result"]["hardwareProperties"]["hardwareModel"])'
   rm -rf ~/.iphone-use/selfrunner-src/spikes/self-runner-app/Resources/DDI
   ```

4. 把 `SR_DEVICE_UDID` 改成新手机，重新 `apply`（daemon 的 `IPHONE_USE_UDID` 跟着改）。
5. 首次签名安装和 RP 配对（插线，手机解锁；会提示在手机上点「信任」并输入密码）：

   ```sh
   UDID=<UDID> DEVELOPMENT_TEAM=<SR_TEAM_ID> spikes/self-runner-app/install.sh
   ```

   它会构建、安装、在缺少时用 `self-boot-probe rp-pair` 生成 `rp_pairing.plist` 并推进 App 的 Documents。配对文件含设备访问密钥，不要提交或分享。
6. 下发 gateway 配对并在 App 里点「重新配对」：

   ```sh
   xcrun devicectl device copy to --device <UDID> --domain-type appDataContainer \
     --domain-identifier <SR_BUNDLE_ID> --source ~/.config/phone-gateway/gateway-pair.json \
     --destination Documents/gateway-pair.json
   ```

   App 导入后存进钥匙串并删除文件；没有合法新文件时点「重新配对」不会清掉现有配对。
7. 设置 → 显示与亮度 → 自动锁定，设为「永不」；如需后台保活，定位权限设为「始终」。自动锁定 2 分钟时出现过：自动重签重装 App 后自动化会话没起来，要人到手机上点 Start。
8. 让自动重签接手：`selfrunner-autoresign --force`。它用 `--remote-on --runner-core-on` 等预设参数启动 App，等自动化会话就绪；首次会通知你在手机上输入一次密码。之后签名剩余不到 48 小时、手机在家里局域网且 LocalDevVPN 开着时自动重签重装。
9. 直接调试手机上的 runner API 时，用 `scripts/selfrunner-runner-token.py` 从 gateway 的 PSK 算出请求签名 token。

## 掉线通知

`selfrunner-watchdog` 定时检查手机还能不能被远程控制，只读：不解锁、不重启、不改手机状态。锁屏不算故障，agent 需要时 `phone_reconnect` 会远程解锁。下面这些情况要人处理，会发通知：

| 事件代码 | 情况 | 通知里让人做的事 |
|---|---|---|
| `gateway_down` / `daemon_down` | Mac 上的 phone-gateway 或 iphone-use daemon 没响应 | 检查 Mac 上对应的服务 |
| `offline` | 手机没连上 gateway | 打开 Self Runner，确认 LocalDevVPN 已开 |
| `unresponsive` | 连着但不回应状态查询 | 打开 Self Runner；不行就上滑关掉再打开 |
| `needs_start` | 自动化会话停了（例如重装后要密码） | 打开 Self Runner 点 Start，按提示输一次密码 |
| `needs_relaunch` | 进程要彻底重启 | 上滑关掉 Self Runner，重新打开后点 Start |
| `runner_api_off` | Runner API 没开 | 在 Self Runner 里打开「Runner API 8100/9100」 |

规则：第一次查到问题后隔 `SR_WATCHDOG_CONFIRM_SECS` 秒再查一次，两次都有问题才通知；同一个问题每 `SR_WATCHDOG_REMIND_HOURS` 小时最多提醒一次（0 表示不重复提醒），问题变了马上通知；恢复时发一次「已恢复」（事件代码 `recovered`）。自动重签正在重装时这一轮跳过，结果由自动重签自己通知。状态在 `SR_WATCHDOG_STATE/state`，日志在同目录的 `watchdog.log`。

通知默认只弹 Mac 本机通知。要推到手机，把 `SR_NOTIFY_CMD` 设成一个可执行文件的路径（不带参数；需要参数就写个包装脚本）。它会收到两个参数：标题和正文；环境变量 `SR_NOTIFY_EVENT` 是上表的事件代码（自动重签的通知为 `autoresign`），参数文件里其他 `SR_NOTIFY_` 开头的键也原样传给它，可放推送服务的地址、收件人等。退出码非 0 表示没送到，下次检查重发；超过 30 秒会被结束。自动重签的通知也走同一个命令。推送服务的 token 不要写进参数文件，让通知命令自己从权限 600 的文件或钥匙串里读。

手动检查：`selfrunner-watchdog --dry-run` 打印事件代码和将要发的通知，不发、不记录；`--test-notify` 经同一条路径发一条测试通知，送不到时退出码非 0。改了脚本后跑 `bash scripts/test-selfrunner-watchdog.sh`（假 gateway，不碰真手机）。

## 验收

1. `~/.iphone-use/bin/phone-gateway cmd status`：`transport` 为 `online`，在家 Wi‑Fi 下 `link` 为 `lan`；
2. 手机关掉 Wi‑Fi 只用蜂窝（LocalDevVPN 保持开）：`link` 变为 `wss`，`cmd read` 正常；
3. daemon 的 `phone_status` 为 `drivable:true`，`phone_run_steps` 能执行；
4. `python3 scripts/selfrunner-remote-setup.py check` 全部一致；`selfrunner-autoresign --dry-run --force` 只打印计划、不动描述文件；
5. `selfrunner-watchdog --dry-run` 打印 `ok`；`selfrunner-watchdog --test-notify` 发一条测试通知（事件代码 `test`），确认手机收得到。

## 已知限制

- 重签重装要用 devicectl，只能在家里局域网或插线时做；在外只能控制、不能重装，48 小时窗口内回家一次即可。
- 手机重启后要人解锁、开 VPN、点开 App。
- 自动重签重装 App 后，自动化会话有时要人在手机上点 Start 并输一次密码，远程做不到；`selfrunner-watchdog` 会通知。
- 在外（`link=wss`）时所有字节走 Cloudflare 免费套餐，不要长时间开 MJPEG 或录屏（§15.9）。

## 用 Self Runner 当托管模式的运行器

先按本文完成 Self Runner 安装及 phone-gateway 配对，再在 Mac 上运行：

```bash
IPU_RUNNER_HOST=selfrunner iphone-use setup-native
# 自定义 App 标识时同时指定：
# IPU_SELFRUNNER_BUNDLE_ID=dev.example.selfrunner
```

保留托管模式（`IPHONE_USE_RUNNER_MANAGED=true`，回环端点默认开启）。setup 会持久化宿主选择，拉起已有的 Self Runner，并将手机 8100/9100 中继到 Mac 回环端口；它不会构建、安装、卸载任何 App，也不会轮换运行器令牌或续签。`prebuild` 在此模式下直接成功退出。免费 Apple ID 的设备自签 App 名额只需 **1 个**；如果之前安装过独立运行器，需自行移除，setup 不会代为卸载。

运行器令牌由 Mac 的 `$PHONE_GATEWAY_HOME/psk`（默认 `~/.config/phone-gateway/psk`，base64 编码的 32 字节配对 PSK）派生，Mac 必须与手机使用同一配对。令牌只写入实例状态目录的 `runner-token`（0600），不要复制到命令行或日志。自定义 `PHONE_GATEWAY_HOME` 会随 supervisor 持久化。

**手机重启后仍要人操作**：在手机上打开 Self Runner 点 Start，按提示输一次密码。devicectl 只能打开 App，不能替人点 Start 或授权；不会使用 terminate-existing 打断已有会话。Self Runner 的签名有效期、安装和重签由它自己的脚本管理；此模式不会自动修复过期签名，也不能保证 App 被系统终止后完全无人值守恢复。USB / CoreDevice 隧道、开发者服务、解锁等前置条件仍适用；远程非托管模式不受影响。

`status` / `doctor` 显示宿主模式。`selfrunner_pairing` 表示 PSK 缺失、无效或长度不是 32 字节；`selfrunner_token_mismatch` 表示签名请求被 401 拒绝，需要核对配对；`selfrunner_not_started` 表示等待就绪超时，需要在手机上点 Start。守护进程只监测 HTTP 与中继，失败后重新打开 App 和等待就绪，不重建产物。
