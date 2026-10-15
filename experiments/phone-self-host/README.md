# iPhone 自主被控探索

2026-10-11，基于 `8976975`。目标：iPhone 不连接 Mac，仍能被另一台设备看屏和操作。

**验收结论：iPhone 17 Pro Max（iOS 27.0）在首次 USB 安装、配对和 VPN 授权后，可在 Wi-Fi 下脱离 Mac 托管，独立点击、输入、读屏和截图。后台维持和完整重启后的恢复均已通过；重启后首次密码解锁仍由人完成。**

2026-10-11 夜间至 10-12 真机续测：iPhone 17 已交接，独立 App 0.2.0 和独立 runner 已安装。原 VPN 未连接、选中 Shadowrocket。用户在手机上完成实验 VPN 授权后，网络扩展通过 `createTCPConnectionThroughTunnel` 收到合法 RemotePairing 握手（wire protocol 26，21 ms）。新建配对返回 `pairing_approval_failed`；新增数字错误码后复测为 I/O 错误（域 1、子码 0），不能据此认定系统拒绝授权。尚未取得配对记录或验证 RSD，正常 Mac runner 没有暂停。详见 [首次真机报告](evidence/ios27-native-vpn-first.json)和[配对诊断报告](evidence/ios27-native-pairing-diagnostic.json)。

新增「导入首次配对记录」入口：只接受 RemotePairing 格式，限制 64 KiB，检查 Ed25519 公私钥一致、UUID 和可选 IRK 长度，保存到设备本地共享 Keychain。普通 lockdown 配对文件不能直接导入。Debug 构建的 `--selfhost-import-pair` 只读取 App Documents 中固定的 `pair-record-import.plist`，成功保存后删除临时文件。导入路径已构建并通过合成格式测试；真实首次配对与导入结果见下文。

后续 USB/lockdown 首次配对成功，记录已导入 Keychain。手机内 RSD 验证读取到 iOS 27.0，开发服务可用；暂停正常 Mac supervisor 后，手机内扩展启动了独立 XCTest 会话，`test_plan_started=true`。见 [RSD 报告](evidence/ios27-native-rsd.json)和[启动报告](evidence/ios27-native-launch.json)。首次控制曾被旧 runner 的凭据拒绝；加入 `KillExisting` 和新凭据的实际服务验证后，已通过真实输入、读屏和截图验收，见下方独立控制记录。

首次 USB 配对工具仅用于安装配置阶段：在仓库根目录运行 `~/.local/bin/heavy-cargo build --release --manifest-path experiments/phone-self-host/launcher/Cargo.toml --bin ipu-phone-host --features usb-provision`，再执行 `experiments/phone-self-host/launcher/target/release/ipu-phone-host --usb-pair <UDID> --pair-out <私有目录/记录.plist>`。它只选择指定的 USB 设备，使用已信任的 lockdown 服务，不输出密钥，记录以 0600 创建且不覆盖旧文件。Debug 的 `--selfhost-export-control` 可为本机验收导出两个随机测试令牌到受保护文件；验收后用 `--selfhost-delete-control-export` 删除，文件及令牌禁止进入报告或 git。`--selfhost-stop` 停止独立服务和实验 VPN，用于恢复正常环境。

2026-10-11 后续构建使用独立 App 和独立 runner，不改变原项目的其他会话。最初准备时 iPhone 17 Pro Max 离线；当时空闲的 iPhone 14（iOS 27.2）外部 Wi-Fi 启动返回 `wifi_automation_refused`，要求 USB 授权。该结果属于原有 Mac 启动路径，不能代替手机内隧道的验收。没有反复重试 Wi-Fi 启动。

## 最初网络探测（历史对照）

设备为 iPhone 17 Pro Max，iOS 27.0。探测程序是普通开发签名 App，独立 bundle ID `com.leeguoo.iphone-use.selfhost-probe`，不使用 XCTest 权限。以下最初网络探测期间原有 runner、Mac USB 测试会话保持运行。后续停止 Mac 托管服务后的独立验收另有记录。

| 连接方式 | 端点 | 结果 | 能说明什么 |
|---|---|---|---|
| 手机 App → 回环地址 | `127.0.0.1:62078` | `EPERM` | 普通 App 的本机 lockdown TCP 连接被拒绝 |
| 手机 App → 回环地址 | `127.0.0.1:49152` | TCP 可连接，RemotePairing 握手后 `ECONNRESET` | 端口连通不足以证明开发服务可用 |
| 手机 App → 自己的 Wi-Fi 地址 | 自身 IP 的 62078 / 49152 | 分别 `EPERM` / `ECONNRESET` | 改用自身 Wi-Fi IP 没有解决问题 |
| 手机 App → 现有 runner / daemon | `127.0.0.1:8100` / `:8787` | TCP 可连接 | 已启动服务能被同机 App 访问；未验证鉴权或完整 API |
| Mac → 手机 Wi-Fi 地址 | 手机 IP 的 49152 | 收到 RemotePairing 握手，协议版本 26 | 同一握手格式在外部网络路径上有效 |
| Mac → USB 转发 | 49152 | 对端关闭连接 | USB 转发也未通过，不能用它作正对照 |

手机 Wi-Fi 地址由探测 App 的 `getifaddrs(en0)` 取得，Mac 正对照使用相同 IP。报告仅保留结果、时间、IP 和协议版本，不保留设备标识或原始握手内容。

- [手机回环协议探测](evidence/ios27-loopback-handshake.json)
- [手机自身 Wi-Fi 协议探测](evidence/ios27-own-lan-handshake.json)
- [Mac 局域网正对照](evidence/ios27-network-handshake.json)
- [USB 对照](evidence/ios27-usb-handshake.json)
- [最初仅测 TCP 的报告](evidence/ios27-no-vpn.json)：62078 当时被归为超时，细节已显示 EPERM；后续版本将其明确标为 `tcp_denied`。

这些最初探测只覆盖本机网络入口和未认证协议握手。外部 LocalDevVPN 没有安装或测试；后续改为 App 自带本地网络扩展，已通过独立控制验收。观察到的重置不能用于断言所有 iOS 版本都禁止同机开发连接，也不能单独确定系统拒绝的内部原因。

## 独立控制验收记录

- [USB 连接时控制验收](evidence/ios27-independent-control-usb-attached.json)：确认独立会话实际执行点击、输入、读屏和截图。
- [拔掉 USB 后控制验收](evidence/ios27-independent-control-usb-detached.json)和[Mac 托管停止观察](evidence/ios27-mac-detachment-observation.json)：usbmux 仅剩 Network 连接，正常 Mac 服务停止，控制仍通过。Mac 在此仅作为 Wi-Fi HTTP 客户端。
- [后台观察](evidence/ios27-background-observation.json)和[后台后控制验收](evidence/ios27-independent-control-background.json)：Calculator 前台、控制 App 后台 160.9 秒，独立会话持续可用，回到测试 App 后控制通过。
- 正式 runner 使用稳定嵌入库 `69829e8`（v0.17.18），`/agent/status` 返回 `drivable=true`。[标准批次 ACK](evidence/ios27-standard-agent-batch.json)报告完成 5 步、应用 3 个动作，但最后的验收按钮未实际触发，页面仍显示未验证。刷新 AX 后单独点击按钮，生成新的[Wi-Fi 重连后控制报告](evidence/ios27-wifi-reconnect-control.json)，真实点击、输入、读屏和截图通过。批次 ACK 不能代替动作后置条件。

- [正式 v0.17.18 拔线验收](evidence/ios27-formal-control-usb-detached.json)：真实点击、输入、读屏、截图通过，2.16 秒。
- [冷启动观察](evidence/ios27-cold-restart-observation.json)、[原生会话状态](evidence/ios27-cold-restart-runtime.json)和[冷启动后控制报告](evidence/ios27-cold-restart-control.json)：USB 拔掉、正常 Mac daemon 停止，完整重启后由用户首次解锁；手机自行恢复至 ready，通过约 2.12 秒的实际控制验收。重启到验收通过之间未通过 Mac 启动 App、安装、挂载开发镜像或启动 XCTest；通过后才复制报告。
- [停止与重启观察](evidence/ios27-stop-restart-observation.json)：停止后实验 runner 和网络扩展进程退出，随后重新 ready，没有手动杀进程。此项由 Mac 仅启动控制 App 的 Debug 参数触发，手机 App 与扩展实际执行停止和启动；独立冷启动结果另列。
- [环境还原记录](evidence/ios27-normal-environment-restored.json)：正常 Mac v0.17.19 ready、owner=null，手机嵌入 daemon serving；Shadowrocket 重新选中且两个 VPN 开关均断开，实验凭据导出已删除。

源码检查：Swift 协议与安全配对导入测试通过；原生 Rust 使用 `heavy-cargo test --release --manifest-path experiments/phone-self-host/launcher/Cargo.toml --features usb-provision`，4 项测试通过；App 和正式 runner 的深度签名检查通过。报告 JSON 未包含控制令牌或配对私钥。

第一轮[重启失败记录](evidence/ios27-cold-restart-failed.json)为 `bridge_ack_failed`；用户随后确认手机尚未连接 Wi-Fi。传输错误现已加入退避重试，但此轮不能用来断言单一原因。Wi-Fi 连接和手机 VPN 重连后恢复，第二轮 Wi-Fi 下冷启动通过。

本轮没有覆盖蜂窝切换、公网跨网访问、签名过期或长期待机。首次安装、签名和 USB 配对需要电脑；运行期间由手机自身维持会话。下方待办保留扩展测试范围，不代表上述已通过项目仍未完成。

## 已有代码能复用的部分

- `runner/` 负责看屏、触摸、输入，并能链接 `crates/ios-embed` 在手机内运行完整控制服务。
- `crates/legacy-launch` 已实现 iOS 15/16 的 lockdown、heartbeat 和 XCTest 启动。当前实现是外部进程，它必须在 runner 生命周期内持续运行。
- 依赖的 `idevice` 固定在 `41c388baef0dd55c0ce4924789abbfde889d9b87`，包含 RemotePairing、软件 TCP 隧道和 RSD 服务发现。
- 原版本的 `XCUITestService::new` 接受 `IdeviceProvider`。`run` 从 lockdown 读取版本，现代路径经 `CoreDeviceProxy` 创建 RSD 栈。它没有直接接收现成 RemotePairing adapter + RSD handshake 的公开入口；不能只把手机内隧道传给现有启动器。

源码入口：`crates/legacy-launch/src/lib.rs`、`crates/ios-embed/src/lib.rs`；依赖源码 `idevice/src/services/dvt/xctest/mod.rs` 中的 `run`、`connect_rsd_stack_once`，以及 `remote_pairing/{mod,socket,tunnel}.rs`。

## 当前实现

- `Shared/LocalPacketReflection.swift`：只反射 `10.7.1.1 ↔ 10.7.0.1` 的 IPv4 包；默认路由排除在隧道外。网络扩展来自本项目的独立 bundle，引用方案保留 LocalDevVPN/StosVPN 的版权和署名。
- `Tunnel/ScopedTunnelBridge.swift`：本机随机端口、随机鉴权令牌，仅允许连接手机自己的虚拟地址；原生 Rust 通过该桥使用 `createTCPConnectionThroughTunnel`，不会依赖网络扩展普通 socket 的默认路由行为。
- `launcher/`：静态 Rust 库，配对、验签、TLS-PSK、RSD、读取真实设备信息、查找独立 runner 并维持 XCTest 会话；状态不包含配对记录或控制令牌。停止时失效旧会话，旧异步任务不能覆盖新会话状态。App 指令、后台恢复和隧道停止使用同一串行队列；用户停止后，即使 Keychain 写入失败，本次扩展生命周期也不会自动重启会话。
- `patches/idevice-rsd-xctest.patch`：固定版本的两项补丁，增加现成 RSD 隧道的 XCTest 入口和按文件分块上传开发镜像的入口。只修改生成的隔离依赖，不修改全局 cargo 缓存或上游仓库。
- `Shared/ProtectedStore.swift`：配对记录和控制凭据使用共享 Keychain、`AfterFirstUnlockThisDeviceOnly`。每次独立 runner 启动重新生成请求签名令牌；启动被拒绝不会改动当前会话的令牌。
- `Tunnel/PhoneTunnelProvider.swift`：原生会话运行在网络扩展内，普通控制 App 切到后台不负责保活；传输中断使用有上限的退避，配对或授权错误等待人处理。后台维持和 Wi-Fi 下首次解锁后的冷启动恢复已实测。
- `launcher/src/image.rs`：缺少开发服务时，按硬件选择镜像、申请 Apple 个性化签名、64 KiB 分块上传、挂载后刷新 RSD。开发镜像来自用户本机的 Xcode 安装，进入私有构建资源，不进 git 或公开发行资产。
- `Sources/ControlSmoke.swift`：在手机上实际发送签名 HTTP 请求，观察自己的按钮计数、输入内容、无障碍树和真实截图。验证结果保存为 JSON，图片不写入报告。该程序不是模拟 runner，必须有真实独立 runner 才能通过。

独立 runner 的 bundle ID 为 `com.leeguoo.iphone-use.selfhost-runner.xctrunner`，控制/MJPEG/daemon 端口分别为 18100/19100/18787。检测到原有 8100 自动化端口仍在运行时，独立启动会拒绝抢占。

## 真机验收待办

1. 测试手机启用 App 中的「本机开发网络」后，用「检测指定地址」测 `10.7.0.1:49152`。第一项验收是收到合法 RemotePairing 握手，而非仅显示 TCP 连接成功。此步骤会启用手机 VPN 配置，需要核对原有 VPN 的使用情况并保留恢复方法。
2. 通过后，使用保存在设备本地的 RemotePairing 记录建立隧道，验证真实 RSD 服务响应。旧式 lockdown 配对记录和 RemotePairing 记录不是同一格式。真实配对记录不进入仓库、日志或报告。
3. 通过已补充的 RSD 入口测试启动独立 runner。在现有 runner 的占用被正常释放前，不能开启第二个 XCTest 自动化会话。
4. 验证控制 App 进入后台后的会话维持、停止与恢复。会话已移入自有 Network Extension；签名构建通过仍不能证明后台生命周期有效。
5. 最后验收 Mac 断网后持续看屏和操作，再覆盖锁屏/解锁、Wi-Fi/蜂窝切换、VPN 重连、进程被结束、重启后在手机上手动恢复。普通设备重启后的首次解锁仍由人完成。

没有通过第 1、2 项前，不开发公网中继和设备列表。隧道成立后，也不能据此宣称可以无人值守或作为普通 App Store App 提供全系统控制。

## 复现探测 App

需要 Xcode、iOS 17.4+ SDK、`xcodegen` 和 Rust iOS target。首次安装和签名在电脑上进行；目标是安装后不依赖电脑维持控制。签名团队默认沿用本仓库设置，可以在构建时覆盖 `DEVELOPMENT_TEAM`。

```sh
python3 experiments/phone-self-host/prepare-dependency.py
~/.local/bin/heavy-cargo test --manifest-path experiments/phone-self-host/launcher/Cargo.toml
./experiments/phone-self-host/launcher/build.sh
./scripts/build-ios-embed.sh
python3 experiments/phone-self-host/build-runner.py
./experiments/phone-self-host/test.sh
./experiments/phone-self-host/build.sh unsigned
./experiments/phone-self-host/build.sh signed

xcrun devicectl device install app --device <UDID> \
  experiments/phone-self-host/build/Build/Products/Debug-iphoneos/PhoneSelfHostProbe.app
```

安装两个独立 App 后，打开 Self Host Probe，依次启用本机开发网络、配对本机开发服务、验证本机隧道。第一次 VPN 配置及系统授权由手机上的人完成。启动独立被控服务前正常暂停原 Mac runner；不要启动第二个 XCTest 会话。

普通网络探测可选择「检测本机端口」「检测自身 Wi-Fi 地址」或输入虚拟网络地址后选择「检测指定地址」。指定网络地址可能触发系统本地网络权限提示。报告保存在 App 的 `Documents/self-host-probe.json`，也可以在 App 中导出。

```sh
xcrun devicectl device copy from --device <UDID> \
  --domain-type appDataContainer \
  --domain-identifier com.leeguoo.iphone-use.selfhost-probe \
  --source Documents/self-host-probe.json --destination /tmp/self-host-probe.json

# Mac 正对照只发送未认证握手，不读配对记录。
python3 experiments/phone-self-host/mac-control.py --host <手机Wi-Fi-IP>
python3 experiments/phone-self-host/mac-control.py --udid <UDID>
```

支持 `--probe-on-launch` 自动进行无认证网络探测。Debug 构建可用 `--selfhost-pair` / `--selfhost-probe` / `--selfhost-launch` 执行指定操作；没有公开 URL scheme 自动启动控制。

只有用户明确点选配对或启动按钮时才申请配对/自动化会话。无认证探测仍不读凭据、不启动 runner。测试覆盖拆包、无效帧、错误方向、报告脱敏、局部包反射、错误包拒绝、独立 Python HMAC 答案、配置凭据检查和镜像路径隔离。最新真机进展见本文开头；通过项目和未验收项目须分别报告。

运行状态与控制验收报告分别为 App 的 `Documents/self-host-runtime.json` 和 `Documents/self-host-control-smoke.json`。`phase=running` 不能替代输入和截图验收，也不能替代 Mac 断开观察。

## 协议与参考项目

- [idevice](https://github.com/leeguooooo/idevice/tree/41c388baef0dd55c0ce4924789abbfde889d9b87)：MIT，探测报文依据其公开协议实现独立编写。保留其许可见 `THIRD-PARTY-NOTICES.txt`。
- [StikDebug](https://github.com/StikDebug/StikDebug/tree/775fba15f3fbadc18f7f1444dc6ffb1566adaa4c)：已有同机开发服务连接方案；它的 README 要求配对记录和 LocalDevVPN。这里只用于调研，没有复制其 AGPL 代码。
- [LocalDevVPN](https://github.com/jkcoxson/LocalDevVPN/tree/af3fd697803ada4ac2b8d518358f5ab0a534844c)：可测试的虚拟网络工具，引用其本地地址反射方案，版权与自定义许可证见 `THIRD-PARTY-NOTICES.txt`；未安装外部工具。
- [Apple 后台执行说明](https://developer.apple.com/documentation/uikit/extending-your-app-s-background-execution-time)：普通后台任务提供有限执行时间。
- [Apple Network Extension 用途说明](https://developer.apple.com/documentation/technotes/tn3120-expected-use-cases-for-network-extension-packet-tunnel-providers)：自有 Network Extension 方案还需验证，不能把网络扩展当成任意后台服务。

## 2026-10-12 语音验收与音色接入

麦克风停止却显示已连接的问题已修复。手机记录显示音频引擎运行、识别收到事件；用户确认能回复、能操作，但任务报 API 错误。[失败状态](evidence/iphone14-voice-task-failed.json)保留任务失败与音频正常的区别，不包含原始语音、转写或密钥。相关模拟器回归 35 项通过；后续本地音色调整另经设备构建验证。

本地朗读优先使用手机已安装的高质量普通话音色，语速参数为系统默认的 1.15 倍。改进版已安装到 iPhone 14，用户试听确认“语速合适，音色仍机械”。这不构成音色质量通过。

后续准备接入[豆包双向流式语音合成](https://docs.volcengine.com/docs/DoubaoVoice/bidirectional-streaming-text-to-speech-websocket?lang=zh)。目前用户没有该服务授权，云端功能尚未实现、启用或计费。现有 `AssistantVoiceAudio` 协议提供开始、暂停、恢复、播报和立即打断入口；接入时保留原有本地识别与控制，云端播报沿用 PCM 播放及 generation 校验，避免打断后播放旧音频。手机直接连接语音服务，不依赖 Mac；云端音色需要互联网。授权保存在 Keychain，不写入日志或仓库；断网和服务失败回退到本地音色，并显示当前使用的引擎。

接入验收需覆盖首包延迟、中文音色试听、连续对话、播报中打断、断网回退、旧音频丢弃及 API 错误；未经真机验证不宣称自然音色已交付。

### 模型选择

从 iPhone 14 读取的非密钥配置为 Zen / big-pickle 在前、Kilo / free 在后，两者原先均启用。原有执行链只返回最后一次失败，因此 Kilo 报错掩盖了主模型的失败。更新增加主模型选择和“失败时允许使用备用模型”开关，默认关闭跨服务切换，旧配置保留原有列表与顺序。模型选择规则 11 项、语音及统一助手 10 项回归通过；设备签名构建与安装成功。语音页面的“模型设置”可直接进入选择；开启备用后才发送整个模型列表。合成文本向 Zen / big-pickle 请求得到 HTTP 403，该桌面请求不能替代真机请求结果；模型可列出也不代表请求可用。

## 2026-10-12 私有模型与读屏修复

- 自定义模型地址由用户填写，密钥留在手机 Keychain。私有地址、授权值、原始屏幕及网络名称不进入源码和验收证据。
- 自有网关的指定工具选择对象异常、流式工具结果丢失已修复；网关 35 项测试及实际合成工具调用通过。该通用修复不包含私有服务配置。
- 原生任务进程的外网 DNS 失败已真机复现。新增手机内模型转发：仅将该任务已选择的模型请求转到 App 的 URLSession，原生控制仍走手机上的 iphone-use。转发只监听回环，随机路径和原模型授权共同限制访问，目的地固定为所选服务；已知任务结束后释放映射，不持久化转发地址。当前真机证明覆盖 App/通话进程存活期间，不代表 App 被结束后仍能联网推理。
- 连续手机阶段合为一个原生任务，避免逐阶段自动返回导致原页面丢失。返回由执行服务在核验后处理，模型不再为了返回助手提前 BACK/HOME。
- 增加只读 inspect 工具及明确的读屏语音指令，调用 `/agent/elements`，只播报当前可见标签，不读取编辑框内容；没有实际文字就报读取失败，不猜测页面。
- 语音页显示任务阶段与错误；失败结果保存到加密任务记录，锁屏实时活动保留失败提示。普通 iPhone 14 没有灵动岛，需在锁屏验收实时活动；灵动岛需在支持的机型上另测。
- [私有模型控制与读屏记录](evidence/iphone14-private-model-read-control.json)：自动诊断从统一助手提交只读手机任务，真实 LAUNCH/DONE 通过；屏幕树独立核对了设置标题及五个可见项目。没有执行修改设置的工具。该结果不代替新修复后的真实中文语音入口验收。
- 本轮 44 项 iOS 回归通过。真机冷重启恢复、长时间后台、改进后的真实语音入口、自然音色仍未全部验收，不宣称整项完成。

后续真人另测了非诊断任务，其输入动作返回 `outcome_unknown / retry_safe:false`，没有消息发送成功证据，不能算通过。App 增加立即请求停止和保留结果核对状态；原生循环增加不再继续执行的保护。App 相关追加 19 项回归通过，原生保护的构建和设备部署结果单独记录。自动诊断只跟随自身的任务 ID，避免真人换任务后覆盖原有只读验收记录。当前材料不包含该真人任务的联系人或消息内容。

原生 `uncertain_action_requires_review_instead_of_replay` 测试通过（1/1），但尚未重建并安装 iOS runner。最新 App 保护及活动清理版本已完成设备签名构建；2026-10-12 安装返回 CoreDevice 4016，手机当前不可连接，未部署该新增版本。最后已部署的只读设置任务通过；后续新增保护不能作为已在真机生效的结论。锁屏活动授权已启用，但观测到多个活动对象，旧卡片清理仍待新版本真机验收。返回助手的界面观察包含真人后续失败任务，不据此宣称完整语音成功。
