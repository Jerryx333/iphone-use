# Self-runner: the Mac only for setup

Status: exploration on branch `explore/self-runner` (based on `upstream/feat/on-phone-daemon`).

## Goal

Today the device runner lives exactly as long as the testmanagerd session that the Mac
(`xcodebuild test-without-building`, or `iphone-use-legacy-launch`) holds for it. The
`feat/on-phone-daemon` branch already runs the whole daemon inside the runner
(`libipu_embed.a`, `127.0.0.1:8787`), so a task asked on the phone has no Mac in its request
path, but the runner still cannot start, restart or survive without the Mac.

Target: the Mac is needed only to

1. build, sign and install the app once (and again when the signing certificate expires), and
2. export the pairing files to the phone once.

Everything else (starting UI automation after a reboot or idle release, mounting the DDI,
serving the agent API, running tasks and flows) happens on the phone.

## How (the self-runner pattern)

Prior art: willfaust/TouchSynthesis (no licence: read for protocol facts only, copy no code),
sorrycc/Fingertip (MIT, built on TouchSynthesis), StephenDev0/StikDebug (AGPL: facts only).
Library we may use: jkcoxson/idevice (MIT; already a dependency through `crates/legacy-launch`).

```
iPhone
  LocalDevVPN (App Store)  10.7.0.0 <-> 10.7.0.1 loopback
  host app (one process plays both sides)
    Rust boot lib (idevice): RPPairing tunnel -> RSD -> CoreDeviceProxy
        heartbeat; DDI status / personalized mount (TSS)
        testmanagerd IDE side: control session + test session (DTX), authorize our own pid
    ObjC bootstrap: setenv XCTestSessionIdentifier=<session uuid>, dlopen XCTest,
        XCTRunnerDaemonSession handshake, enableAutomationModeWithError: (passcode prompt)
    runner core (IPURBridge, RunnerHTTP, RunnerWDA, ...) + libipu_embed.a daemon on :8787
Mac (once)
  build / sign / install; create RPPairing + lockdown pair records; copy them into the app
```

Why one process: on iOS 26+ AMFI strips the environment of processes launched through
AppService, so a separately spawned runner never sees `XCTestSessionIdentifier`.

## Stages

| Stage | What | Verify |
|---|---|---|
| S0a | `crates/self-boot`: idevice-based boot library with a C ABI (tunnel, heartbeat, testmanagerd IDE sessions, DDI status) and a Mac probe CLI that runs the same calls over USB | `cargo test -p self-boot`; `cargo build -p self-boot --bin self-boot-probe`; probe against a real phone opens both testmanagerd sessions without launching anything |
| S0b | `spikes/self-runner-app`: minimal host app linking S0a, ObjC bootstrap, Start button, log view; after automation is on, one AX snapshot and one tap through `IPURBridge` | `xcodebuild -sdk iphoneos CODE_SIGNING_ALLOWED=NO build` succeeds (needs rustup + `aarch64-apple-ios`) |
| S0c | On the phone, person present | setup time, snapshot latency, survives 30 min in background / app switches / VPN reconnect; result written below |
| S1 | DDI mount from the phone (no Xcode after a reboot) | reboot -> LocalDevVPN -> app -> automation, Mac off |
| S2 | Runner core shared between the XCTest bundle and the host app; embedded daemon served by the host app | existing runner checks still pass; `/status` from the host app |

S0c decides whether S1/S2 are worth doing.

## Hard constraints (known before starting)

- One VPN slot: LocalDevVPN must stay on while the session lives (DTX goes through it), so no
  Tailscale/WARP/proxy VPN on the phone at the same time. Remote access must be outbound from the app.
- Free Apple ID signatures expire after 7 days -> a Mac is needed weekly; a paid account makes it yearly.
- Every automation start asks for the passcode (same as today); a reboot needs a person.
- Background survival relies on background location ("Always"); iOS may still kill the app.
- Verified upstream only on iOS 26.x; iOS 26.4 already changed loopback lockdownd (hence RPPairing).
  iOS 27 is unknown. iOS 15/16 are out of scope.
- The pairing files on the phone grant full device access over the network.

## Results

(fill in per stage)

### S0a — self-boot library and host probe

Implemented `crates/self-boot` (`rlib` + `staticlib`), the blocking header
`crates/self-boot/include/self_boot.h`, and `self-boot-probe`. No device was
connected or operated during this stage; protocol acceptance on a real phone
remains for the main session to verify.

The unchanged idevice patch is rev `41c388baef0dd55c0ce4924789abbfde889d9b87`.
Direct features: `ring`, `remote_pairing` (the actual name, not `rp_pairing`),
`rsd`, `core_device_proxy`, `tunnel_tcp_stack`, `xctest`, `heartbeat`,
`mobile_image_mounter`, `usbmuxd`. `xctest` also enables its upstream transitive
features, including installation helpers, but this crate never invokes them.

- RP path: `RpPairingFile::read_from_file`, `RpPairingSocket`,
  `RemotePairingClient::{attempt_pair_verify, validate_pairing, create_tcp_listener}`,
  `connect_tls_psk_tunnel_native`, then `Adapter::new` / `to_async_handle` and
  `RsdHandshake::new`. The record is verified only; no PIN fallback or record
  rewrite. The RP control connection stays owned by `Session`.
- USB path: idevice's environment-aware `UsbmuxdAddr`, USB-only UDID selection,
  `CoreDeviceProxy::connect` / `create_software_tunnel`, then the same adapter/RSD
  session. There is no hardcoded macOS socket path in this crate.
- Status: `ImageMounter::connect_rsd`, `copy_devices`,
  `query_developer_mode_status`. Reports raw image entries too. Mounted status
  recognizes Developer/Personalized `ImageType` or `/Developer` / `/System/Developer`
  `MountPath`; the device's actual response shape needs probe confirmation.
  No upload, mount or personalization/TSS operation is performed.
- Keepalive: `HeartbeatClient::connect_rsd`, `get_marco` / `send_polo` in an
  owned background task. `Session::state`, `watch_state` and `subscribe` expose
  disconnect reasons; DTX EOF/errors update the same state. Heartbeat service
  absence is an explicit connection error, not silently degraded keepalive.

Differences from the plan: this rev's `RemoteServerClient` capability handshake,
correlated calls, incoming-channel registration, header accessors and XCTest
archive helpers are `pub(crate)`. Rather than patching the dependency, self-boot
uses idevice's public `Message`/`Aux`/header framing and decoder with its own
single-reader DTX dispatcher. Header metadata is read via public serialization.
NSUUID and typed XCTCapabilities archives are constructed locally, using MIT
idevice as the protocol reference; no TouchSynthesis or StikDebug source was used.
Each independent connection publishes DTX capabilities, waits for the peer hello,
opens `dtxproxy:XCTestManager_IDEInterface:XCTestManager_DaemonConnectionInterface`,
and calls `_IDE_initiateControlSessionWithCapabilities:` or
`_IDE_initiateSessionWithIdentifier:capabilities:`. Authorization calls
`_IDE_authorizeTestSessionWithProcessID:` on control. Replies and incoming messages
are observable, including reverse channels. RPC/connection waits are bounded.

Scope not implemented: callback-specific XCTest bootstrap replies (configuration,
runner readiness negotiation) and executing a test plan belong to S0b or later.
S0a records unsolicited callbacks and sends empty DTX acknowledgements when
requested; it does not claim to host a complete XCTest IDE implementation. It
never launches a process. A `TestSessions` must be dropped before its owning
`Session` is closed. C callers serialize operations on a handle; each handle owns
a multi-thread Tokio runtime, so heartbeat continues between blocking calls.
C errors are borrowed thread-local UTF-8 strings; see header lifetime rules.

Host validation: 10 offline tests passed (archives, frame/reply correlation,
callback interleaving and EOF detection via an in-memory daemon, image
classification, argument/error/ABI checks). `cargo build -p self-boot`,
`cargo build -p self-boot --bins`, `cargo clippy -p self-boot -- -D warnings`
and `./target/debug/self-boot-probe --help` passed on aarch64-apple-darwin.
This sandbox could not resolve crates.io or unpack cached packages into
`~/.cargo`; checks used `--offline` and a writable temporary
`CARGO_HOME=/private/tmp/self-boot-cargo-home` reusing the local caches, with
`CARGO_BUILD_JOBS=2`. No iOS target build was attempted.

Main-session hardware command (not run here):
`./target/debug/self-boot-probe --udid <UDID> [--authorize-pid N]`.
It emits JSON lines for connection, DDI status, both initiation replies, optional
PID authorization, incoming messages, ten seconds of observation, and disconnect.


#### S0a 返工 — 2026-10-11（离线修正完成，真机验收受阻）

**验收状态：未完成。** 原心跳失败的底层根因尚未确认，不能把本次状态修正
当成心跳已修复。当前执行环境对 USB/usbmuxd socket 返回 EPERM，无法取得首个
Marco 或复现原服务失败；没有成功的 60 秒 Connected 输出，也没有真实回复的
JSON 样本。没有运行授权 PID、启动进程、测试计划或其它手机操作命令。

源码依据（均为本机缓存的 idevice rev
`41c388baef0dd55c0ce4924789abbfde889d9b87`，上游项目
<https://github.com/jkcoxson/idevice>）：

- `idevice/src/services/heartbeat.rs:125-134` 明确实现 RSD 服务
  `com.apple.mobile.heartbeat.shim.remote`，连接后先 `rsd_checkin`。
  `idevice/src/lib.rs:291-333` check-in 依次读 RSDCheckin 和 StartService 回复。
  因此没有证据支持删除 RSD 心跳，或改用未经验证的 RemotePairing 保活。
- `heartbeat.rs:62-104` 的 `get_marco` 使用调用者给定的秒数等待；立即出现
  socket I/O 错误不是等待 30 秒后触发的 HeartbeatTimeout。
  `tools/src/heartbeat_client.rs:12-19` 使用 connect、等待 Marco、Polo、
  下一轮 Interval+5；`tests/src/heartbeat.rs:32-36` 首次等待 30 秒。
  本库保留同一协议，新增成功的 `keepalive` 事件，包含服务返回的 interval。
- 原 `probe1.jsonl` 不仅有 heartbeat 错误：两条 initiate 成功以后，control
  和 test 也出现 socket 错误，最终 close 报 `adapter closed`。
  它证明心跳不能单独决定 DTX 已断开，但不能证明整个观察期隧道可用。
  `jktcp-0.1.7/src/handle.rs:195-202` 的隧道读错误会传播到全部 streams 并退出；
  `handle.rs:228-243` 的单条连接 FIN/RST 则只移除对应 stream。
  原输出把 IdeviceError 的底层 socket 原因隐藏成统一 Display 文本，无法据此
  判定是服务 FIN/RST、隧道读失败还是其它 I/O 原因。下一次允许的真机复测
  应使用本次保留的 Debug 错误信息判断，不能声称上述任一假设已证实。

本次范围内修正：

- `ConnectionState::KeepaliveDegraded(reason)` 与 `Disconnected(reason)` 分开。
  心跳连接失败、超时或 Marco/Polo 失败显式报告降级；不再写 disconnected
  事件，也不会覆盖 Disconnected/Closed。心跳建连有 30 秒 deadline。
  DTX 自身读写错误仍报告 disconnected；降级没有被当作心跳成功。
  注意：Disconnected 目前代表 DTX 不可用，不是单独证明整个隧道断开。
- 探针新增 `--observe-secs N`，默认 10，拒绝 0，并记录实际观察时长。
  顶层错误和保活错误保留 Debug 信息，以免再次丢失底层原因。
- initiate 的 `control_reply` / `test_reply` 从 Debug 字符串改为 JSON 对象。
  从 Message.raw_data 重新读取完整 NSKeyedArchive，递归解析 UID、
  自定义 XCTCapabilities 的 capabilities-dictionary 和 NSDictionary 键值。
  上游 `ns-keyed-archive-0.1.5/src/decode.rs:154` 对未知类只返回对象本身，
  这正是旧输出中 capabilities-dictionary 仍为 Uid(?) 的原因。
  本地解析拒绝越界 UID、循环引用和不匹配的字典键值数量。

验证（heavy-cargo 不存在，按仓库规定回退 cargo；CARGO_BUILD_JOBS=2）：

```text
cargo test -p self-boot --offline: 12 library tests + 1 CLI test passed
cargo clippy -p self-boot --offline -- -D warnings: passed
PATH=/opt/homebrew/opt/rustup/bin:$PATH IPHONEOS_DEPLOYMENT_TARGET=17.0 CARGO_BUILD_JOBS=2 cargo build -p self-boot --release --target aarch64-apple-ios --lib --offline: passed
```

新增离线测试覆盖保活降级不覆盖断连/关闭、capabilities JSON 键值、非法和循环
UID；现有内存 DTX 测试继续验证回调交错、回复关联和真实 EOF。

本次确实执行了以下获准的真机探针命令（未带 --authorize-pid）：

```sh
./target/debug/self-boot-probe --udid <UDID> --observe-secs 60
```

输出保存于 `/private/tmp/self-boot-rework-60.jsonl`，退出码 1，唯一关键行：

```json
{"detail":"Device(Socket(Os { code: 1, kind: PermissionDenied, message: \"Operation not permitted\" }))","error":"device socket io failed","step":"error"}
```

该失败发生在 connect_usb 返回之前，尚未开始 60 秒观察，不能计作真机验收通过。
当前沙箱策略禁止提权，需在有 USB socket 访问权限的主会话继续同一获准命令；
原心跳根因修复及成功输出的补录仍待完成。

#### S0a — main-session hardware check (2026-10-11, iPhone 14 Pro, iOS 27.0.1, USB)

`self-boot-probe --udid … --observe-secs 60`, exit 0:

- `connect_usb` 78 ms, 85 RSD services; DDI: personalized Xcode 27 DDI (cryptex) mounted at
  `/System/Developer`, developer mode on.
- Both IDE sessions initiated within ~20 ms; testmanagerd's capabilities include
  `"automation mode": 1` and `"authorize test session capability": 1`. Both DTX connections
  stayed up for the full 60 s (no `disconnected` event).
- Heartbeat shim (`com.apple.mobile.heartbeat.shim.remote`) is closed by the device right after
  check-in (`BrokenPipe: channel closed`) -> state `KeepaliveDegraded`. Unverified guess: the
  Mac's own CoreDevice already holds this device's heartbeat over USB. Re-check on the phone's
  RPPairing path in S0b, where Fingertip reports the RSD heartbeat working.

### S0b — host app scaffold + RP pairing CLI (2026-10-11)

结构：`spikes/self-runner-app` 包含 xcodegen SwiftUI App（iOS 17.0）、独立
`SpikeBridge` ObjC runtime bridge、Rust iOS staticlib 构建脚本与显式真机安装脚本。
Bundle ID `dev.iphoneuse.selfrunner.spike`；team 由安装环境传入；无 VPN entitlement。
Documents 放 RP record 与 selfrunner.log，位置保活默认开/自身 PID 授权默认关。
生成项目和全部 Xcode 输出、配对文件保存在/忽略于 spike build 目录（生成工程独立忽略）。

self-boot probe 现在是显式 `probe` / `rp-pair` / `rp-connect` 三个子命令。
配对协议参考现有 idevice rev tools/src/rppairing.rs，USB proxy → RSD → untrusted
RemoteXPC → RemotePairingClient::connect；交互 PIN 检查六位 ASCII 数字，180 秒
总 deadline；新 record 0600 create_new，不覆盖已有文件。RP connect 复用原探针
DDI/status/两条 IDE sessions/observe/close 流程，不做配对 fallback。
C ABI 增加带 deadline 的 connect、borrowed JSON status+events、initiate/authorize
reply JSON；后台 runtime 保持运行，宿主串行调用。

与方案差异：IPURBridge 直接依赖 XCTest 和 runner 辅助头文件；spike 用自写最小
runtime 桥接（不修改 runner），AX 单次深度 24/2000 节点统计，不做深树补全与
结构化树输出；截图只用 daemon PNG request；触摸仅 UIKit 靶按钮实际坐标，竖屏，
目标限定自身 PID，需前台可见无遮挡。没有读/复制 TouchSynthesis/StikDebug 源码。

本机 Xcode 26.6 的 `otool -ov` 确认
`+[XCTRunnerDaemonSession initiateSharedSessionWithCompletion:]` 和
`-[XCTRunnerDaemonSession enableAutomationModeWithError:]` 的方法与类型编码。
宿主设置要求的三个 XCTest 环境变量后 dlopen DDI dependencies，异步等待 daemon
session（90 秒 deadline/每十秒日志），可选授权 PID，再仅调用 enableAutomationMode。
不调用 _XCTestMain、finishInitializationForUIAutomation、requestAutomationSession。
没有证据需要其它环境变量，未设置额外 test bundle/configuration 路径。

未决：iOS 27 的 shared-session callback 是否还需 S0a 未实现的 IDE callback 回复；
self-RP 路径心跳能否成功；是否必须授权自身 PID；DDI dlopen/AMFI 在免费签名下是否
允许；多前台时 AX 应用选择；私有 synchronous AX/enable/synthesis 的等待无法可靠
取消；Stop 后进程全局 XCTest sharedSession 是否允许重连（失败需重启 App）；定位
Always 权限与系统调度不能保证后台不被杀。编译验收不能代表这些协议已经通过。

主会话真机验证步骤（本单未执行任何设备命令）：

1. Xcode 挂载 personalized DDI，Developer Mode 开；按 spike README 使用环境
   UDID/DEVELOPMENT_TEAM 安装，解锁/Trust，生成并推送新 RP record。
2. 装 LocalDevVPN、打开 VPN，App Start，授予本地网络/定位，输入自动化设备密码。
   默认不 authorize，记录启动时长与 capabilities/replies、每项 dlerror；失败先拉日志。
3. 如 daemon 或 automation 失败，重新启动 App 并启用 authorize 开关作对照，
   不要调用已知关闭自动化的初始化/automation-session 方法。
4. ready 后读屏（bundle id/节点数/耗时），截图（缩略图/耗时），点自测按钮
   （只点本 App 靶，计数必须恰好 +1）。Stop、重启，观察 session 释放/恢复。
5. 手机内 RP 观察 60 秒，检查 Marco/Polo 与 DTX 各自状态；再切到其它 App、
   后台 30 分钟、VPN 断连重连，返回拉 Documents/selfrunner.log。记录定位权限。
6. Mac 的 rp-connect 使用实际可达的网络地址排查；10.7.0.1 是手机内 VPN 回环，
   不假设 Mac 能直接访问。日志拉取命令见 spike README。

离线编译验收：`CARGO_HOME=/private/tmp/self-boot-cargo-home CARGO_BUILD_JOBS=2`
使用 `/opt/homebrew/opt/rustup/bin/cargo`、`--offline`：self-boot tests
12 library + 2 CLI/配对文件 tests passed；clippy `-D warnings` passed；probe binary
build 及三项 `--help` passed；build-lib.sh 产出 32 MB aarch64-apple-ios release
libself_boot.a；xcodegen generate 与指定 iphoneos Debug/CODE_SIGNING_ALLOWED=NO
build succeeded；两脚本 bash -n passed。Xcode 日志在 spike build/xcodebuild.log。
没有运行 install.sh、devicectl、iphone-use 或任何设备连接。

工作区边界例外：首次 git status 已有范围外 Cargo.toml/Cargo.lock 修改（S0a
workspace member/依赖 lock 更新），本单未编辑/撤销它们；因此最终整个工作区 status
不能满足字面的「只含范围内文件」，本单新增修改均在授权范围内。无 git commit。

### S0b 真机问题 1 — 栈溢出（已修）与 EHOSTUNREACH（2026-10-11）

真机：iPhone 14 Pro / iOS 27.0.1。先前 C ABI 在 App 的小栈上构造/轮询大
Rust future 导致栈溢出，已由专用 16 MiB FFI worker 栈、8 MiB Tokio worker
栈及在 worker 内构造/Box::pin future 修复；本单保留该实现，现有小调用栈
回归测试通过，未重新操作真机。

随后手机 Start：LocalDevVPN 已连接、状态栏有 VPN、本地网络权限已开，
约 2 秒失败：`Io(Os { code: 65, kind: HostUnreachable, message: "No route to host" })`。
原 Rust pairing 连接直接 connect `10.7.0.1:49152`，第二条 tunnel TCP 连接
也没有接口/源地址绑定；Swift 传固定地址和端口。此现象还不能证明 pairing
端口、pair verify、TLS 或 RSD 是否可用。

依据是本单提供的 LocalDevVPN Constants.swift / PacketTunnelProvider.swift
配置事实：接口 `10.7.1.1/32`，对端 `10.7.0.1/32`，includedRoutes 只有
对端 /32、excludedRoutes 为默认路由，VPN 实现是数据包反射器。另有已经
真机跑通的 actell 文档陈述：找接口地址匹配的 utun、Bonjour 发现 RP 端口、
每条 TCP 使用 IP_BOUND_IF 与接口 IPv4 绑定。本单仅采用用户给出的事实，
没有查看或复制 actell GPL 代码；路由根因与协议最终结果仍待真机确认。

改法：

- `connect_rp(pairing, addr, Option<LocalBind>)` 支持接口名或正 index + 本地
  IPv4；两者须同时提供，均不提供保持普通 connect。共用 TcpSocket 创建路径，
  socket2 SockRef 的 bind_device_by_index_v4 在 Apple 设置 IPPROTO_IP/IP_BOUND_IF，
  然后 bind 接口 IPv4:0，再 connect；pairing 和 tunnel 两条连接都走此路径。
- pairing connect / pair verify / create tunnel listener / tunnel connect / TLS /
  RSD 分阶段错误和 30 秒 deadline；connect 错误携带目标、接口、源地址。
  C ABI 保留原入口，新增 bound 入口（可选字符串均 NULL 表示不绑定），
  App 外层 deadline 35 秒；FFI 使用 Display 保存错误，保留可读阶段前缀。
- getifaddrs 枚举所有 IPv4/IPv6 名称、index、地址与 UP/RUNNING/POINTOPOINT
  flags，通过线程本地 borrowed JSON 返回；App Start 先复制并记录全文，选择
  接口 IPv4 精确匹配的 utun（多个匹配时取最小 index），否则报错并列全部 utun。
- 设置分为接口地址、对端地址及自动 Bonjour/手动端口，默认 10.7.1.1 /
  10.7.0.1 / 自动。NetServiceBrowser 浏览 `_remotepairing._tcp` 六秒并解析
  每个服务，记录发现、解析成功的全部地址/端口和解析错误。优先接口地址
  匹配、其次对端地址匹配，同级按服务标识与端口排序；连接目标始终是对端。
  无有效匹配时日志明确标注回退 49152；手动模式直接使用验证过的端口。
  Info.plist 和 xcodegen 配置均加入该 Bonjour service。Stop 取消发现与解析。
- build-lib.sh 在 heavy-cargo 存在时使用它，否则回退 cargo，保留每次构建
  CARGO_BUILD_JOBS=2 与独立 target；本机 heavy-cargo 不存在。

离线验收（使用可写缓存 `CARGO_HOME=/private/tmp/self-boot-cargo-home`、
`PATH=/opt/homebrew/opt/rustup/bin:$PATH`、`CARGO_BUILD_JOBS=2`，cargo 加 `--offline`）：

- `cargo test -p self-boot`：16 library tests passed，1 failed。绑定解析与
  C ABI 接口列表 JSON 含 lo0 均通过；lo0 + 127.0.0.1 TCP 实连测试在
  `network.rs` 的 TcpListener::bind 阶段被沙箱拒绝（EPERM / Operation not permitted），
  尚未进入 IP_BOUND_IF 调用，不能算该代码路径验收通过；测试保留且不跳过。
- `cargo clippy -p self-boot -- -D warnings`：通过。
- `spikes/self-runner-app/build-lib.sh`：aarch64-apple-ios release 静态库通过。
- `xcodegen generate` 与指定 iphoneos Debug / CODE_SIGNING_ALLOWED=NO
  `xcodebuild`：通过，日志 `spikes/self-runner-app/build/xcodebuild-bound.log`；
  静态库日志 `build/build-lib-bound.log`。这些日志包含系统模拟器服务不可用
  与 AppIntents 元数据跳过提示，不影响 iphoneos build success。

未 commit、未操作真机；EHOSTUNREACH 是否消失、实际 Bonjour 返回以及 RP /
TLS / RSD 成功与否仍需后续真机验收。范围外 .gitignore / Cargo.toml / Cargo.lock
在开始时已有改动，保留；本次 cargo 自动向 lock 中的 self-boot 依赖列表加入的
libc/socket2 两行已撤回，未覆盖其它会话的 lock 改动。

### S0c 真机记录 1（2026-10-11）

用户提供的真机反馈：iPhone 14 Pro / iOS 27.0.1 已跑到 automation enabled。
RP 经 `utun6/10.7.1.1` 连接 `10.7.0.1:60930` 用时 **148 ms**；IDE 会话、
dlopen XCTest 成功，daemon session **17 ms**，enableAutomationMode **3 秒**成功。
`XCTTargetBootstrap` 不存在不影响此次启动。心跳 shim 在手机 RP 路径同样出现
`BrokenPipe`，但 DTX / test_sessions 持续 **5 分钟以上**存活；这不代表心跳恢复，
也还不是 30 分钟后台验收。LocalDevVPN 需在 Wi-Fi 下启动，见
<https://github.com/jkcoxson/LocalDevVPN/issues/31>。RemotePairing 端口会漂移，
需 Bonjour 解析，不能依赖固定 49152。

前台为本 App 时读屏超过两分钟不返回；原桥接没有步骤日志，阻塞位置未知。
Bonjour 本机服务 host 为本机 `.local.`，地址只有 en2 的 `169.254.x` /
`fe80::…%en2`，端口 60930；原规则拒绝后回退 49152 出现 EHOSTUNREACH/拒绝，
手动 60930 成功。全局 busy 使卡住的调用锁住其它操作按钮。

本次修改（离线，未操作真机）：

- AX、截图、点击增加私有调用前后的步骤名与毫秒耗时日志。AX 取得 PID 后
  立即标注 self-read，结果包含 bundleID / pid / selfRead；已取得的元数据在
  snapshot 失败或异常时仍保留。新增「5 秒后读屏」及切换其它 App 的提示，
  五秒后在 worker 上调用同一只读实现，结果写日志。
- AX、截图、点击各用独立串行 worker，boot 轮询不再被 AX 卡住。main 每次
  操作设 20 秒看门狗（延时读屏从五秒延时结束后计），超时输出
  `<op> still running after 20 s at step <最近步骤>` 并释放 busy。迟到步骤和
  结果标 late；操作身份检查防止旧结果解除新操作的 busy。Stop 后返回也标 late。
  看门狗无法取消私有同步调用，同类队列可能仍被阻塞；其它类型及 boot 可继续。
  后台计时仍受 iOS 调度/定位权限限制。
- 点击仍在 main 校验并捕获可见靶按钮坐标，事件构造和 synthesis 在 tap worker，
  保留自身 PID/竖屏/无遮挡约束与计数检查。截图仍用 PNG daemon request 与
  10 秒回调等待。
- Bonjour 以解析 host 匹配本机 gethostname（忽略大小写、尾点和 `.local`），
  或任意解析地址匹配 `ipu_boot_list_interfaces_json` 中任意本机接口地址，
  判为本机服务。IPv6 按二进制比较并忽略 scope 后缀以兼容现有 JSON。
  全部候选和选择依据写日志，多个本机候选取最近解析者；用所选端口连填写的
  **对端地址**。保留手动模式与无本机候选时明确记录的 49152 fallback。
  无需修改 self-boot C ABI。

编译验收：要求的 `build-lib.sh`、`xcodegen generate` 与 iphoneos Debug /
CODE_SIGNING_ALLOWED=NO `xcodebuild` 通过。日志为
`spikes/self-runner-app/build/build-lib-s0c.log` 与 `build/xcodebuild-s0c.log`。
未改 self-boot，未触发条件 test/clippy 验收。未 commit；范围外已有
`.gitignore` / Cargo.toml / Cargo.lock 改动保留。

### S0c 真机记录 2（2026-10-11）

用户提供的真机事实（iPhone 14 Pro / iOS 27.0.1，修改前 spike）：自动化
ready 约 **6 秒**；截图 **153 ms**；自测靶按钮点击计数 **0→1** 成功，但
`synthesizeWithError:` 耗时 **5393 ms**。读本进程或「设置」（self-read=NO），
不论是否 authorize 自身 PID，`requestSnapshotForElement:attributes:parameters:error:`
均在约 **65 秒**返回 `Timed out while fetching snapshot from testmanagerd.`。
`applicationMonitor` → `applicationProcessWithPID:` → `bundleID` 得到 unknown。
App 在后台日志持续写入，说明该观察期间仍在运行，不代表长期后台存活保证。

依据用户给出的 sorrycc/Fingertip（MIT）
`App/Automation/DeviceAutomation.m` 同类 self-runner 读屏用法，以及本仓库
`runner/IPhoneUseRunner/IPhoneUseRunnerUITests/IPURBridge.m` 的实际参数、属性
过滤与 quiescence 实现；本次未修改 runner，未操作真机。

本次修改：

- 主读屏先取 `XCUIDevice.accessibilityInterface`，缺失时取
  `XCAXClient_iOS.sharedClient`；`defaultParameters` 可变副本只覆盖 maxDepth。
  深度默认 24，UI Stepper 可调 1–100，派发时捕获本次深度。
  identifier/value/label/frame/enabled/elementType/placeholderValue/selected/hasFocus
  九项 key paths 经 `axAttributesForElementSnapshotKeyPaths:isMacOS:` 转换，
  不过滤转换器添加的属性；返回结果严格取 `rootElementSnapshot`。
  默认参数全部键值首次可用时记录一次，每次记录实际参数、key paths、转换后
  属性、节点数、根 label、前 15 个有 label 节点的 elementType/label/frame
  及步骤/总耗时；遍历保持 2000 节点上限，超出标 truncated。错误也记录总耗时
  与已取得的 PID/selfRead/bundle 元数据。
- bundle 首选 client 的 `applicationProcessTracker` →
  `applicationProcessWithPID:` → `bundleID`；缺失、异常或 unknown 时回退
  device 的 `applicationMonitor`，逐个记录成功/失败，结果带 bundleSource。
- 新增「读屏（runner 参数）」按钮：同一深度、maxNodes=2000，按 IPURBridge
  从默认参数开始加 maxDepth/maxChildren/maxArrayCount/traverseFromParentsToChildren；
  同样转换含 children 的十项 key paths，再按九个属性后缀过滤，保留原实现
  的映射 fallback；根取 `_rootElementSnapshot`，缺失则用返回结果自身。
  此模式使用 accessibilityInterface，与主模式分开标 ax-runner/ax-default 写日志。
  现有「5 秒后读屏」调用主模式，可继续对「设置」作只读验证。
- enableAutomation 成功后、ready 之前安装从 IPURBridge 复制的 quiescence
  bypass，源码注明来源及 MIT 归属；只替换原实现列出的 void wait 方法，
  dispatch_once 保证单次安装，记录实际替换列表。点击构造自身 bundle 的
  XCUIApplication，调用 `_performWithInteractionOptions:block:`，options=1|2
  跳过动作前后等待，内层继续记录 `synthesizeWithError:` 的毫秒耗时。
  保留前台、竖屏、可见、无遮挡 UIKit 靶按钮与自身 PID 限制及计数检查。
- 保留独立 worker、步骤日志、20 秒看门狗及 late 结果机制。看门狗不取消
  私有同步调用，同一 AX 队列上两种模式仍须等前一次调用实际返回。

离线验收：`PATH=/opt/homebrew/opt/rustup/bin:$PATH spikes/self-runner-app/build-lib.sh`
通过（heavy-cargo 不存在，脚本依规定回退 cargo，CARGO_BUILD_JOBS=2）；
`cd spikes/self-runner-app && xcodegen generate && xcodebuild -project
SelfRunnerSpike.xcodeproj -scheme SelfRunnerSpike -sdk iphoneos -configuration Debug
-derivedDataPath build/DerivedData CODE_SIGNING_ALLOWED=NO build` 通过。
日志：`spikes/self-runner-app/build/build-lib-s0c2.log`、`build/xcodebuild-s0c2.log`。
CoreSimulator 服务不可用和 AppIntents 元数据跳过提示未影响 iphoneos 构建。
本次编译成功不能证明 65 秒超时消失、bundle 解析成功或点击提速，均待后续
真机复测。未 commit，范围外既有 .gitignore/Cargo.toml/Cargo.lock 改动保留。


### S0c 结论（2026-10-11）

已验证（用户提供的真机事实）：Mac-free 启动链路为 LocalDevVPN + RPPairing
自连 + IDE 会话 + 进程内 XCTest + enableAutomationMode，约 **6 s**。
LocalDevVPN 需在 Wi-Fi 下启动，RP 端口需 Bonjour 解析。截图 **153 ms**；
纯合成点击成功（**5393 ms，约 5.4 s**，待优化）；后台继续运行，会话存活
**5 分钟以上**；心跳 shim 出现 BrokenPipe，但不影响上述已观察到的会话行为。

未通：无障碍快照不论自读/他读、是否 authorize，均约 **65 s** 超时，报
`Timed out while fetching snapshot from testmanagerd`。新版 quiescence bypass
安装成功，但点击在 `XCUIApplication initWithBundleIdentifier:` 抛出
`NSInternalInconsistencyException: Device is not configured for UI testing - use of XCUIApplication is not supported...`，
计数 **0→0**；因此 XCUIApplication 路径也未通。

本轮离线修正：点击恢复为直接调用 `XCSynthesizedEventRecord synthesizeWithError:`，
移除 XCUIApplication 构造及 `_performWithInteractionOptions:` 包装，记录 synthesis
耗时（异常时也记录）。保留原有 quiescence swizzle；依据 TouchSynthesis 公开
README/注释描述的行为事实独立实现空闲通知立即回调 YES，不复制其代码。
对 `XCTRunnerAutomationSession` / `XCTAutomationSession` 的
`notifyWhenMainRunLoopIsIdle:`、`notifyWhenAnimationsAreIdle:` 与 `_XCT_` 前缀变体，
仅在类、方法及 void/block 签名存在时安装，并记录实际安装列表。
读屏的 defaultParameters / applicationProcessTracker 实现保持不变，本轮未能验证。
这些修正是否提速仍需真机复测。

假设（尚未验证）：完整 UI 测试会话需要 IDE 侧响应 testmanagerd 回调，如
`_XCT_testBundleReadyWithProtocolVersion:minimumVersion:`、
`_XCT_testRunnerReadyWithCapabilities:` 等，并在 runner 侧调用
`finishInitializationForUIAutomation`。TouchSynthesis 注释称后者会关掉自动化，
可能正因其 IDE 侧不回应；这属于待验证的解释。S0a 明确未实现这些回调。

下一步 **S0d**：在 self-boot 中实现 IDE 侧回调应答，参考 MIT idevice 的 xctest
模块及 WDA 启动流程中 IDE 的应答顺序；Mac 上可先用 USB 探针 + 正式 runner
抓取 xcodebuild 的 DTX 交互作对照。随后在宿主中尝试
`finishInitializationForUIAutomation` / 设置 UI testing 配置，再测快照。
备选是不经 testmanagerd，直接走进程内 AX（AXRuntime / `AXUIElement` 私有 API）
读取其它 App。本轮不实现 S0d，不操作真机。

本轮离线验收：要求的 `build-lib.sh`、`xcodegen generate` 与 iphoneos Debug /
`CODE_SIGNING_ALLOWED=NO` 的 `xcodebuild` 均成功。日志为
`spikes/self-runner-app/build/build-lib-s0c3.log`、`build/xcodebuild-s0c3.log`。
`rg -n "XCUIApplication" spikes/self-runner-app/Sources` 仅命中保留的 quiescence
安装逻辑，不在点击路径。CoreSimulator 服务错误与 AppIntents 元数据跳过提示
未影响本次 iphoneos 构建。未改 runner/、crates/，未 commit，未操作真机。

### S0d 工具：xctest-trace

Mac 经 USB 启动已安装的正式 XCTest runner，记录 IDE 参考会话；本轮仅离线
实现/验收，未操作真机。默认从 installation_proxy 查找 bundle id 含
`com.leeguoo.iphone-use.runner` 且以 `.xctrunner` 结尾的唯一 app；有多个候选
时必须用 `--bundle` 指定实际安装 id。使用 idevice 的 `TestConfig`，不访问
runner HTTP（包括 ready 后，也不调用 `/status`）。

```bash
./target/debug/self-boot-probe xctest-trace --udid "$UDID" --observe-secs 30 --out trace.jsonl
# 可选：--bundle com.leeguoo.iphone-use.runner.xctrunner
```

`--observe-secs` 默认 30，从 runner 启动并授权后计时，包括等待 ready/driver
channel 的时间；到期取消读循环，通过 DVT `killPid:` 终止本次启动的 runner，
再释放 control/test 会话与 tunnel。启动后发生协议错误也走同样清理路径；
授权失败会尝试 kill。清理最多等待 10 秒，失败返回错误；killPid 是 idevice
原有的不等待确认调用，因此退出是否完成仍需真机验证。启动前的 RSD handshake
沿用 idevice 的超时和重试，不计入观察窗口。不保证 SIGKILL/进程崩溃后的清理。

输出为每条逻辑 DTX 消息一行 JSON（分片重组，不逐 TCP packet 记录）：

- `timestamp_unix_us`、`elapsed_us`：Unix/工具启动以来时间，微秒。
- `direction`：`send` / `receive`（Mac 视角）；`connection` 为连接标签，
  `connection_role` 为 `control` / `test` / `other`（DVT 等）；标签末尾序号区分重试连接。
- `channel`：idevice 解码后的带符号 channel；`channel_name` 从双向 root
  channel request 建立映射，未见 request 时为 null，channel 0 为 root。
- `identifier`、`conversation_index`、`expects_reply`、`message_type`：DTX header。
- `selector`：payload 是字符串时记录；`aux`：解码后的参数数组；`payload`：
  完整解析的 NSKeyedArchive，包括 ready 回调回复中的 XCTestConfiguration。
  无法解码的字节用 `{base64, byte_length, truncated}` 表示，最多保留前 1024 字节。

`--out` 不覆盖现有文件；省略则 JSONL 写 stdout，诊断写 stderr（错误终止时
CLI 原有 error JSON 也在 stdout）。逐消息 flush，磁盘写入失败返回错误。
文件可能包含 app 路径、配置/环境参数，分享前自行检查。

idevice 的 debug 日志没有覆盖自动 raw reply，故复制其 XCTest 编排、DTX client
及 process_control 到 `crates/self-boot/src/xctest_trace/`，在统一收发入口插记录。
来源是 MIT idevice rev `41c388baef0dd55c0ce4924789abbfde889d9b87` 的
`idevice/src/services/dvt/{xctest/mod.rs,remote_server.rs,process_control.rs,message.rs}`；
保留许可于 `LICENSE.idevice`。仍复用原有 TestConfig、XCTestConfiguration、类型与
listener，保留 USB → CoreDeviceProxy → RSD 及 DVT lockdown fallback 编排；未改
workspace `[patch]`。DTX 解码器保留未知 payload/aux 的原始字节供 base64 回退；
不合法 frame（含超过 64 MiB 的逻辑消息）终止连接，不伪造协议内容。
这是一份 idevice IDE 会话对照，不是 xcodebuild 的抓包；未证明 iOS 27 真机成功。

离线验收：Clippy `-D warnings`、桌面 `--bins`/`xctest-trace --help`、
`IPHONEOS_DEPLOYMENT_TARGET=17.0` 的 aarch64-apple-ios release lib 均通过。
完整 `cargo test -p self-boot` 的既有
`network::tests::bound_loopback_connect_and_interface_json` 在当前沙箱因 TCP
listen 返回 `Operation not permitted` 失败；仅跳过此测试时其余 22 个单元测试
和 1 个 doc-test 全通过。新增测试全部离线，无设备访问。机器没有 heavy-cargo，
按仓库约定回退 `CARGO_BUILD_JOBS=2 cargo`，重型构建顺序执行。

#### S0d — formal runner trace on iOS 27.0.1 (2026-10-11)

`self-boot-probe xctest-trace --bundle <runner .xctrunner id> --observe-secs 30` over USB, 349 DTX messages.
IDE side: initiate control + session, processcontrol launch, authorize PID, runner opens the
`XCTestDriverInterface` channel, IDE sends `_IDE_startExecutingTestPlanWithProtocolVersion:`, runner sends
`_XCT_testRunnerReadyWithCapabilities:` and the IDE answers with an `XCTestConfiguration`
(`_XCT_didBeginInitializingForUITesting` follows ~80 ms later). The self-runner's IDE side never answers this.

The runner's own "Initializing for UI testing" log (~20 ms): link XCTAutomationSupport, daemon session,
automation mode, loading Accessibility, localized string gathering, AX timeout 60 s, AX notification
registrations, background assertion. The self-runner only performs the daemon session and automation mode
steps, which matches the AX snapshot timeout seen in S0c. The raw trace stays out of the repo (it contains
the runner launch environment).

## Backlog (2026-10-11, to revisit)

1. Boot robustness: remember the last discovered RemotePairing port and try it before Bonjour; tell the
   user clearly when LocalDevVPN is down or was started on cellular, and offer a one-tap reconnect.
2. Link the on-phone daemon from `upstream/feat/on-phone-daemon` (`libipu_embed.a`) into the host app, so
   screenshot and tap are reachable through `127.0.0.1:8787` instead of only the spike's test buttons.
3. Progress write-up for upstream (issue draft): what "Mac only for setup" has verified so far and what is open.

### S0d — bootstrap implementation research (2026-10-11)

本轮只读研究本机 **Xcode 26.6 / build 17F113 / iphoneos26.5** 的 iPhoneOS
SDK arm64 二进制，以及当前宿主源码。没有连接、安装或操作手机，没有修改运行时
初始化路径。**SDK 实现不能直接证明 iOS 27 personalized DDI 的实现相同**。

新增可重复取证入口（不读配对文件、不连接设备、拒绝覆盖已有输出）：

```bash
python3 scripts/research-xctest-bootstrap.py --out /tmp/xctest-bootstrap-evidence.md
```

输出包含 Xcode 版本、三个 framework 的路径 / SHA256 / Mach-O UUID，及选定函数
的完整反汇编。地址仅用于标识本轮 SDK 证据，不能作为运行时调用地址。

#### 收敛后的证据

1. **不能把重复调用 finish 当作完整初始化。** XCTestCore
   `-[XCTRunnerDaemonSession initWithConnection:remoteInterfaceCapabilities:]`
   在 `0x65f00–0x65f18` 检查并调用 `finishInitializationForUIAutomation`。
   XCUIAutomation 的该方法（`0x7adfc`）设置 implicit event confirmation interval
   为 5 秒、设置两个 fetchAttributes reply 的解码类；没有加载 Accessibility，
   也没有关闭 automation mode。宿主已先加载 XCUIAutomation 再建立 daemon session，
   因此在这个 SDK 实现下构造过程已覆盖该调用；真机仍需核对实际方法是否存在。
   先前「未显式调用 finish 所以缺完整 UI 初始化」的解释需要修正。
2. **XCUIApplication 报错有明确的本地配置条件。**
   `commonInitWithApplicationSpecifier:device:` 在 `0xed7c` 检查
   `configuredForUITesting`；`-[XCUIDevice configuredForUITesting]`
   （`0x28fc0–0x29000`）读取
   `XCTestConfiguration.activeTestConfiguration.initializeForUITesting`。
   当前宿主不安装 active configuration。这解释该 SDK 的异常触发条件，
   不能据此断定 AX snapshot 超时也由同一条件引起。
3. **显式加载 Accessibility 是更直接的 AX 实验。**
   `-[XCAXClient_iOS loadAccessibility:]`（`0x4dc4–0x4ea0`）依次执行
   `_loadAccessibility:`、`_setAXTimeout:error:`，再注册通知
   4002 / 1006 / 1021 / 3031；分别对应 user testing、alert、PID status changed、
   app accessibility ready。`_loadAccessibility:` 经 daemon 请求
   `loadAccessibilityWithTimeout:reply:`，随后设置 localized string gathering。
   当前 `SpikeBridge` 的 enable/read 路径都没有这一步。
4. **XCTest 有独立于测试计划的完整初始化入口。** 导出 C 符号
   `XCUIEnableUIAutomation`（`0x683c`）创建 XCTestConfiguration，设置
   initializeForUITesting / reportActivities / emitOSLogs 为 YES、安装 active
   configuration，再调用内部 `XCUIInitializeForUITesting`。后者要求主线程，
   开启 automation、调用 `loadAccessibility:`，还请求 background assertion。
   但入口里的 automationFrameworkPath 固定为
   `/Developer/Library/PrivateFrameworks/XCTAutomationSupport.framework`；
   已有 iOS 27 真机记录的 DDI mount 为 `/System/Developer`，默认路径是否可解析
   尚未验证。主线程同步调用也可能阻塞 UI，不能承诺看门狗可取消。
5. **IDE 配置应答仍是缺口，但不应先假定该回调在宿主里发生。**
   `_XCT_testRunnerReadyWithCapabilities:` 由
   `XCTRunnerIDESession._locked_makeTestConfigurationFuture` 的 block
   （`0x68ae4`）发送。当前宿主只建立 daemon shared session，没有创建 driver /
   runner IDE session，也没有请求这个 future。Rust `dtx.rs` 对 expects_reply
   仍统一回空 ACK；未来进入完整 driver 路径时必须提供语义正确的配置应答，
   但单独加 responder 不等于宿主会请求或消费配置。

#### 下一轮实验与验收（尚未实现 / 未操作真机）

每组从新进程开始，保持 DTX / PID 授权 / quiescence / snapshot 参数不变，
逐项验证；Stop → Start 不算新进程。已有 shared-session 实现有进程级单次初始化
保护，SDK 异常文本为 `Cannot initiate shared session more than once.`。

| 实验 | 唯一干预 | 观察和验收 |
|---|---|---|
| E0 基线 | 无 | 记录 active configuration 是否存在、initializeForUITesting、configuredForUITesting、实际 AX client 类；复测当前快照耗时 |
| E1 AX 加载，优先 | 对原有 accessibilityInterface 调用一次 loadAccessibility: | 先核对运行时方法参数 / BOOL 返回签名，再捕获 BOOL、NSError domain/code、耗时；成功后用同一快照路径读本 App 和「设置」 |
| E2 本地配置 | 新进程安装本地 XCTestConfiguration，initializeForUITesting=YES、sessionIdentifier 与 IDE UUID 一致 | 记录 configuredForUITesting 是否变为 YES；只构造 XCUIApplication，不启动/终止其它 App；与 E1 分开，不能把谓词变化当 AX 已修 |
| E3 完整初始化 | 经复核和 framework 路径验证后，评估导出 XCUIEnableUIAutomation | 用 dlsym 查真实导出，不调用硬编码地址或局部符号；主线程要求、默认路径和进程全局状态须先解决 |
| E4 完整 driver | 仅在证据要求走 driver 时实现 IDE 配置 responder / reverse channel | 同时记录请求、回复内容及宿主实际消费配置 / 进入 UI 初始化；空 ACK 不算配置回复 |

E1 在 AX worker 执行以保留 UI 和 boot 轮询；运行时若要求主线程则停止该实验，
不能强行绕过断言。20 秒 UI 看门狗不取消私有同步调用；超时后不堆叠新的 AX 操作，
等待实际返回或结束进程。E3 不能直接套用 AX worker。

「设置」快照须返回多个有效节点和可识别标签，并有可靠 PID / bundle 来源；
unknown 元数据或空树不能记成功。报告实际延迟，至少三个独立新进程结果，
不把人为设置的超时当成已恢复。截图和靶按钮计数 +1 用作回归核对。
5.4 秒合成延迟是否与 SDK 的 5 秒 implicit confirmation interval 有关也是
待验证假设，不能在 AX 实验里同时修改该常量。

本轮取证脚本在本机运行成功，所列 SDK 函数均找到，已有输出覆盖请求被拒绝；
未运行 Rust / iOS 构建，未声明硬件问题已修。参考浏览仅检查 MIT Fingertip
当前公开源码结构，未据此新增未核实的初始化事实。

独立意见：`aiw claude ... --ro` 返回 rc=1（HTTP 401 authentication failure），
未取得评审产物，`.provider` 未生成；工具提示在用户终端运行 `claude` 后 `/login`。
因此本轮只落地取证工具和研究记录，初始化改动仍待独立复核。
文本代码提炼 workbuddy 返回 rc=124（600 秒超时、无产物）；附录草稿由 qoder
返回 rc=0、`.provider=qoder`、无顶替。已修正草稿中的 SDK/真机边界、方法调用
无需 dlsym、config 字段名等问题；所有核心结论由主会话核对二进制 / 源码。

后续修改范围 / verify 依赖：`crates/self-boot/src/**/*.rs`、
`spikes/self-runner-app/Sources/*`、`scripts/research-xctest-bootstrap.py`、
`docs/research/2026-10-self-runner.md`。DDI 手机挂载、30 分钟后台、VPN 重连、
daemon 整合仍未验收，保持原阶段门槛。

### S0d — E0/E1 manual AX experiment ready (2026-10-11)

已实现并安装可选诊断，不在 Start 时自动执行。范围为
`spikes/self-runner-app/Sources/{SpikeBridge.h,SpikeBridge.m,SelfRunnerSpike.swift}`：

- **E0 状态**：先验证 getter 的运行时 ABI，再记录 configurationPresent、
  initializeForUITesting（仅当配置存在）、configuredForUITesting、clientClass、
  mainThread。不创建或修改 active configuration。
- **E1 加载 AX（一次）**：在原 AX worker 上取得
  `XCUIDevice.sharedDevice.accessibilityInterface`；严格核对 BOOL 返回和 NSError **
  参数的签名后才调用 `loadAccessibility:`，记录 success、NSError domain/code、
  exception 与 elapsedMs。BOOL 类型使用平台 `@encode(BOOL)`，不硬编码 B/c。
  每进程只允许一次尝试（失败也消耗尝试）；Stop 不清此标记，新进程才复位。
- `axInFlight` 在真实返回前保持；E0/E1 和所有读屏按钮互斥。20 秒看门狗仍只释放
  UI busy，不取消同步调用；屏幕显示 AX 仍在执行。Stop 日志说明无法取消，迟到结果
  记 late，不重新设 ready；截图 / 点击 / boot 仍有独立 worker。
- ready 日志修正了「finish 未调用」的误导。未修改 Rust / DTX / PID 授权 /
  quiescence / UI testing 配置，也未启用完整 XCTestDriver。

验收：xcodegen 与 iPhoneOS Debug 无签名构建通过，日志
`spikes/self-runner-app/build/e1-xcodebuild.log`；使用已有本机签名配置的构建通过，
`codesign --verify --deep --strict` 通过，日志 `build/e1-xcodebuild-signed.log`。
没有重编译 Rust（使用已存在的 libself_boot.a）。已通过 devicectl 安装并以
terminate-existing 打开新进程；手机解锁 / 自动化密码由用户本人操作。
**安装成功不等于 E1 或 AX 快照通过；等待本轮真机日志。**
主会话还在 macOS 原生 ObjC harness 中运行了同一 CheckedSignature helper：
正确 BOOL / NSError ** 形状可调用，错误返回类型 / 参数数量 / 指针类型 / 缺失 API
四种情况均抛出并拒绝调用。该检查验证 ABI 防护，不验证 iOS 私有实现。
安装后拉取日志，最新 `2026-10-11T15:24:23Z` 为新进程 idle；尚无 E0/E1 结果。

手机操作：LocalDevVPN → Start → 输入自动化密码 → E0 状态 → E1 加载 AX（一次），
等实际返回后，用与基线相同的深度 / 参数读屏，并以「5 秒后读屏」切到设置。
截图及靶按钮计数 +1 另行核对。需严格因果比较时先独立新进程跑基线，再独立新进程
跑 E1；至少三次，记录原始耗时。不要在调用仍挂起时继续点击，结束进程再开始。

独立复核记录：Claude rc=1（认证熔断，无 `.provider`），因此仍缺 Claude 审阅；
诊断实验为手动默认关闭，不将此缺失扩大成所有研究的阻塞。qoder 第二意见、
diff 审閱与记录草稿均 rc=0、`.provider=qoder`、无顶替；主会话采纳真实返回后释放
in-flight / 不重试语义，驳回无证据的 UAF、缓存冻结推测及其写反的 BOOL 平台类型。
diff 审阅未发现具体阻塞，不把文本意见当真机证据。

### S0d — E0/E1 首次真机结果 (2026-10-11)

同一新进程在 `15:38:10Z` 完成自动化启用，随后手动执行 E0、E1。
未修改 active configuration、snapshot 参数、DTX、PID 授权或 quiescence。
原始手机日志留在本机临时目录，不外发；以下为主会话核对的技术结果。

| 观察 | 真机结果 |
|---|---|
| E0，15:38:12Z | configurationPresent=false；configuredForUITesting=false；clientClass=XCAXClient_iOS；mainThread=false；整体 61 ms |
| E0 配置字段 | 无配置时未读取 initializeForUITesting，不能把 nil 消息返回值记作真实配置值 |
| E1 ABI | sharedDevice / accessibilityInterface 返回 `@`；loadAccessibility: 返回 `B`、3 个参数（含 self / _cmd）、显式参数 `^@`，与平台 @encode(BOOL) / NSError ** 匹配 |
| E1，15:38:18Z–15:38:19Z | loadAccessibility: 返回 success=true，733 ms；整体 735 ms；未报告错误或异常 |

这证明显式 AX 加载入口在当前 iOS 27.0.1 真机能成功返回。**尚不能判定
snapshot 超时已解决**，也不能据此认定 active configuration 或
configuredForUITesting 已改变（E0 在 E1 之前，尚未复测）。
加载后的同参数本 App / 设置快照、截图与靶按钮计数待收集；至少三个独立新进程
复测和因果比较尚未完成。此次只有一组 E0/E1，无同进程 E1 前的快照对照。

本节事实由主会话从本机原始日志核对；脱敏技术事实的文本草稿派给 workbuddy，
rc=0、`.provider=workbuddy`、无顶替，未向文本座位发送手机原始日志或网络信息。

### S0d — E1 后首次有效快照 (2026-10-11)

`16:10:56Z`，用户在 E1 成功的同一进程点选「读屏（runner 参数）」。
本次不是普通「读屏」，不把两组参数的结果混同。

| 项目 | 真机结果 |
|---|---|
| 前台对象 | activeApplications 223 ms，systemApplication 103 ms；返回 PID 与 getpid 相等，selfRead=true |
| bundle 解析 | applicationProcessTracker / applicationMonitor 均 unknown；随后 Mac 只读 devicectl 查询同一 PID，路径确为 SelfRunnerSpike.app/SelfRunnerSpike，属于外部校验，不是宿主解析成功 |
| 请求参数 | maxDepth=24，maxChildren=2000，maxArrayCount=2000，traverseFromParentsToChildren=true |
| 实际属性 | XC_kAXXCAttribute 前缀的 Value / Identifier / Frame / HasNativeFocus / Label / PlaceholderValue / ElementType |
| 快照返回 | requestSnapshotForElement:attributes:parameters:error: 316 ms；取 _rootElementSnapshot 后遍历成功 |
| 节点 | 43，truncated=false；根 label 为 Self Runner；包含 Self Runner · ready、Start、停止、读屏、截图等实际标签 |
| 总耗时 | 桥接 elapsedMs=659；UI 操作 761 ms，两种计时分别记录 |

**E1 后本 App 的 runner 参数快照已成功，历史同类约 65 秒的超时此次未出现。**
这是实际节点结果，不是根据 loadAccessibility 的 BOOL 推断。未主动安装配置。
无同轮加载前快照、尚无默认模式对照且只测一次，不能声称严格因果证明，
也不能声明「设置」跨 App 快照、默认参数、截图或点击回归通过。
启动后约 33 分钟的一次成功，不构成 30 分钟连续后台验收。
后续保持当前进程，补普通读屏 / 5 秒后切设置 / 截图 / 靶按钮，再安排独立新进程复测。

本节脱敏技术事实草稿 workbuddy rc=0、`.provider=workbuddy`、无顶替；
原始手机日志、进程列表和私人界面由主会话本机核对，未外发。

### S0d — USB 研究命令入口与跨 App 快照 (2026-10-11)

用户要求不通过 iPhone 镜像操作。新增 `ResearchMailbox.swift` 与
`scripts/research-selfrunner-command.py`，仅为研究控制入口；启动参数
`--research-mailbox` 才开启，默认关闭。没有网络监听、额外 entitlement 或
background mode；未修改 Rust / DTX / bootstrap / AX 参数。
通过 devicectl appDataContainer 传输 `Documents/research-command.json`，
App 原子写 `research-status.json`；不启动正式 runner，不用镜像。

命令仅含 version=1、UUID id / runID、expiresAt、action，拒绝其它字段。
action 白名单为 start / e0 / e1 / read / read-runner / read-delayed / screenshot / tap；
status 是 CLI 只读动作，不下发命令。文件最多 4096 bytes、到期时间在未来
120 秒内、runID 匹配、本进程最多消费 128 个 UUID，无淘汰重放。
复用已有 worker / busy / axInFlight / E1 一次 / 前台自身靶按钮限制。
20 秒看门狗只标 stillRunning，不取消；returned 表示真实返回，**仍须检查 result**。
`--expect-run-id` 防进程切换；`--no-wait` 仅表示 USB 交付，不是操作成功。
App 启动即发布当前 runID，旧进程文件不执行；半截 JSON 等下一次完整内容。
密码仍由用户在系统框输入，通道不读取、不存储密码。

```bash
xcrun devicectl device process launch --device <device-id> --terminate-existing \
  dev.iphoneuse.selfrunner.spike -- --research-mailbox
python3 scripts/research-selfrunner-command.py --device <device-id> status
python3 scripts/research-selfrunner-command.py --device <device-id> --expect-run-id <run-id> read
```

离线 Foundation Swift harness 已验证 schema / 错误类型 / expiry / runID /
replay / 半截 / size / 重复读取 / busy 拒绝 / watchdog / 结果身份。
签名 iPhoneOS 构建和 codesign 通过，日志 `build/mailbox-xcodebuild-signed.log`；
`build/mailbox-goal.json` 的三项 verify 通过。USB 真机启动、E0、两种快照、
截图与点击均收到对应 command id 的真实返回。

同一新进程 `16:24Z–16:28Z` 的结果（设备未重启，先前 E1 已运行过）：

| 操作 | 节点 / 结果 | 快照或合成 / 桥接 / UI 耗时 |
|---|---|---|
| E1 前 E0 | configurationPresent=false；configuredForUITesting=false | 53 ms UI |
| E1 前 runner 参数自读 | 43 节点，Self Runner，未截断 | 205 / 524 / 584 ms |
| E1 前默认参数自读 | 43 节点，Self Runner，未截断 | 104 / 416 / 494 ms |
| E1 前默认参数他读设置 | 202 节点，根「设置」，含通知 / 专注模式 / 屏幕时间，未截断 | 211 / 529 / 5818 ms（UI 包含人为 5 秒延迟） |
| E1 前截图 | 509939 bytes，OK；只显示在 App，不新增图片文件 | 228 ms UI |
| E1 前靶按钮 | 0→1，SUCCESS | synthesis 5386 ms，UI 5601 ms |
| E1 | success=true | 加载 732 ms，UI 734 ms |
| E1 后 E0 | 两项配置谓词依旧 false | 76 ms UI |
| E1 后默认参数自读 | 44 节点，Self Runner，未截断（截图显示后 UI 已变） | 105 / 415 / 526 ms |
| E1 后首次靶按钮 | 1→2，SUCCESS | synthesis 316 ms，UI 529 ms |
| E1 后第二 / 三次靶按钮 | 2→3 / 3→4，均 SUCCESS | synthesis 316 / 312 ms，UI 523 / 524 ms |

他读 PID 与自身不同，Mac 只读 devicectl 确认对应
`/Applications/Preferences.app/Preferences`。这是有效跨 App 节点与外部 PID 证据；
宿主的 applicationProcessTracker / applicationMonitor 仍返回 bundleID=unknown，
**宿主 bundle 解析未通过**。跨 App 测试只常规启动设置，不读设置容器。

新进程没执行 E1 也能读，说明「新 App 进程」不能当作干净系统 AX 基线。
daemon 中的初始化状态可能保留，但尚未证明；不能声称 E1 严格因果证明，
也不能声称已完成三组干净对照。E1 后首次点击从 5.386 秒变为 316 ms，
同进程三次均约 0.31 秒，但仍需独立基线排除首次动作 / 缓存效应，不据顺序作因果判断。
当前已实际读到自 App 与设置节点；DDI 手机自挂载、30 分钟连续后台、VPN 重连、
产品 API 均未验收。USB devicectl 会启用 Mac DDI 服务，不能拿这轮作为 Mac-free
重启 / 挂载证据；控制端仍在 Mac。

派单：Claude 方案与收尾审查均 rc=1（认证熔断，无 provider，仍缺独立 Claude 审阅）；
qoder 方案 / diff 审查 rc=0、`.provider=qoder`、无顶替，未发现具体阻塞。
主会话采纳文件大小 / 严格 schema / 结果身份，驳回 runID 已防护下的跨重启重放
猜测和看门狗强行取消建议，保留被拒 UUID 消费以防延迟执行。
两段脱敏事实草稿 workbuddy rc=0、`.provider=workbuddy`、无顶替；修正草稿的
status 语法和歧义边界。原始手机日志 / 进程路径 / 私人界面均由主会话本机核对。

### S0c/S0d — 三进程复测与后台验收 (2026-10-11)

同一设备未重启、Mac USB 已挂 DDI；三组不同 App 进程分别重新建立 self-runner
会话（不是 Stop → Start）。固定靶按钮与既有 synthesis / quiescence 路径，
在每进程的 E1 前后对照，计数每次准确 +1。

| 组 | E1 前 synthesis | E1 后 synthesis | E1 load |
|---|---|---|---|
| 1，16:24Z 进程 | 5386 ms ×1 | 316 / 316 / 312 ms | 732 ms |
| 2，16:36Z 进程 | 5390 / 5376 ms | 313 / 314 ms | 728 ms |
| 3，16:39Z 进程 | 5297 / 5378 ms | 313 / 313 ms | 733 ms |

三组都由约 5.3 秒变为约 0.31 秒，第二 / 三组 E1 前重复点击仍慢，
排除了「仅该进程第一次点击慢」的解释。这支持将 loadAccessibility 作为
后续启动流程的候选步骤；尚未随机化顺序，也未区分其内部加载 / localized strings /
timeout / 通知子步骤的作用，不声称具体机制已证明。
组 2 在本进程 E1 前默认读自己 43 节点（桥接 531 ms），读设置 202 节点
（521 ms）；组 3 E1 后设置 202 节点（540 ms）。设备可能保留此前系统 AX 状态，
新进程不是读屏冷启动的干净基线，三组只算不同进程的可用性与点击对照。
宿主 bundle 解析仍 unknown，配置未主动安装。

后台窗口 `2026-10-11T16:40:37Z` 至 `17:10:37Z`，实际 1800 秒。
**USB 连接条件下后台调度存活与终点 AX RPC 通过。** 先取后台现场，再实际读屏，
最后以普通 launch 回前台，无 terminate / 自动重启；runID
`38E5E733-CF59-42ED-AB31-49B3F8E8F475` / PID 4863 全程不变。

| 检查 | 实测结果 |
|---|---|
| 窗口日志 | 严格窗口内 360 条 boot 采样，最大间隔 5 秒，无 restart / Disconnected / Closed，test_sessions=true |
| 后台端点 | 17:10:50Z status 新鲜，foreground=false / ready=true，busy=false / axInFlight=false，hits=4 |
| 后台读屏 | 17:11:07Z 设置 202 节点，selfRead=0 / truncated=0，桥接 425 ms / 操作总 520 ms |
| 读屏身份 | PID 2807 外部核实为 Preferences.app，实际标签含通知 / 专注模式 / 屏幕时间；bundle 解析仍 unknown |
| 回前台截图 | 同进程，323591 bytes / OK，操作总 162 ms |
| 回前台点击 | 同进程，计数 4 → 5 / SUCCESS，synthesis 316 ms / 操作总 525 ms |

后台不点击期间 hits 保持 4，是额外的无动作对照。分析脚本为检查尾部覆盖，
纳入终点后 15 秒，共 363 条；严格 1800 秒窗口为 360 条，首尾 16:40:38Z /
17:10:33Z。USB 持续连接、Mac 此前已挂 DDI、LocalDevVPN 开启、keepLocation=true，
定位权限 WhenInUse。初始后台快照取证至 16:41:04Z，此后至 17:10:47Z 不发设备命令，
不称整段 30 分钟绝对零命令。foreground 只在端点采样，连续未切回按操作约定假设，
用户未另外确认整段无操作。

boot 状态始终 KeepaliveDegraded，不代表 heartbeat 健康；采样证明 App 得到调度，
实际 AX RPC 证明终点可用，不证明整段持续 RPC 负载健康或无限期保活。
本轮不证明 Mac-free、手机自挂 DDI、VPN 恢复或产品 API；VPN 开关操作仍待验收。
原始证据保存在本机 `/tmp/iphone-selfrunner-acceptance/` 的
`background-end-status.json` / `background-end-log.txt` / `background-analysis.json` /
`background-end-read.txt` / `background-final-log.txt`，未外发手机日志或界面。

脱敏事实草稿 workbuddy rc=0、`.provider=workbuddy`、无顶替；主会话修正第 1 组
E1 耗时（733 ms 是更早实验）和 bundle unknown「未核实」措辞，后者已实际核实。
终点脱敏事实草稿 workbuddy rc=0、`.provider=workbuddy`、无顶替；主会话核实
严格窗口采样数并补上截图 / synthesis / 操作总耗时。

### S0c — VPN 关闭负对照与恢复进展 (2026-10-12)

用户报告已关闭 LocalDevVPN。00:20:27Z 查询旧 run 时，邮箱因状态过期被拒；
原始 status 时间停在 2026-10-11T22:00:06.865099Z，当前 devicectl 进程清单不含
PID 4863 / SelfRunnerSpike。退出原因与 VPN 切换时刻均未知，不能认定是这次
关闭 VPN 导致退出，也不能拿已退出的旧进程验收活跃会话断线。此前 30 分钟
后台与终点 RPC 结果保持独立。

旧 run 追加日志共 3852 条 boot 采样（16:39:08Z–22:00:03Z），最大间隔 5 秒。
该描述不升级为额外后台 / RPC 验收；该段没有前后台、USB 连续在位或退出原因证据。
保存原始 stale status、进程清单与日志后，以 `--research-mailbox` 启动新进程
PID 514 / runID `5F9E74AF-67F7-42AA-98E6-6E4A7FF249CA`；旧命令正确被拒。

**VPN 关闭时启动负对照通过**：新进程 Start 实际返回 ERROR，缺少
`10.7.1.1` 的 utun；ready=false，清理后回 idle，没有进入 XCTest 会话，
没有回退到 USB 启动路径（`SelfRunnerSpike.swift:186` 的接口前置检查）。
VPN 重新开启后初次连接重试与实际 AX RPC 已通过（见下）；这不是同进程
活跃会话自动重连。原始证据保存在本机
`/tmp/iphone-selfrunner-acceptance/vpn-off-*`，未派送原始手机日志。

脱敏事实草稿 workbuddy rc=0，run 目录 `provider=workbuddy`、无顶替，
stderr 范围内修改为无；主会话核对原始数据、纠正源码引用行号并落盘。

#### 代理自行开启 VPN 与重试结果

用户要求代理自行操作后，通过标准 iphone-use runner 进入系统 VPN 页面，确认
LocalDevVPN 为已选配置，将 VPN 开关 0 → 1，实际标签变为「已连接」，
未使用 iPhone Mirroring。最初 VPN 行位于底部遮挡区域，点击未生效；滚动后
进入配置页并成功切换。标准 runner 第一实例远程连接失效，由 supervisor 恢复；
本轮因此已改变 XCTest / AX 环境，不能当冷 AX 基线。

随后释放控制 owner 并执行 `iphone-use setup stop`，专用 supervisor 与受管
进程停止；后续外部进程清单无 iPhoneUse-Runner，SelfRunnerSpike PID 514 与
Preferences PID 616 仍在。控制服务代码 / 配置未改动。

| 检查 | 标准 runner 停止后的 self-runner 实测 |
|---|---|
| 连接重试 | 同 PID 514 / runID，00:27:44Z 选择 utun4 / 10.7.1.1；RP 175 ms，daemon 13 ms，00:27:51Z ready（含 6 秒 Bonjour 等待） |
| E1 | success=1，740 ms |
| 自读 | 43 节点，Self Runner / PID 514 / selfRead=1，桥接 423 ms |
| 截图 | 326828 bytes / OK，操作总 141 ms |
| 点击 | counter 0 → 1 / SUCCESS，synthesis 313 ms |
| 跨 App 读屏 | 设置 87 节点 / PID 616 / selfRead=0 / truncated=0，桥接 417 ms，操作总 469 ms；外部核实 Preferences |

第一条 delayed read 在设置启动完成前返回自身 44 节点，不计跨 App 成功；
设置进入前台后另发普通 read，取得上述真实设置结果。bundle 解析仍 unknown。
VPN-off 失败发生于 XCTest 初始化之前，因此同进程重试成功只算初次连接恢复，
不是活跃 XCTest 会话断线后自动重连。源码遇 Disconnected / Closed 会 fail → stop，
未实现自动恢复。本次用标准 runner 辅助开关、Mac 已挂 DDI、USB 仍用于控制，
不证明 Mac-free 或 DDI 手机自挂载；此前 30 分钟无第二 runner 的验收独立保留。
原始证据保存在本机 `vpn-on-*` / `vpn-control-*`，未外发手机界面或日志。

恢复结果脱敏事实草稿 workbuddy rc=0，run 目录 `provider=workbuddy`、无顶替；
stderr 范围内修改为无。主会话修正草稿「仅两进程」与 tap 耗时歧义，核实结果身份、
准确 +1、真实跨 App 读屏；CLI 语法和 `git diff --check` 通过，本轮无代码改动。

### S1 — 只读缺口盘点 (2026-10-11)

本轮仅检查本地源码与 DDI 素材，不执行 upload / mount、不改依赖、不重启手机。
`crates/self-boot/src/lib.rs:265` 的 `ddi_status` 只执行 `copy_devices` 与
`query_developer_mode_status`；App 要求 mounted=1 / developer=1，否则提示先用
Xcode 挂 DDI。`crates/self-boot/Cargo.toml:12` 未启用 `tss` / `cryptexd`。

patch idevice rev `41c388b` 已有两条独立入口：

| 入口 | 现有代码流程 | 尚缺的真机证据 |
|---|---|---|
| `mobile_image_mounter::mount_personalized_rsd` | image / trustcache / BuildManifest / ECID → 缓存 personalization manifest 或 TSS → upload / mount | 实际挂载类型、identity、手机端签名与挂载 |
| `cryptexd::install_ddi` | RSD 每 routine 新连接 → personalization IDs → nonce → TSS Cryptex1 ticket → XPC 四 payload 与 ticket → installed_ddi | 该入口是否适配当前 iOS 27 与 DDI |

两条路径不能互相替代。本机 `/Library/Developer/DeveloperDiskImages/iOS_DDI/Restore`
版本为 `17F113`，两套素材均存在；cryptex 包含 GenericDmg、GenericTrustCache、
GenericVolume、CryptexInfoPlist。素材和函数存在不等于真机挂载已通过。
实际挂载类型、设备端 TSS 联网、签名、断线重试与 Mac 关闭后的冷启动仍未验收；
USB devicectl 会启动 Mac DDI 服务，不能用它证明 S1。

另在 `idevice/src/services/cryptexd.rs:274` 发现 `persistence` 取
`request.nonce_persistence`，而非 `request.persistence`（默认分别为 1 / 2）；
已以 aiw note 记录，协议影响未确认，本轮不修改共享依赖。
先保留 S0 现场，再单独设计两入口判别与 Mac-free 冷启动实验；S1 尚未实现。
公开源码提炼与脱敏事实草稿 workbuddy rc=0、`.provider=workbuddy`、无顶替；
主会话回源码核对。原始手机数据未派单。

### S0d — E1 后台崩溃根因与 E1c (2026-10-12，Mac 主机)

**结论：** 后台 self-runner 进程的「原因不明死亡」至少有两次是 E1 引入的自身崩溃，
不是被系统杀掉。`-[XCAXClient_iOS loadAccessibility:]` 会同时注册
`kAXAlertNotification`（1006）；此后任何 App 弹出 alert，XCTest 会为该 PID
构造 `XCUIApplication`，而本宿主没有 UI testing 配置，断言失败后 abort。
新增 E1c（AX 加载 + 只注册 4002）在 3 个新进程中既保留 0.3 秒点击，又不再因 alert 崩溃。

#### 证据

手机 systemCrashLogs（时区 UTC+7，以下已换算 UTC）：

| 报告 | 进程 | 类型 | 栈（自上而下摘要） |
|---|---|---|---|
| 10-11 16:17:08Z | PID 4429（15:24Z 启动） | EXC_CRASH / SIGABRT | `NSAssertionHandler` ← `-[XCUIApplication commonInitWithApplicationSpecifier:device:].cold.1 +60` ← `initWithApplicationSpecifier:device:` ← `-[XCUIApplicationMonitor monitoredApplicationWithProcessIdentifier:]` ← `-[XCAXClient_iOS handleAccessibilityNotification:fromElement:payload:]_block_invoke` |
| 10-12 00:43:40Z | PID 514（00:21Z 启动） | 同上 | 同上；与 status 停止时刻 00:43:39Z、日志末行 00:43:35Z 吻合 |
| 10-11 11:16–11:19Z ×5 | PID 3520–3533 | SIGBUS | `dev.iphoneuse.selfrunner.boot` 队列栈溢出（stack guard）于 `connect_rp` timeout future；早期 debug 构建，本轮未复现 |

PID 4863 于 22:00:06Z 停止，与 `LowBatteryLog`（22:00:07Z）吻合；该日志显示
testmanagerd `Developer testing(BackgroundUI)` 与本 App 的 Location subscription
断言已保持约 5 小时 21 分，SpringBoard 保持 `PreventIdleSleep`。随后 PID 从 4863
回到 514，**推测**手机电量耗尽重启；未取得关机记录，不作结论。

Xcode 26.5（17F42）iphoneos 26.5 SDK 的 XCUIAutomation（arm64 切片）反汇编：

- `cold.1 +60` 对应第一处 `handleFailureInMethod:`，描述为
  `Device is not configured for UI testing - use of XCUIApplication is not supported.`
  （第二处为 `applicationSpecifier.bundleIdentifier != nil`）。与 E0 实测
  `configuredForUITesting=false` 一致。设备二进制与 SDK 仅按符号偏移对应，
  由下面的真机复现确认。
- `handleAccessibilityNotification:fromElement:payload:` 按通知分派：
  1006 → 日志 `Handling kAXAlertNotification for %@` → 崩溃所在 block；
  1021 → `kAXPidStatusChangedNotification`；4002 → `kAXUserTestingNotification`。
- `loadAccessibility:` = `_loadAccessibility:` → `_XCTAXIPCTimeout()` →
  `_setAXTimeout:error:` → `_registerForAXNotification:error:` 依次 4002、1006、1021、3031。

#### 真机复现与 E1b / E1c

方法：`--research-mailbox` 新进程 → start → AX 加载变体 → 自读 / 截图 / 点击 →
Mac 局域网页面 `alert()` 经 `devicectl --payload-url` 在 Safari 打开 → 15 秒后查进程、
crash 报告、status，再跨 App 读屏。iPhone 14 Pro / iOS 27.0.1，USB，Mac 已挂 DDI，
LocalDevVPN 开启，签名 team <TEAM_ID>（Mac 主机 构建）。

| 变体 | 新进程 | 加载 | 点击 synthesis | alert 后 |
|---|---|---|---|---|
| E1（另一台 Mac 的旧构建） | PID 959 | 726 ms | — | 05:06:40Z 崩溃，栈同上 |
| E1（新构建，对照） | PID 1045 | 726 ms | 0 → 1 | 05:38:58Z 崩溃，栈同上 |
| E1b：不注册通知 | PID 1012 | 438 ms，timeout 60 s | **5370 ms**，0 → 1 | 存活；后台读 Safari 60 节点含 `selfrunner alert probe` |
| E1c：只注册 4002 | PID 1015 / 1027 / 1036 | 546 / 424 / 422 ms | **311 / 316 / 316 ms**，均 0 → 1 | 3/3 存活；后台读 Safari 60 节点含 alert 文本与「关闭」 |

各进程自读 43 节点，截图 325–328 KB OK；`_registerForAXNotification:error:` 运行时
签名为 `B` / `i` / `^@`，与 SDK 一致。E1b 点击回到 E1 前的 ~5.4 秒，说明此前的
点击提速来自 4002（user testing / 事件确认）而非 `_loadAccessibility:` 本身；
崩溃来自 1006。1021 / 3031 未单独测，E1c 不注册它们。

#### E1c 30 分钟后台浸泡（05:39:46Z–06:10:04Z）

同一新进程 PID 1056 / runID `E889C723-5B43-489E-9453-94D8C39D49FA`：start → e1c 后切到
Safari，每 5 分钟以新 Safari 进程打开 alert 页（共 6 次，05:40–06:05Z），10 秒后核对进程
并后台读屏。6/6 存活，每次读到 Safari 60 节点（含 alert），App 一直在后台。结束时切回
前台：点击 0 → 1，synthesis 316 ms；截图 329568 bytes OK。窗口内 360 条 boot 采样
全部为 KeepaliveDegraded（无 Disconnected / Closed）；当天无新增 SelfRunnerSpike 崩溃报告
（仅 E1 复现与对照的两份）。条件同上（USB、Mac 已挂 DDI、VPN 开、keepLocation）。

#### 未解决与边界

- E1c 只是绕开 alert 路径；`configuredForUITesting` 仍为 false，任何其它触发
  `XCUIApplication` 构造的路径仍会 abort。bundle 解析仍 unknown。
- 不注册 1006 意味着 XCTest 自带的 UI interruption 处理不会触发；自动化层需自己从
  快照识别 alert（快照中可见，见上）。
- 截图与 alert 读屏均为 USB + Mac 已挂 DDI 条件；不证明 Mac-free。
- 代码：`SpikeBridge.m` `+loadAccessibilityVariant:log:`，mailbox / CLI 动作 `e1b`、`e1c`，
  与 `e1` 共用每进程一次。

### 判别实验：VPN 断开后 App 内 XCTest 是否仍可用 (2026-10-12，Mac 主机)

**结论（初步）：** 会话建立后，App 内 XCTest 的读屏、截图、点击**不依赖** VPN / Rust IDE 链。
Rust 侧在 VPN 断开后也**检测不到断线**：heartbeat 早已 KeepaliveDegraded，链上无流量，
状态一直不变。因此「活跃会话断线自动恢复」的前提需要重新定义：先弄清哪些操作真正需要 IDE 链。

方法：新增启动参数 `--observe-disconnect`（`SelfRunnerSpike.swift`），断线只记录、不 stop；
mailbox state 增加 `observeDisconnect` / `bootDisconnected` / `bootDisconnectedAt` / `bootState`。
VPN 用手机快捷指令「VPN 关」「VPN 开」（「设定 VPN」→ LocalDevVPN 断开 / 连接），由
`devicectl process launch --payload-url "shortcuts://run-shortcut?name=…" com.apple.shortcuts`
触发，不经任何 runner。VPN 实际状态由 self-runner 后台读 `prefs:root=VPN` 页面核对
（「VPN状态, 已连接 / 未连接」）。该页同时显示「“LocalDevVPN”将按需连接」。

| 进程 | 过程 | 结果 |
|---|---|---|
| PID 1173 | 快捷指令名写错（实际名含空格），读屏读到「未找到快捷指令“VPN关”」 | VPN 未变，作对照：read / screenshot / tap 1→2→3 全部正常 |
| PID 1178 | VPN 关（**当时未核对 VPN 状态**）→ 80 s 后 tap；VPN 开 → 20 s 后 tap | read / screenshot 正常；两次 tap synthesis 316 ms 无错误但计数 1→1 **FAIL**；截图 514 / 521 KB（其它运行 268–328 KB）；bootDisconnected 始终 false |
| PID 1186 | 同上流程 | tap 1→2、2→3 均 SUCCESS |
| PID 1186（续） | 再次「VPN 关」，Settings 连续 4 次读到「未连接」（06:27:12–41Z）；切回前台 | read 44 节点、截图 268 KB、tap 3→4、4→5 SUCCESS；之后 Settings 仍「未连接」 |

全程所有 boot 采样均为 KeepaliveDegraded，无 Disconnected / Closed，`bootDisconnected=false`。

**未解释：** PID 1178 的点击静默失败（synthesis 返回成功、无 NSError，但事件未到达靶按钮）。
截图明显偏大，**推测**当时有 App 之外的系统层覆盖（例如定位「始终允许」升级提示或通知横幅），
本 App 的 AX 树看不到它；未取得截图内容，不作结论。这暴露一个风险：synthesis 成功
不代表点击生效，验收必须继续看计数或界面变化。

**待做：** 截图落盘以便事后查看；VPN 断开下长时间后台浸泡；确认哪些操作需要 IDE 链
（新 Start、E1c、跨进程 PID 授权等）；按需连接对实验的影响。

### 哪些操作需要 VPN / IDE 链 + VPN 关闭 5 分钟浸泡 (2026-10-12，Mac 主机)

同一新进程 PID 1217 / runID `F6824A53-EEE1-497B-AA21-4E05E8C0CEDF`（`--observe-disconnect`）。
每次切换 VPN 后均由后台读 `prefs:root=VPN` 核对状态。

| 操作 | VPN 状态 | 结果 |
|---|---|---|
| Start（新进程） | 未连接 | **失败**：`未找到地址 10.7.1.1 的 utun`，回 idle（建立会话需要 VPN） |
| Start（同进程重试） | 已连接 | ready |
| 后台跨 App 读屏（E1c 前） | 未连接 | 设置 84 节点 |
| **E1c** | 未连接（06:34:37Z 核实） | success，418 ms，`_registerForAXNotification:4002` 101 ms |
| 自读 / 截图 / 点击 | 未连接 | 43 节点；328601 bytes；0 → 1，synthesis 312 ms |
| 后台浸泡 5 分钟（06:35:46–06:39:24Z，每 ~55 s） | 未连接（每次读屏时页面即显示） | 5/5 读到设置 84 节点 |
| 回前台：自读 / 截图 / 点击 ×2 | 未连接 | 44 节点；282229 bytes；1 → 2、2 → 3，synthesis 311 / 311 ms |
| VPN 开 | 已连接 | 进程仍为 1217，无新增崩溃报告 |

窗口内 65 条 boot 采样全为 KeepaliveDegraded。结论：**只有建立会话（RP → RSD → testmanagerd →
initiateSharedSession）需要 VPN**；会话建立后 E1c、读屏（自身与跨 App）、截图、点击都不经 IDE 链。
尚未覆盖：PID 授权（默认关）、同进程 Stop → Start（mailbox 无 stop 动作）、更长时间 VPN 关闭。

截图现在写入 `Documents/screenshots/<UTC>-shot-<pid>.png`（保留最近 30 张），点击判定 FAIL 时
自动补拍 `-tapfail-` 图；已真机验证 shot 文件可取回（1178×2556 PNG），tapfail 路径尚未真机触发。

### 弹窗摘要与快照落盘 (2026-10-12，Mac 主机)

读屏结果新增 `alerts` / `alertCount`（elementType 5 / 7 / 8 的节点，附子树按钮与文本），并把全部遍历节点写成
`Documents/snapshots/<UTC>-<rootLabel>-<pid>.jsonl`（i / depth / type / label / identifier / value / frame，保留 30 份）。

| 场景 | 结果 |
|---|---|
| 快捷指令运行不存在的快捷指令 | alertCount=1：type 7，label「该文件不存在。」，buttons「好」，texts 含「未找到快捷指令“NoSuchShortcutXYZ”。」 |
| Safari `alert()` | 结构为 `identifier=SFDialogView`（type 1）→ `SFDialogViewMessageText`（type 48）→ 文本（type 52）+ 按钮「关闭」（type 9）。加入 identifier 前缀 `SFDialog` 规则后：alertCount=1，`source=identifier`，buttons「关闭」，texts 含「selfrunner alert probe」 |

### 无 Mac 重建会话：App 被杀后，拔掉 USB 由手机自行重连 (2026-10-12，Mac 主机)

新增持久开关「自动会话」（`UserDefaults autoSession`，启动参数 `--auto-session-on/off` 或 UI 开关）：
App 启动后每 5 秒查 utun 10.7.1.1 → Start → E1c → 每 60 秒自检读屏（每第 5 次加截图）→ 写
`Documents/auto-status.json`。自动会话隐含 observe-disconnect。

带 USB 基线：06:55:56Z 启动 → 06:57:03Z 首次自检，E1c success。随后 devicectl 结束进程（06:57:29Z），
用户拔掉 USB。Mac 侧 06:57:48Z 与 06:58:07Z、07:00:12Z 均为 `unavailable` / `tunnelState: unavailable`，
`lastConnectionDate` 06:57:00Z，无网络回连；07:12:00Z 用户插回 USB 后才恢复 `wired`。

**Mac 不在场窗口 06:57:00–07:12:00Z 内，用户从主屏幕点开 App（无启动参数），手机自行完成：**

| 时间（UTC） | 事件 |
|---|---|
| 06:59:40 | 新进程 PID 1392 启动；选中 utun4 / 10.7.1.1 |
| 06:59:46 | Bonjour 6 s；RP connect 153 ms；`DDI mounted=1, developer mode=1`；IDE sessions；daemon session 12 ms；ready |
| 06:59:47 | E1c success，426 ms |
| 07:00:47–07:11:54 | 12 次自检全部返回（主屏幕 SpringBoard PID 37：11 次 468 节点、1 次 761 节点），554–691 ms |
| 07:04:50、07:09:53 | 后台截图 1994823 / 1995273 bytes；取回 07:09 图确认为主屏幕 |

窗口内 146 条 boot 采样全为 KeepaliveDegraded。结论：**手机未重启时，DDI 挂载会保留；App 被杀后可在
没有 Mac 的情况下自行重建会话并恢复读屏 / 截图**。触发条件仍需「有人点开 App」（或另找无人值守的启动方式，
例如快捷指令自动化）；手机重启后 DDI 失效属于 S1，仍未解决。LocalDevVPN 需处于连接状态。

### S1 P0：App 内只读 DDI 探测 (2026-10-12，Mac 主机)

方法：新增 mailbox 动作 `ddi-probe`（`Session::ddi_probe`，`ipu_boot_ddi_probe_json`）。复用 start 后的 RP/RSD 连接，每个请求都新建连接，并单独设 30 s 超时。全程只读，不调用 upload/mount/roll/install/TSS。标识、nonce 和 manifest 只输出类型、长度和 sha256 的前 8 位 hex；idevice 的 debug 日志在探测期间静音。DDI 当前处于已挂载状态，VPN 已连接。探测连跑 2 次，均 rc=0，两次结果一致。

结果：
- **RSD**：解析出 85 个服务。`com.apple.mobile.mobile_image_mounter.shim.remote` 存在，Features 为空。`com.apple.security.cryptexd.remote` 存在，ServiceVersion 3，Features 为 `CryptexInstall, Cryptex1, ReadIdentifiers, Cryptex1,UseProductClass, InstallEvents`。idevice 的 RsdHandshake 不保留原始服务表，因此 parser 丢弃了多少条目仍未知。
- **mounter**：CopyDevices 有 1 条，被识别为 developer 镜像。条目里有 `PersonalizedImageType` 和 `PersonalizedImageVersion` 等键，但没有 `ImageType`。Developer Mode 已开启。personalization identifiers 共 9 个键，包括 `UniqueChipID`、`ChipID`、`BoardId`、`SecurityDomain` 和 `Ap,SikaFuse`。`DeveloperDiskImage` 的 nonce 长 48 字节。
- **签名缓存**：CopyDevices 里没有 48 字节的数据字段，所以本次没有查询任何 manifest。要查询设备侧缓存，需要另外传入 personalized Image.dmg 的 SHA-384（P1 素材打包后才能做）。
- **cryptexd**：read identifiers 返回 19 个 `img4_chip_*` 整数，包含 ecid 和 cpro，补上了计划中「缺 ECID/cpro」的疑点。copy-installed 返回的 `remote-cryptex-array` 存在，含 1 条记录（推测就是当前的 DDI）。Index 域 2 的 nonce 长 56 字节，需要 unwrap 才能得到真正的 nonce。Handle 域需要 Manifest handle，本次未查询。
- 连续两次探测 nonce 摘要不变，说明只读查询不会滚动 nonce。

结论：在 DDI 已挂载、App 内走 RSD 的条件下，两个入口的只读能力都可用。冷启动（DDI 未挂载）时这两项服务是否还会发布，仍需在 P2 重启窗口验证（计划步骤 ⑦）。

### S1 P1：手机经 HTTPS 自取 DDI ticket（mounter 路线，不挂载）(2026-10-12，Mac 主机)

方法：新增 `ddi-ticket` 动作（`Session::ddi_ticket`）。素材是 Mac 主机17F42 的 iPhone15,2 identity「Customer iOS Developer PDI」，包括 PersonalizedDMG（11,452,141 B）、LoadableTrustCache 和 BuildManifest。素材由 `scripts/research-stage-ddi.sh` 解析 BuildManifest 后复制到 `Resources/DDI/`，打进 App，不入库。

TSS 请求在 `crates/self-boot/src/tss.rs` 自建：从 idevice 41c388b 移植，单测断言与 idevice `TSSRequest` 构造出的字典一致。传输也是自写的：
- 只连 `gs.apple.com:443`，信任锚只有内嵌的 Apple Root CA（SHA-256 `B0B1730E…F024`，已单测）。
- 不跟随重定向，响应上限 4 MiB，不回退 HTTP。
- 不调用 idevice 的 `TSSRequest::send` / `idevice::http`，因为前者用明文 HTTP 并打印完整请求。

返回 JSON 和日志中只有长度、摘要前缀和阶段耗时。

过程与结果：
- 第 1 次失败，报 `Tls`。gs.apple.com 的证书链为 gs.apple.com → Apple Server Authentication CA → Apple Root CA，而 Apple Root CA 不在 webpki-roots 里。改为只信任 Apple Root CA 后握手通过。
- 第 2 次失败，报 60 s 整体超时。测试网络较慢：Mac 上 curl 发一个空请求，首字节也要约 4.4 s。改为分阶段超时（首字节 90 s）并记录各阶段耗时。
- 第 3 次成功：请求 1,501 B；connect 1,334 ms、TLS 1,035 ms、首字节 963 ms；响应键为 `@ServerVersion`、`ApImg4Ticket`；ticket 2,984 B。ticket 存入 App 的 `Application Support/ddi-ticket/`，权限 0600，排除备份，文件保护为 completeUntilFirstUserAuthentication，不进 Documents。
- 设备签名缓存查询（`QueryPersonalizationManifest`，hash 为镜像的 SHA-384）返回 `InternalError`，不是 NotFound。脱敏后的 DetailedError 为 `com.apple.MobileStorage.ErrorDomain Code=-2 "Failed to copy personalization manifest."`，下层是 `Failed to retrieve personalization …`，含义是设备上没有该镜像 hash 的签名缓存，等同于未命中。pymobiledevice3 和 idevice 遇到这种情况都会转去请求 TSS。当前已挂载的 DDI 是 Xcode 挂的，设备上没有我们这份 17F42 Image.dmg 的缓存。可能是 Xcode 用的镜像不同，也可能是设备不保留缓存；未进一步区分。因此 S1 重启后的自挂按「必须联网请求 TSS」设计，设备缓存只作可选的快速路径。复测：TSS 再次成功，ticket 2,984 B，首字节 4,187 ms。
- 风险：重启后自挂需要实时 nonce 和联网 TSS（除非设备缓存可用）。网络差时，这一步决定恢复耗时。

### 安全边界变更：`tap-offer` (2026-10-12)

为验证 App Store 应用重装，新增并保留 `tap-offer`。它先读屏，要求前台为 App Store；快照中必须恰有一个 `AppStore.offerButton[state=get|redownload|update]`，然后对该元素中心合成一次点击，目标 PID 为 App Store。其它情况一律拒绝，从不点击购买类按钮。这是 self-runner 首次跨 App 合成点击。

实测：从 redownload 状态点击一次后，按钮依次变为 downloading、open，测试应用安装完成。这同时证明了：会话建立后，后台 self-runner 能向其它 App 合成有效点击。

### S1 P2 真机：重启后 DDI 仍处于挂载状态，自挂路径未触发 (2026-10-12，Mac 主机)

准备：Self Runner 以 `--auto-session-on --auto-mount-on` 启动（两项写入 UserDefaults）；`ddi-mount` 在已挂载时实测返回 `skipped/AlreadyMounted`（19 ms，无写操作）。

**第 1 次**（拔 USB、退出 Xcode，手机仍连测试 Wi‑Fi）：手机重启后，用户解锁、开 VPN、点开 App。App 新进程 pid 471（旧进程 1869），10:43:33Z 检查时 `DDI mounted=1`，于是跳过挂载，直接建会话，E1c 加载成功，读屏正常，出现「Automation Running」。Mac 日志显示，17:41（本地 UTC+7）remotepairingd 通过 Wi‑Fi 重新连上手机，17:42:08 有一次来源不明的 `devicectl` 调用，不是本会话发的。因此不能排除 DDI 是 Mac 挂上的，本次作废。

**第 2 次**（同样拔 USB；用户在重启后把手机切到另一部手机的热点，手机地址 <lan-ip>，与 Mac 网络隔离）：
- Mac 侧时间线：17:52:17 手机消失（重启）；17:52:44–17:54:01 remotepairingd 通过测试 Wi‑Fi 与手机有一条 TCP control channel（手机重启后先连回了原 Wi‑Fi），但**没有建立 tunnel 和 RSD**，也没有任何 `devicectl` 调用；17:54:01 起手机不可达（已切到热点）。
- App 侧：新进程 pid 516，10:54:13Z RP 经 VPN 连上，`DDI mounted=1, developer mode=1`，10:54:16Z ready，随后自检读屏正常（例如读到「设置」）。

结论（推断）：手机重启后没有任何 tunnel、RSD 或 devicectl 能把 DDI 挂上，DDI 却仍是挂载状态。因此 iOS 27.0.1 上，Xcode 挂载的 personalized DDI **很可能跨重启保留**。这与「重启后需重新挂 DDI」的原假设相反。

影响：
- 只要 DDI 已挂过一次，S1「重启后自挂」可能根本不需要：重启后用户点开 App，会话就能自行重建（两次重启均成功）。
- App 自挂路径（`ddi_mount` 的写操作部分）仍**未经真机验证**：要等 DDI 真正未挂载时才会触发。本研究不主动 unmount（风险边界），所以只能等 DDI 自然失效时再验证，比如系统更新或 Xcode 换版本。
- DDI 何时失效、是否有有效期，仍未知。
