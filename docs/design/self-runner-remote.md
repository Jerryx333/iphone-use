# Self Runner 远程控制：手机 App 接受 Mac 主机 / 调用方指令（设计稿）

状态：草案（2026-10-12）。目标：手机上装一个 App，由它控制手机；Mac 主机或调用方（例如本机 agent）给它发指令，不需要 USB。手机与 Mac 主机处于同一局域网，使用免费 Personal Team 签名。

## 1．背景与已有能力

Self Runner（`spikes/self-runner-app`，分支 `explore/self-runner`）已能：
- 在手机上自建 XCTest 会话（经 LocalDevVPN 走 RP/RSD，见 `crates/self-boot`）；
- 读屏（AX 快照）、截图（daemon PNG）、合成点击：现在只允许点自身靶按钮，或 App Store 免费 offer（`tap-offer`）；
- 重启后由用户点开 App，自动重建会话；会话建立后关 VPN 不影响使用。

缺的是**不走 USB 的指令通道**和**足够的动作集**。现在的指令通道是 USB 文件信箱（`ResearchMailbox`），保留作调试与配置下发。

## 2．架构

```
本机 agent / CLI / 调用方服务端
        │  本机 HTTP 127.0.0.1（Bearer token）
        ▼
phone-gateway（Mac 主机常驻，Rust，新 crate crates/phone-gateway）
        ▲  TLS 1.3 over TCP，长度前缀 JSON 帧；手机主动连入
        │
Self Runner App（手机，前台或后台保活）
```

- **手机主动连 gateway**，手机端不开监听端口。gateway 监听 Mac 主机局域网地址的一个端口（默认 7381），手机与 Mac 主机在同一 Wi‑Fi。
- 选 TLS+TCP，不选 WebSocket：Cargo.lock 已有 rustls/tokio-rustls/hmac/sha2，不用引入新依赖；Swift 端用 Network.framework 的 `NWConnection`（TLS）。
- 将来手机要离开局域网，在 gateway 前加中转即可，协议不变（不在本期范围）。

## 3．信任与认证

1. **服务端身份**：gateway 用自签证书（安装时由 `openssl` 生成，存 `~/.config/phone-gateway/`，权限 600）。手机**钉死证书 SHA-256**，不信任系统 CA 校验结果。
2. **客户端身份**：配对时生成 32 字节随机预共享密钥 PSK。连上后 gateway 发 32 字节随机 challenge，手机回 `HMAC-SHA256(PSK, "selfrunner-v1" || challenge || device_id)`；gateway 用常量时间比较。失败即断开，并对同一来源限速。
3. **配对下发**（一次性，走 USB 信箱或 devicectl over Wi‑Fi）：Mac 写 `Documents/gateway-pair.json`，内容为 host、port、证书指纹和 PSK。App 读取后**存入 Keychain**（`kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`），然后**删除该文件**。之后 Documents 里不留 PSK。
4. **本机调用方**（本机 agent / CLI）：gateway 的 HTTP 只绑 `127.0.0.1`，要求 `Authorization: Bearer <token>`。token 存 `~/.config/phone-gateway/client-token`（600）。
5. 日志不得出现 PSK、token、截图内容；读屏文本只在调用方需要时回传，gateway 日志只记动作名、耗时和结果状态。

## 4．协议

帧格式：4 字节大端长度（≤ 16 MiB）+ UTF-8 JSON。

- 握手：`{"t":"challenge","nonce":b64}` → `{"t":"hello","v":1,"device_id":..., "mac":b64, "app_version":..., "session":{"ready":bool}}` → `{"t":"welcome"}`
- 请求（gateway → 手机）：`{"t":"req","id":"<uuid>","action":"...","args":{...},"deadline_ms":N}`
- 响应（手机 → gateway）：`{"t":"res","id":"...","ok":bool,"result":{...}|null,"error":{"code":"...","message":"..."}|null,"ms":N}`
- 心跳：双方每 15 s 一次 `{"t":"ping"}` / `{"t":"pong"}`，45 s 无数据即断开重连。
- 手机状态推送：`{"t":"event","kind":"session","ready":bool,...}`（会话断开或恢复时）。

同一时间只执行一个动作；手机端收到新请求时如有动作在跑，回 `busy`。gateway 侧按 FIFO 排队，队列长度上限 8。

## 5．动作集（白名单）

| 阶段 | 动作 | 参数 | 结果 |
|---|---|---|---|
| 1 | `status` | — | 会话、VPN、前台 App、版本 |
| 1 | `read` | `max_depth?`、`target?: foreground|system` | 节点数组（type、label、identifier、value、frame、enabled），前台 bundle/pid |
| 1 | `screenshot` | `format?: png\|jpeg`，`quality?` | base64 图片、尺寸 |
| 1 | `tap` | `{x,y}` 或 `{label}` 或 `{identifier}`，`index?`、`wait_occlusion_ms?`、`expect_label?` 或 `expect_identifier?`、`expect_timeout_ms?` | 命中的元素与坐标 |
| 2 | `type` | `text` | — |
| 2 | `swipe` | `from{x,y}`、`to{x,y}`、`duration_ms?` | — |
| 2 | `press` | `home\|volume_up\|volume_down\|lock` | lock：`{locked:true}` 或 `{locked:"unconfirmed"}`；其余 — |
| 2 | `open_url` / `launch` | `url` 或 `bundle_id` | — |
| 2 | `wait_for` | `label\|identifier`，`timeout_ms` | 是否出现 |
| 2 | `unlock` | 空 args 或 `timeout_ms?`（默认 4000，上限 10000，非负整数） | `{already_unlocked:true}` 或 `{unlocked:true}` |

- 按 label 或 identifier 点击时，App 内部先读一次屏，要求**恰好命中 1 个**元素（或给出 `index`），否则返回 `ambiguous` 或 `not_found`，不猜。
- 所有 tap 先检查 SpringBoard AX：frame 与屏幕相交、包含点击点、面积小于屏幕 50% 的节点为遮挡物；排除 statusBar（type 25）及子孙、完全位于顶部 54pt 的 frame。每 500ms 重查，`wait_occlusion_ms` 默认 6000（非负整数）。顶部横幅（frame.y < 屏高/3）先等最多 `min(1500ms, wait_occlusion_ms)`，仍在就用 SpringBoard PID 向上滑一次（约 200ms）；上滑后重查最多 1500ms，仍在则继续等到原 `wait_occlusion_ms` 上限。`wait_occlusion_ms:0` 不等待自然消失，但顶部横幅仍允许上滑一次和上滑后重查；非顶部遮挡物只等、不滑。所有等待受请求 deadline 约束。仍遮挡返回 `occluded`，不点击；系统树失败或截断时拒绝点击。遮挡消失后重新读前台树并重定位 label/identifier，坐标保持原值。
- 只要本次检测到遮挡，tap 所有后续返回路径（含成功、`occluded`、`no_effect` 和失败/过期）的 result/details 都带 `occlusion: {detected_ms_into_request, waited_ms, cleared_by, swipe_attempted, relocated, occluders}`。时间以单调时钟计毫秒：前者为进入动作执行到首次检测的偏移（不含 gateway 排队），后者从首次检测到确认无遮挡或退出遮挡处理，含读树和上滑耗时，不含之后的 expect 等待。`cleared_by` 为 `auto_dismiss`（未上滑即观察到消失）、`swipe`（尝试上滑后观察到消失）或 `none`（尚未确认清除）；这是观测路径，不证明消失的因果。`swipe_attempted` 表示是否调用过上滑，`relocated` 表示是否成功重新匹配 label/identifier 并计算中心（坐标点击为 false）。`occluders` 累计本次遇到的遮挡物，按相同 frame 去重、优先保留非空 label，label 截断至 120 字。原顶层 `occluders` 保留，内容与对象内相同；无遮挡时不添加 `occlusion`。
- `expect_label` / `expect_identifier` 二选一，精确匹配；点击后每 300ms 读前台树，`expect_timeout_ms` 默认 3000，上限 15000（非负整数）。匹配返回 `expect: met`；超时返回 `no_effect`，result/details 含 `tap_sent: true`、`retry_allowed: false`，点击已发出，不得重试。未提供 expect 时 ok 仍只代表合成调用成功。所有等待受请求 deadline 约束，过期返回 `expired`；点击后的 expired 同样带点击已发出标记。
- `read {"target":"system"}` 只读 SpringBoard（同节点格式，深度上限 100、节点上限 2000），不进入前台 snapshot_id 缓存。
- 点击坐标为屏幕点坐标（与读屏 frame 同一坐标系）。
- 不提供任意代码、文件读写或 Shell 动作。

### 第 2 阶段实际参数（2026-10-12；真机效果未核实）

- `type {text}`：最多 1000 UTF-16 code units（与 gateway 一致），允许空串；向当前焦点追加，不清空。文本事件 `XCPointerEventPath initForTextInput` / `typeText:atOffset:typingSpeed:shouldRedact:`（redact=true），目标为前台 PID。不构造 XCUIApplication。不做 `no_focus` 检查（真机 AX 焦点与跨进程键盘不能证明无焦点）；读屏失败为 `read_failed`，PID 改变为 `target_unknown`。日志仅记录长度。
- `swipe {from:{x,y},to:{x,y},duration_ms?}`：屏幕点坐标、仅竖屏；时长整数默认 300，50–5000。端点必须在屏幕内（右/下边界不包含）；目标为 `remoteForeground` PID。复用触摸事件路径，横幅消除原路径仍为 200ms。
- `press {button}`：支持 `home|volume_up|volume_down|lock`；前三者通过 `XCUIDevice sharedDevice` / `pressButton:`。lock 默认走下节快捷指令策略，XCTest 锁屏入口仅保留显式实验。
- `launch {bundle_id}`：非空、≤255 ASCII 字节，仅字母数字、点及连字符；其他 App 使用 `LSApplicationWorkspace openApplicationWithBundleID:`，自身 bundle 的分支见下节。发起前记录前台 PID，每 300ms 轮询、最多 3 秒且受 deadline 约束；bundle 等于目标时 `confirmed_by: "bundle"`，否则 PID 变化且不是 SpringBoard 时为 `"pid_change"`（bundle 可为 unknown）；到时 PID 未变且 bundle 不可知则 ok + `"unchanged_unverified"`，不证明目标已在前台。成功返回 `{pid,bundle,confirmed_by}`。
- `open_url {url}`：≤2048 UTF-16 code units，绝对 URL、无空白/控制字符；http/https 必须有 host，主线程调用 `UIApplication.shared.open`，以 completion 结果判定；失败或 SelfRunner 不在前台返回 `unsupported`，details 提示「请改用 launch Safari + tap 地址栏 + type url\n」。成功后等待 300ms 返回观测到的 `{pid,bundle}`（不保证页面加载完成），等待受 deadline 约束。
- `wait_for {label|identifier,timeout_ms?}`：两种定位字段恰好一个，精确匹配；超时非负整数，默认 5000、上限 30000；立即读一次，然后每 300ms 读前台树。匹配返回 `{found:true,node}`，超时 `{found:false}`；读屏失败 `read_failed`。
- 参数不合法为 `invalid_args`；缺少 API/ABI 不匹配为 `unsupported`；调用失败分别为 `type_failed|swipe_failed|press_failed|launch_failed`；无法核对启动后前台为 `foreground_unconfirmed`。屏幕方向错误为 `unsupported_orientation`，前台 PID 不可用为 `target_unknown`。所有动作受请求 deadline 约束，过期 `expired`；写动作进入调用后失败/过期结果带 `action_sent:true,retry_allowed:false`（表示已尝试派发，不证明效果）。沿用 §11.A 执行闸、缓存与断线 unknown，不自动重试。
- 研究 USB mailbox 原协议没有 args，保持不变；`scripts/research-selfrunner-command.py --remote --args '<JSON>' <action>` 经已配对的 `phone-gateway` 调用新增动作。
- 构建与参数单测不替代 §11.F 真机验收；上述新增动作的实际效果均待逐项核实。

`press {"button":"lock","strategy"?:"auto"|"shortcut"|"hid"|"device_event"|"press_lock","shortcut_name"?:"锁屏"}`：默认 auto **只用 shortcut**。前置条件：用户在「快捷指令」App 建立名为「锁屏」的快捷指令，唯一动作「锁定屏幕」；自定义名称通过 shortcut_name 指定（非空，≤50 UTF-16 code units，与 gateway / Swift 校验一致）。

Self Runner 的 `UIApplication.applicationState == active` 时，主线程打开 `shortcuts://run-shortcut?name=<percent-encoded>`，每 300ms 读屏确认，最多 4s。后台或 URL 未确认锁屏时，launch `com.apple.shortcuts` 并读屏：存在 identifier `BackButton` 和 label 等于名称的标题按钮则直接使用编辑页；否则有 BackButton 先返回列表，找 type 75、label 以 `<name>, ` 开头的卡片。找不到时点 type 49 /「搜索」/ `Search` 搜索框并输入名称，仍无卡片返回 `shortcut_not_found`。点卡片进入编辑页，点 type 9、label「播放」/ `Play` / `Run`，再每 300ms 确认最多 4s。不会点列表的 `shortcut.button.run`。导航读取最多等 2s，全部受请求 deadline 约束；已发送的编辑页播放点击绝不自动重试。锁屏确认要求「已锁定」/「向上轻扫以解锁」/ `Locked` / `Swipe up to unlock`，不单凭 SpringBoard PID（主屏也可能是 SpringBoard）。

结果为 `{locked:true|"unconfirmed",strategy_used:"shortcut",path:"url"|"ui",attempts:[{strategy,path,entry,ms,error_code?}]}`；错误也带当前路径和已完成 attempts；已派发动作带 `action_sent:true,retry_allowed:false`。shortcut 的 ms 是入口调用耗时，不含后续确认/导航等待。显式 `hid|device_event|press_lock` 仍供实验，保留原入口和每次最多 2s 的确认，不进入 auto。

**2026-10-13 真机事实（本单提供）**：XCTest HID/device_event/pressLockButton 均不锁屏，每个耗时 5–9s；「锁屏」编辑页的「播放」按钮（type 9，约 x304,y772,50×50）能锁屏，锁后 SpringBoard 当次 pid 37、读屏有「已锁定」（不硬编码 PID/坐标）。列表卡片运行按钮 identifier `shortcut.button.run`、label「播放、锁屏」不能锁屏，type 75、label「锁屏, 1个操作」的卡片本体能进入编辑页。Self Runner 后台 open_url 为 unsupported；launch 自身返回 launch_failed。代码调查：原实现只调用 LSApplicationWorkspace，false 转为 launch_failed，Info.plist 没有自身 URL scheme；该系统返回 false 的内部原因**未核实**。现在 launch 自身若已 active 直接确认；后台无已配置的可用唤起方式，返回 unsupported，响应 result.message 明示需手动打开，不再尝试该 LS 入口。新 shortcut 流程端到端真机效果**未核实**。

### 数字密码解锁（2026-10-13）

真机输入事实由本单提供：iPhone 14 Pro、中文系统，锁屏前台为 SpringBoard（当次 pid 37，不在代码中硬编码）；press home 后等待 800ms，上滑 `{196,840}→{196,300}`（300ms）可调出数字键盘。标志为「已锁定」「向上轻扫以解锁」「密码栏」，兼容 `Locked`、`Swipe up to unlock`、`Passcode field`；键 label 为 `0`…`9`。本次新增 unlock 的完整真机效果**未核实**。

`unlock` 沿用执行闸、deadline、最近 64 个响应去重；判定已解锁时不输入。亮屏后若无完整键盘只上滑一次，每 300ms 轮询最多 3s，找不到为 `keypad_not_found`。按数字 label 的唯一可见 frame 中心逐位点击，PID 来自该次前台快照；位间等待 120ms，不走通用 tap 遮挡等待。输入后每 300ms 核对前台及 SpringBoard：键盘和锁屏标志均消失才返回 unlocked；到 timeout 键盘仍在为 `unlock_failed`，其余不确定状态为 `unlock_unconfirmed`。失败响应含 `retry_allowed:false`；输入绝不自动重试。

密码的持久副本只放 **Mac 登录钥匙串** generic password，service 为 `phone-gateway.passcode`，account 为配置文件中的完整 `device_id`。用户自行在终端执行以下命令（把占位符换成 device_id；`-w` 不带值，交互输入，不把密码写在命令行）：

```sh
security add-generic-password -s phone-gateway.passcode -a <device_id> -w
# 设置好后调用（args 不带密码）
phone-gateway cmd unlock --args '{"timeout_ms":4000}'
```

Gateway 在实际派发 unlock 时执行 `/usr/bin/security find-generic-password -s phone-gateway.passcode -a <device_id> -w`；读取失败只返回 `passcode_unavailable`，不转发命令输出/错误。只接受 4–6 位 ASCII 数字，非法值为 `passcode_invalid`，不发送。HTTP / cmd 的 args 含 `passcode` 字段一律 `invalid_args`；unlock 拒绝除 timeout_ms 之外的调用方字段。密码不进入排队/在途 args、结果或去重缓存，仅注入本次 TLS 请求；unlock 响应只转发固定布尔字段/错误码，日志仅 action/id/ms/code。

App 只在本次请求内存中使用密码，不写日志、文件、邮箱或 USB mailbox，也不返回读屏/密码内容；解锁快照不进快照缓存。用完尽力覆写工作用 UTF-8 数字数组。**Swift String/JSON 的副本、COW 缓冲及系统/框架持有的传输内存无法保证覆写**，不能声称所有副本已清零；请求结束后释放引用。Mac security 输出缓冲用后覆写；Rust JSON/序列化/TLS 内存同样没有完全清零保证。钥匙串读取可注入假源供测试，生产默认只用上述 security 命令。

## 6．安全边界

- 手机端这一层**不做业务判断**，只执行白名单动作；风险判断交给调用方：对发送、付款、删除类目标先走审批流程。
- 现有研究约束仍然有效：DDI 不卸载、不 roll nonce；`tap-offer` 保留。
- gateway 只接受 PSK 认证通过的一台手机；第二台手机或重复连接时，旧连接会被替换并记录日志。

## 7．App 端变化

- 研究模式（mailbox、E0/E1 实验等）保留；新增「远程模式」开关（UserDefaults），打开且已配对即连 gateway，不等待 XCTest ready（见 §11.D），断线以退避重连（1 s → 30 s 上限）。
- 动作执行复用现有 `SpikeBridge`：读屏和截图沿用；点击需要放开「仅自身靶按钮」的限制，改为对任意屏幕坐标合成（目标 PID 取前台 App）。原有 `tapTarget` 保留给自检。
- 后台保活沿用定位权限方案；长时间稳定性在阶段 4 验收。

### 自动化模式与密码（2026-10-13，WIB +0700）

**真机事实（本单提供，iPhone 14 Pro / iOS 27.0.1）**：当天多次 `devicectl` 重装、启动后直接就绪，没有密码框；本次重装在 `enableAutomationModeWithError:` 后等待 60 s，报 `Timed out while enabling automation mode.`，随后释放 DTX/tunnel。再次 `launch --terminate-existing` 在 IDE sessions 后报 `Cannot initiate shared session more than once.`；是否没有真正重启进程或同进程重入**未核实**。`control_reply` 的 capabilities 含 `"automation mode":1`，这是能力，不是已开启状态，不能据此跳过 enable。

**本机 API 调查**：Xcode 26.5（17F42），iPhoneOS 平台 Developer/Library 下的 XCTest、XCUIAutomation、XCTestCore、XCTAutomationSupport，arm64。用 `nm -arch arm64`、`strings -a`、`otool -arch arm64 -ov` 读取符号、字符串与 class-dump 风格 ObjC 类/协议/方法元数据，用 `otool -arch arm64 -tvV` 核对调用行为。以下编码来自元数据，`@?` 只说明参数是 block，不编码 block 内部参数：

| 框架 / 接收者 | 选择器 | 类型编码 |
|---|---|---|
| XCUIAutomation / `XCUIDevice` | `-enableAutomationMode:` | `B24@0:8^@16`（BOOL，NSError **） |
| XCUIAutomation / `XCUIDevice` | `-automationModeInterface` | `@16@0:8` |
| XCUIAutomation / `XCUIDevice` | `-configuredForUITesting` | `B16@0:8`（不是自动化模式状态） |
| XCUIAutomation / `XCTRunnerDaemonSession(XCUIDeviceAutomationModeInterface)` | `-supportsAutomationMode` | `B16@0:8`（检查 remote capabilities） |
| 同上 | `-enableAutomationModeWithError:` | `B24@0:8^@16` |
| XCTestCore / `XCTMessagingRole_CapabilityExchange`（daemonProxy 协议） | `-_XCT_enableAutomationModeWithReply:` | `v24@0:8@?16` |
| XCTestCore / `XCTRunnerDaemonSession` | `+sharedSessionIfInitiated` / `+sharedSession` | `@16@0:8` |
| 同上 | `+sharedSessionPromiseAndImplicitlyInitiateSession:` | `@20@0:8B16` |
| 同上 | `+initiateSharedSessionWithCompletion:` | `v24@0:8@?16` |
| 同上 | `-daemonProxy` / `-connection` | `@16@0:8` |
| 同上 | `-registerInvalidationHandler:` | `@24@0:8@?16`（返回 observer token；反汇编确认回调无参数） |

四个框架中未找到 `_XCT_getAutomationModeStatus…`、`automationModeEnabled`、`_XCT_requestAutomationModeStatus…` 或 disable ObjC 入口；其它版本/手机 DDI 是否提供仍**未核实**。XCTest 本身仅见调用 `enableAutomationMode:`；状态查询线索来自 XCTAutomationSupport 的未定义 C 符号 `XAMIsAutomationModeEnabled`。`otool -L` 显示它弱链接系统 `/System/Library/PrivateFrameworks/AutomationMode.framework/AutomationMode`；`-[XCTAutomationSession acceptNewConnection:]` 在 arm64 地址 `0x33df4` 无参数调用并测试 `w0` bit 0，据此采用 `BOOL (*)(void)`（C 符号没有 ObjC 类型编码；完整原型未核实）。本机 macOS SDK 的 AutomationMode.tbd 另导出 `XAMDisableAutomationMode` / `XAMDisableAutomationModeWithCompletion`；这不是 iOS 可用性或签名证据，iOS disable 和这两个函数的原型**未核实**，实现不调用。

反汇编确认本机 enable 检查 supports 后，**每次**建立 future 并调用 daemonProxy 的 `_XCT_enableAutomationModeWithReply:`，没有本地“已开启立即返回”分支；daemon 已开启时是否立即回复**未核实**。等待使用 `_XCTApplicationStateTimeout`，读函数返回 `double`（`d0`），`_XCTSetApplicationStateTimeout` 接受 `double`（`d0`），代码只在 enable 调用期间设为 20 s 并在 `@finally` 恢复，避免改变其它动作的超时。

**新行为**：会话启动动态加载 AutomationMode 并查询 XAM；返回 true 时跳过 enable，日志为 `automation mode already enabled`；不可用、异常或 false 时沿用 `enableAutomationModeWithError:`。独立 automation worker 和主线程 20 s watchdog 保证即使私有调用没返回，`status` 仍能给出 `xctest_ready:false, needs_passcode:true`。`needs_passcode` 是超时后的人工检查提示，不证明密码框已显示，也不自动输入密码。私有调用未返回时保留启动门禁；若迟到成功且未 Stop，则清除提示并 ready；若超时错误返回则 Stop，提示保留，用户处理手机提示后可手动 Start。ready 且空闲时 status 保留现有前台查询；启动中/忙时仅返回状态、VPN 和未知前台，避免并发进入 AX。Gateway compact 克隆 result 并只移除节点/图片字段，无白名单，两个 needs 字段自动透传，无需 Rust 改动。

同进程共享 session 只 initiate 一次；后续手动 Stop/Start 优先复用桥持有的 session，首次加载也检查 `sharedSessionIfInitiated`（反汇编确认传 `NO`，不会隐式 initiate，读取 promise.value 不阻塞）。进程内 IDE session UUID 保持一致。Stop/失败/迟到回调只取消当前启动 continuation、释放 boot DTX/tunnel，保留 XCTest shared session，**不调用 disable automation mode，也不主动 invalidate shared connection**。daemon invalidation、初始化失败或已尝试但没有可复用对象时，status 报 `needs_relaunch:true`，日志说明须真正结束并重启进程，不再 initiate。auto-session 在进入 shared session 初始化前置进程级标记，网络变化、远程重连、Stop 和开关切换不清除；RP 建连前的失败仍可重试。remote 连接仅重连 transport。

**待真机验证的假设**：iOS 27.0.1 上 XAM 是否可加载、无 entitlement 时是否能准确读取；跳过 enable 后是否能完成读屏/点击；临时 timeout getter/setter 是否与本机 ABI 一致并实际在 20 s 返回；Stop 释放 IDE DTX 后 singleton 是否仍可复用（系统若 invalidates 则需 relaunch）；重装/真正重启是否保留自动化模式，以及网络变化/远程重连不再 initiate 的日志。上述新行为未在真机执行，不能承诺永不要求密码。

## 8．Mac 主机端

- 新 crate `crates/phone-gateway`，产出二进制 `phone-gateway`：
  - `phone-gateway serve`：常驻，监听手机端口和本机 HTTP（默认 127.0.0.1:7382）；
  - `phone-gateway pair`：生成证书、PSK 和 client token，写出配对文件，经 `devicectl device copy to` 下发到手机；
  - `phone-gateway cmd <action> [json-args]`：经本机 HTTP 调用，打印结果 JSON（截图另存文件）。
- 常驻：`launchd` 用户代理（plist 由 `pair`/`install` 生成；本期只文档化，不自动安装）。

## 9．阶段与验收

| 阶段 | 内容 | 验收（真机，不插 USB；只在配对时插一次） |
|---|---|---|
| 1 | gateway（serve/pair/cmd）+ App 远程模式 + `status/read/screenshot/tap` | 拔 USB 后：`cmd status` 得到 ready；`cmd read` 节点数 > 0；`cmd screenshot` 得到有效 PNG；`cmd tap` 按 label 点 Self Runner 自身靶按钮，计数 +1；断开 Wi‑Fi 再恢复后自动重连，命令可用 |
| 2 | 补全动作集 | 在「设置」里按 label 进入一级页面、返回；`type` 在 Safari 地址栏输入文字；`swipe` 翻页；`press home` 回主屏；`launch` 打开设置 |
| 3 | 调用方接入（调用方仓库，单独设计与派单） | 让调用方读出手机电量（设置 → 电池）并回复 |
| 4 | 稳定性 | 后台 8 小时浸泡，每 5 分钟一次 `read`，成功率 ≥ 99%；手机重启后，用户按「解锁 → 开 VPN → 点开 App」恢复，无需 Mac 操作 |

## 10．已知限制

- 手机重启后需要人解锁、开 VPN、点开 App（约 10 秒）；快捷指令自动化可部分缓解（暂未实施）。
- 免费描述文件每 7 天过期，需要 Mac 重签重装（可走 Wi‑Fi devicectl）。
- 依赖私有 API，iOS 升级可能失效。
- LocalDevVPN 只在建会话时需要。

## 11．独立评审后的决定（2026-10-12，codex-sol high；逐条接受）

**A. 执行语义（P0）**
- 每个请求带稳定 `id`。手机保留最近 64 个已完成请求的结果；收到重复 `id` 时返回缓存结果，不再执行。
- `deadline_ms` 从 gateway 收到调用方请求时开始算，排队时间也计入；超时未派发的请求直接丢弃，返回 `expired`。
- 断线或 gateway 重启时，在途动作一律标 `unknown`，写动作永不自动重发。调用方要重试，必须先读屏核对。
- 手机端只有**一个执行闸**，远程、界面按钮、mailbox、自动自检共用。watchdog 只上报 `still_running`，在底层调用真正返回前不释放闸。
- 不带 expect 的 `ok` 只表示合成调用成功，不代表业务效果；带 expect 时只核对指定节点出现，业务效果仍由调用方判断。

**B. 目标身份（P0）**
- 坐标点击：先用 AX 点查询（`accessibilityElementForElementAtPoint:error:`）取得该点元素及其 `processIdentifier`，作为目标 PID；查询失败时仅允许已有前台快照 PID 与当前前台 PID 一致的 `foreground_fallback`，result 带 `ax_point_error`（原始 pointPID 字典）；无匹配快照则拒绝。
- label 或 identifier 点击：在选定前台应用的结构化快照里查找，必须恰好命中 1 个（或给出从 0 开始、按快照遍历顺序计数的 `index`）；快照被截断时拒绝判唯一，返回 `snapshot_truncated`。
- 可选前置条件 `expect_pid` 或 `snapshot_id`，不符时返回 `stale_target`。
- 只支持竖屏，其它方向返回 `unsupported_orientation`。
- 禁止构造 `XCUIApplication`（E1c 之外的构造路径可能 abort，研究文档已记录）。

**C. 认证状态机（P1）**
- 只接受 TLS 1.3（两端强制）。手机钉扎**叶证书 DER 的 SHA-256**，在 `sec_protocol_options_set_verify_block` 里判定，不接受系统 CA。
- `device_id` 由 `pair` 生成（16 字节随机，hex），写进配对文件；gateway 只接受这一个。
- challenge 为 32 字节，每条连接只用一次，5 秒内必须回 hello，否则断开。
- 认证完成前，单帧上限 4 KiB，未认证连接最多 2 条；认证后单帧上限 16 MiB。
- 只有完整认证通过、`device_id` 匹配的新连接才替换旧连接，旧连接的在途动作标 `unknown`。
- 秘密存放：gateway 的配置目录 `~/.config/phone-gateway/` 权限 0700，文件 0600，其中有 `server.crt`、`server.key`、`psk`、`device_id`、`client-token`。
- `pair` 默认复用已有证书和 PSK，加 `--rotate` 才轮换。
- 手机端只在**未配对**时导入配对文件，重新配对必须在 App 界面上手动点「重新配对」。导入顺序为：校验、写 Keychain、确认写入成功，最后删除文件。
- 本机 HTTP：先校验 token 再解析请求体，只接受 POST，不开 CORS。持有 token 的程序拥有完整控制权；这一点写入文档，pal 的审批约束不了其它持有 token 的程序。

**D. 后台与状态（P1）**
- 远程连接**不依赖 XCTest ready**：App 一启动（远程模式开着）就连 gateway，`status` 总能回答。状态分开报告：`transport`、`xctest_ready`、`vpn`、`last_action_ok_at`。
- gateway 判活：发送带序号的 ping，只认对应序号的 pong，45 秒未确认即判 offline 并断开。手机在回到前台、收到定位回调、网络路径变化时重连。
- 被挂起或被杀时，gateway 无法唤醒手机，只能报告 offline；可靠恢复靠用户点开 App。
- **后台门禁提前到第 1 阶段**（见下）。

**E. 远程模式下手机端少留数据（P1）**
- 远程模式下不落盘读屏快照和截图，日志不记 label，只记动作名、耗时和结果码。研究模式保持原样。

**F. 阶段与验收修订**
- **第 1 阶段**（安全远程 MVP）：`serve/pair/cmd` 加上 `status`、结构化 `read`、PNG `screenshot`、竖屏 `tap`（坐标、label、identifier）。验收都在不插 USB、不连调试器的条件下进行：
  1. `status` 报告 transport online 且 xctest_ready；
  2. `read` 返回结构化节点，数量大于 0；
  3. `screenshot` 返回的 PNG 能解码，尺寸等于屏幕像素；
  4. 按 label 点 Self Runner 靶按钮，再 `read` 读到计数加 1；
  5. 跨 App：手机先停在「设置」，按 label 点「通用」，再 `read` 读到「关于本机」；
  6. 错误的 PSK 或证书钉扎被拒绝（用测试客户端或 gateway 自带测试证明）；
  7. 断开 Wi‑Fi 30 秒再恢复，60 秒内自动重连，命令可用；
  8. App 在后台、手机锁屏静置 30 分钟，每 5 分钟一次 `read`，6 次全部成功。
- **第 2 阶段**：每个动作单独验收。`type` 后读回完全相同的文本；`swipe` 后可见内容变化；`press home` 后前台为 SpringBoard；`launch` 后前台 PID 和身份是目标 App。没跑通的动作返回 `unsupported`，不算通过。
- **第 4 阶段**：8 小时、每 5 分钟一次，共 96 次，**全部成功**才算通过（超时、断线、漏发都计入分母）。另外验收每周重签以后：Keychain 里的配对、远程模式开关和 RP 配对是否还在。
- 每次验收都记录：设备与系统版本、构建号、USB 和调试器状态、锁屏状态、VPN、定位权限、网络，以及原始结果。

## 12. 已验证：横幅遮挡（2026-10-13，WIB +0700）

以下为 2026-10-13 真机实测事实（由本次任务提供）：
- 推送通知横幅在 SpringBoard 树中出现 2–3 秒，多个节点共用 frame `{x:8,y:56,w:377,h:66}`，包含 type 83、label「通知」（type 1）及 label「<通知来源>, 现在, <标题>, <正文>」。未显示时有 375×110 容器位于屏外 x=402,y=34；屏外不相交的 frame 不算遮挡物。
- 改动前 tap 已能识别遮挡并成功：默认等待后约 8.8 秒点中；`wait_occlusion_ms:0` 约 2.6 秒点中。旧结果只有 `occluders`，无法区分自然消失和上滑清除。新流程和观测字段见 §5；本次新实现的真机时延尚未核实，需用下面脚本复测。
- 已确认：AX 点查询在 iOS 上不可用（`only supported in the local macOS AX interface`），前台 PID 保底是正式路径；「设置」无遮挡点击实测 1.9 秒、expect met。此前未看到横幅的观察不再作为横幅不存在的依据。

可重复回归（需已配对且运行中的 gateway、手机解锁、启用通知横幅；推送命令由调用者提供，脚本不内置账号/密钥）：

```bash
PUSH_CMD='<触发一条通知的 shell 命令>' \
GATEWAY=target/release/phone-gateway ROUNDS=3 \
bash scripts/research-banner-occlusion.sh
# 也可用 zsh，或 --push-cmd '<命令>' --gateway <路径> --rounds 3
```

每轮 launch 设置 → tap「通用」（index 0，expect「关于本机」）→ 执行 PUSH_CMD → 最多 10 秒轮询 `read target=system`，匹配 label 恰为「通知」、frame 与屏幕相交且 0≤x<屏宽、y<屏高/3 的节点 → tap `BackButton`（expect「通用」）。屏宽高取前台 AX depth 0 的屏幕 frame（点坐标）；轮间最多等 10 秒直到系统树无「通知」节点，超时该轮记 fail。每轮输出 ok/code/ms/occlusion.cleared_by、横幅观察状态及分类，最后汇总 pass/not_exercised/fail 计数：tap ok、已见横幅、occlusion 存在且 cleared_by 有值且不为 none 才算 pass；tap ok 但无 occlusion 或横幅未出现算 not_exercised；其他算 fail。有 fail 退出 1，全部 not_exercised 退出 2，否则退出 0；未提供 PUSH_CMD 打印用法并退出 64。PUSH_CMD 由 bash 执行且输出不转发；不自动重试失败的 tap。设备/系统版本、构建号、USB/调试器、锁屏、VPN、定位权限、网络及原始结果仍需按 §11.F 在真机回归记录中保留。

## 13. runner 转发通道（P1）

本节定义转发通道的协议与两端实现。gateway 侧见 `crates/phone-gateway/src/tunnel.rs`，手机侧见 `spikes/self-runner-app/Sources/RemoteTunnel.swift`（分支 `Jerry/selfrunner-tunnel-phone`）；真机互通尚未核实。控制链路线上格式和行为不变，不增加控制帧。daemon 经 Mac loopback 转发端口访问手机本机 runner（127.0.0.1:8100）与 MJPEG/H.264（9100）；gateway 不解析 WDA，不修改 runner 自带的端到端 IPU-HMAC 签名。

`serve --runner-forward <ADDR> --mjpeg-forward <ADDR>` 两个参数均可选，默认不开，只接受 IPv4/IPv6 loopback 地址，非 loopback 报错退出。参数地址是 Mac 监听地址；对应手机端口固定为 8100 / 9100，不能由本地调用者指定任意目标端口。

手机主动连接同一个 phone TLS 端口、使用同一证书和 challenge。数据连接回应 `{"t":"hello","v":1,"role":"tunnel","device_id":...,"mac":...}`，mac 为 base64(HMAC-SHA256(psk, `"selfrunner-tunnel-v1" || nonce || device_id`))。认证成功回 `{"t":"welcome"}`，加入空闲池，不调用 `connected()`，不改控制 generation、online、inflight。无 role 的 hello 保持原控制路径和 `selfrunner-v1` 上下文；其他 role、上下文交叉使用、未启用任何 forward 时的 tunnel hello 均拒绝并关闭，不回 welcome。两类连接共用原有握手超时、两个握手槽位、同 IP 100ms 节流。

空闲池最多 8 条，满时淘汰最老的一条、保留新来的（2026-10-14 前为关闭新通道）；控制链路重连时，关闭在上一条控制链路最后一次确认存活之前进池的空闲通道（切网后它们走的是已死的路径），空闲超过 `Timing::tunnel_idle`（默认 120s）关闭。每条 Mac 本地 TCP 连接按 FIFO 领用最老空闲通道，发 `{"t":"open","port":8100}` 或 9100，等待 `{"t":"opened","ok":true}`。读帧限制为 `PRE_AUTH_LIMIT`，无额外 BufReader，避免读走紧随帧后的原始数据。`Timing::tunnel_open` 默认 2s，覆盖发 open 与读 opened；超时、帧错误或 ok 不为 true 时关闭该通道并继续尝试下一条。池空等待新通道；自本地 accept 起总等待不超过 `Timing::tunnel_wait`（默认 5s），失败关闭本地连接。Timing 可在测试中缩短。

opened 成功后双向原样复制字节，正确传播半关闭（单侧 EOF 后可继续接收反向数据），完成或出错后释放两端。全局最多 16 条并发转发连接（包含等待通道的本地连接），超过立即关闭。`/v1/status` 快照只增加 `"tunnels":{"idle":n,"active":m}`，保留已有字段与 CLI compact 输出。日志仅包含 open/close、端口、字节数、耗时及固定失败原因码，不记录内容、psk、mac；复制中途失败仍记录已成功写入的字节数。

离线 fake_phone 支持 tunnel 握手、端口到测试目标地址映射、拒绝端口与不回应 open；`tests/tunnel.rs` 覆盖两端口小包及超过 1MB 数据、半关闭、控制连接共存、认证隔离、等待与打开超时、FIFO 回退、空闲超时、池与并发上限、loopback 限制。

手机侧（`RemoteTunnel`，状态都在 main，接口仿 `RemoteLink`：`configure(enabled:pairing:)`）：远程模式开启且 RunnerCore 在跑时保持 4 条空闲通道（握手中的也算；2026-10-14 前为 2），总数上限 10（原 6），健康时最多 3 条同时握手、失败后退回串行，MJPEG 这类长连接不占空闲名额。拨号间隔至少 0.3s（gateway 对同一 IP 有 100ms 握手节流，也避免挤掉控制链路重连）；认证失败或连不上时指数退避 1s→30s，gateway 120s 空闲回收、流结束属正常回收，按 0.3s 间隔补池，不退避。`ports` 映射决定 gateway 可打开的端口及实际连接的本机端口（默认 8100→8100、9100→9100），不在映射里的回 `{"t":"opened","ok":false}`；本机端口 1.5s 内连不上同样回 ok:false（早于 gateway 的 2s）。对拷时 Mac 侧的半关闭转成对 runner 的 TCP 半关闭；runner 侧 EOF 则在剩余字节发完后优雅关闭整条通道（Network.framework 的 TLS 连接不支持只关写方向）。

Mac 联调（2026-10-13，未接真机）：真实 gateway + 在 macOS 上编译的 `RemoteTunnel.swift` + 本地 echo / 流式服务，5 轮 4MB 往返与 8MB 服务端推流、4 路 echo 并发 + 1 路推流均逐字节一致；端口不在映射里时本地连接在 `tunnel_wait` 后关闭；gateway 重启期间手机侧按 1/2/4s 退避，恢复后补满空闲池并可正常转发。

部署注意：Mac 本机 8100 可能已被其他程序占用（at-mac 上是 palpod 的 mic_bridge.py），转发端口不要默认用 8100/9100，daemon 侧用 `IPHONE_USE_WDA_URL` / `IPHONE_USE_WDA_MJPEG_URL` 指向实际转发地址。


## 14. daemon 外部托管模式（P2）

设置 `IPHONE_USE_WDA_MANAGED=false`，并用 `IPHONE_USE_SELFRUNNER_GATEWAY=http://127.0.0.1:7382` 指向本机 phone-gateway。只接受 loopback HTTP 地址；无效配置会告警并忽略。未配置 gateway 时保留原有外部托管行为。

`IPHONE_USE_SELFRUNNER_GATEWAY_TOKEN_FILE` 可覆盖 token 文件路径。默认读取 `$PHONE_GATEWAY_HOME/client-token`；未设置 `PHONE_GATEWAY_HOME` 时读取 `~/.config/phone-gateway/client-token`。每次请求重新读取 token，不在日志、status 或错误中输出它。daemon 不读取或处理手机密码，解锁密码由 gateway 从 Mac 钥匙串取得。

runner 不可驱动时，status 可增加 `self_runner`：

```json
{"gateway":"up","transport":"online","needs_passcode":false,"needs_relaunch":false,"runner_core_running":true,"vpn":true,"tunnels_idle":2,"tunnels_active":0}
```

取不到的字段为 null，gateway 连不上时为 `"down"`。诊断总预算 2 秒，缓存 5 秒；只有 transport=online 才查询 command status。可驱动时不查询 gateway，也不输出 `self_runner`。

`hint` 和双语 `next_step` 按以下优先级提供恢复建议：

1. gateway 不可达：启动本机 phone-gateway。
2. transport=offline：在手机打开 Self Runner App，确认 LocalDevVPN 已开。
3. needs_passcode 或 needs_relaunch：有人在手机上输一次密码以重启自动化会话，不能远程完成。
4. runner_core 未运行，或空闲和使用中的通道都为 0：打开「Runner API 8100/9100」，或等待通道建立。
5. runner 可达但锁屏：调用一次 phone_reconnect，通过 gateway 使用 Mac 钥匙串密码远程解锁。

此模式下 phone_reconnect 只读取无会话锁屏状态、必要时调用 gateway unlock，然后重新探测 health；不运行本地 setup 或 launchctl。unlock 的失败错误码原样返回；仍不可驱动时附诊断建议，但不会再建议调用 phone_reconnect。未配置 gateway 时 reconnect 行为不变。

health 缓存超过 5 秒时，status 最多等待后台探测 2.5 秒；动作前另用最多 1 秒查询无会话锁屏状态。发现锁屏会在发送动作前拒绝（not_sent、retry_safe=true）。

元素点击（tap_locator、tap_label、tap_element）把顶部 NavigationBar 上方的状态栏区域算作遮挡：目标中心落在状态栏或导航栏下时，沿用已有的「先滚到屏幕中部再点」逻辑，不再点到状态栏左上角的返回链接。导航栏自身的按钮不受影响；没有导航栏的页面不加这条遮挡。

wait_for 为最后一次整树读取预留 `min(2.5 秒, timeout/3)`。快速探测成功但没有匹配时，最终读取失败的 observation 保留 `probe_misses` 和 `probes_answered`，避免把已读到屏幕误报为从未读到。批次中 swipe、scroll、drag 之后的第一个点击或 longpress（中间隔着 pause 也算）先等 settle，最多 1.5 秒；超时继续。

## 15. 外网控制：经 Cloudflare Tunnel 的 WSS 传输与端到端加密（2026-10-14，WIB +0700）

目标：手机只要能上网（蜂窝、热点、别处 Wi‑Fi，LocalDevVPN 常开），pal / daemon 就能经 Mac 上的 phone-gateway 控制它；在家仍可局域网直连。控制链路（§2–4、§11.C）与转发通道（§13）**在字节流之上的线上格式一律不变**，只是多了一种承载它们的传输：

```
手机 ──wss://phone.hiyinni.cn/selfrunner/v1/<route>──▶ Cloudflare 边缘 ──Tunnel──▶ cloudflared(Mac)
     ──http://127.0.0.1:7383（WebSocket upgrade）──▶ phone-gateway WS 入口
WS binary 消息 = E2E 加密记录；记录解开后是一条有序字节流，上面照旧跑长度前缀 JSON 帧、challenge/hello/welcome、open/opened 与原始字节对拷
```

### 15.1 为什么另加一层加密

Cloudflare 边缘会解开 TLS，§11.C 的证书指纹钉扎在这条路上不起作用；unlock 请求里还带着 gateway 注入的手机密码。所以 WSS 只当承载，机密性、完整性和双方认证全靠 PSK 派生的端到端层。Cloudflare 能看到：route 路径、手机公网 IP、消息大小与时间；看不到内容，无法冒充任何一方（没有 PSK），重放和乱序会被计数器拒绝，事后拿到 PSK 也解不开录下的流量（每条连接用临时 X25519 密钥）。

### 15.2 配对信息

`gateway-pair.json` 仍是 `v:1`，新增可选字段 `ws_url`（不带路径，例如 `"wss://phone.hiyinni.cn"`）。`host`/`port`/`cert_sha256` 保留，作为局域网直连（7381，TLS + 钉扎）。

- `phone-gateway pair --ws-url <URL>`：写入该字段；不传则不写（旧格式）。gateway 与 App 对它的校验相同：只接受 `wss://<主机名>[:端口]`（主机名字符集同 `host`，≤253），或仅用于本机联调的 `ws://127.0.0.1[:端口]` / `ws://localhost[:端口]`；不得含路径（单个结尾 `/` 可忽略）、查询、片段、用户信息，总长 ≤ 512。不合规则整份配对无效（`invalid_pairing`）。
- 旧 App 读到带 `ws_url` 的文件会忽略该字段（Codable 未知键），行为不变。
- 下发与导入流程不变；Keychain 已有配对时要在 App 里点「重新配对」才会导入新文件。PSK 不轮换时 runner token 不变。

### 15.3 路由键（挡住扫描器）

WS 路径固定为 `/selfrunner/v1/<route>`，`route = hex(HMAC-SHA256(PSK, "selfrunner-ws-route-v1"))` 的前 32 个字符（小写）。gateway 在 upgrade 之前比对路径（常量时间），不符直接回 404 并关闭，不占握手槽位。路由键不是秘密（Cloudflare 看得到），只用来让互联网上的随机请求够不着握手槽位；认证仍靠下面的握手。

upgrade 之前的资源上限：未完成 upgrade 的 TCP 连接最多 16 条（超出立即关闭），请求头总长 ≤ 8 KiB，从 accept 到请求头读完 ≤ 3 秒。**不做按来源节流**：`CF-Connecting-IP` 可被本机进程或中继任意填写，不能当身份；PSK 为 256 位，无需防暴力猜测。

已知不防：Cloudflare（或拿到路由键的人）可以持续占满握手槽位、丢弃或拖延消息，让外网手机连不上——这是可用性问题，局域网直连不受影响；机密性与完整性不受影响。

### 15.4 E2E 握手

WS 建立后：

1. 手机发第一条 binary 消息 `M1 = "SRE1" || 0x01 || e_c`（共 37 字节，`e_c` 为本连接新生成的 X25519 公钥）。
2. gateway 校验长度和前缀，生成自己的临时密钥，回 `M2 = "SRE1" || 0x02 || e_s`（37 字节）。
3. 双方计算：
   - `dh = X25519(自己的私钥, 对方公钥)`，结果全 0 即断开；
   - `th = SHA256("selfrunner-e2e-v1" || M1 || M2)`；
   - `prk = HKDF-Extract(salt = th, ikm = PSK || dh)`（HKDF-SHA256）；
   - `k_c2s = HKDF-Expand(prk, "selfrunner-e2e-v1 c2s", 32)`，`k_s2c = HKDF-Expand(prk, "selfrunner-e2e-v1 s2c", 32)`。
4. 之后每条 binary 消息是一条记录：`ChaCha20-Poly1305(k_方向, nonce = 4 个 0 字节 || seq 的 8 字节大端, aad = 空)` 的「密文 || 16 字节 tag」。**seq 不上线**：每个方向各自从 0 计数、每条记录加 1，接收方用自己期望的 seq 构造 nonce 解密，因此重放、乱序、丢弃、反射都表现为解密失败，即断开；seq 达到 2^64−1 也断开。临时私钥用后即丢（x25519-dalek `EphemeralSecret` / CryptoKit 私钥不落盘、不复用），以兑现前向保密。
5. 明文 = 1 字节类型 + 载荷：`0x00` DATA（载荷 1..65536 字节，字节流的下一段），`0x01` EOF（载荷为空，表示发送方不再写，即半关闭；之后该方向不得再有记录）。其他类型即断开。
6. **关闭语义**：字节流的「正常结束」只认 EOF 记录。一方发出 EOF 后仍须继续接收对方的数据，双方都发出并收到 EOF 后才关闭 WS。没收到 EOF 记录就遇到 WS Close、断线或解密失败，一律作为**错误**交给上层（读返回 io 错误，不是 EOF），防止 Cloudflare 截掉尾部被当作正常完成。

之后在解出的字节流上照旧走 §4 / §11.C / §13：gateway 发 challenge（这是 gateway 的第一条记录，手机能解开它才算确认对端持有 PSK），手机回 hello（控制链路无 role，转发通道 `role:"tunnel"`，MAC 上下文不变），gateway 回 welcome。因此一条 WSS 连接认证两次：E2E 层证明持有 PSK，hello 再核对 device_id 与上下文，旧逻辑一行不改。

限制与超时：

- WS 单条消息 ≤ 65536 + 17 字节，单帧同上限；分片消息要正确拼装，**在累计分配之前**按上限拒绝（tokio-tungstenite 用 `WebSocketConfig` 的 `max_message_size` / `max_frame_size`，并用带配置的 accept 函数）。收到 text 消息、超长消息即断开。
- 两层上限各管各的：E2E 记录 ≤ 64 KiB 载荷；内层帧在认证前仍 ≤ 4 KiB、认证后 ≤ 16 MiB（原逻辑）。适配器只能持有有界缓冲，不得无限预读排队。
- 从 upgrade 完成到 welcome（E2E 握手 + challenge/hello/welcome），gateway 总时长仍是 `Timing::hello`（5 秒）。
- 握手槽位：WS 入口独立 4 个槽位（控制链路 1 条 + 补池的通道可能同时握手），与 7381 互不挤占；槽位满时回 503 并关闭。

测试向量（两端单元测试都要对上；`PSK = 00..1f`，`client_private = 20..3f`，`server_private = 40..5f`，均为连续字节，X25519 私钥按 RFC 7748 clamp）：

```
m1:              5352453101358072d6365880d1aeea329adf9121383851ed21a28e3b75e965d0d2cd166254
m2:              535245310279a631eede1bf9c98f12032cdeadd0e7a079398fc786b88cc846ec89af85a51a
dh:              04c304fb1ca83cee75e206344231f33797e07d9929db670994b7c6fbeb1dc255
transcript_hash: 418f81a9f94e6c194b81d5f083462fc6ecfd3c94df7f91bc14f055c14b48b219
key_c2s:         840004597a499caeca91c295843319f40c52774522be214b5d6571e378be3c5f
key_s2c:         c79a8bdb55636d741b168d40fe70b9afdc4e22f9933454f26f09e21f0c04ae62
s2c seq 0, DATA "hello": b80ae39be1e326570554cdbcd547fbc28382c664b5d6
c2s seq 0, DATA "world": cd03d4ee6288919dea0bf25a474d3d5432331ef75b35
c2s seq 1, EOF:          6fc3dc760a5c5ef263404a0f4436124ca8
route:           e285a22ef3cf3f7b6c09bb2188bc75ee
```

### 15.5 保活

Cloudflare 文档称会断开 100 秒无数据的 WebSocket（未实测）。gateway 对**每条** WS 连接（控制、空闲通道、正在对拷的通道）每 30 秒（±3 秒抖动）发一次 WS Ping，手机自动回 Pong（不进 E2E 层，不影响 seq）。连续 2 次 Ping（约 60 秒）都没等到 Pong、或单次 WS 写超过 30 秒，即关闭该连接。WS 层的 Ping/Pong/Close 由收发任务独立处理，不能被上层字节流的背压卡住。Pong 可被中继伪造，只说明承载活着；控制链路判活仍靠原有加密的 15 秒 ping/pong、45 秒判离线。转发通道空闲回收仍是 `Timing::tunnel_idle`（120 秒）。

### 15.6 gateway（Mac）

- `serve --ws-bind <ADDR>`：可选，默认不开；只接受 loopback 地址，否则报错退出。部署值 `127.0.0.1:7383`。只做 WebSocket upgrade（HTTP/1.1 GET，路径见 15.3），别的请求一律 404。
- E2E 层实现为一个独立模块：握手完成后返回一个实现 `AsyncRead + AsyncWrite` 的字节流（例如收发任务 + 有界通道，配 `tokio_util::io::StreamReader` 之类能把错误交给读方的适配；`tokio::io::duplex` 无法表达「异常结束」，不要用）。上层写方向 shutdown 时发 EOF 记录；收到 EOF 记录时上层读到 EOF；没有 EOF 记录的 WS 关闭、断线、解密失败让上层读到 io 错误（15.4 第 6 条），转发日志因此记为 `copy_failed` 而不是 `eof`。上层握手（challenge → hello → welcome）和之后的 `connected()`、`Tunnels::insert()` 复用 7381 的同一段代码（把那段从 `listen()` 里抽成对任意字节流通用的函数；`Tunnels` 的空闲池改存装箱的字节流）。
- `/v1/status` 快照增加 `"link":"lan"|"wss"|null`（当前控制链路从哪个入口进来）；其余字段与 CLI compact 输出不变。日志只记入口、事件、原因码，不记 route、密钥、记录内容。
- 实现补充（2026-10-14）：E2E 读端最多排队 16 条记录（约 1 MiB），满了就阻塞读 WS 形成反压（读端停住期间 Pong 不计数，消费方卡住约 60 秒后由保活关闭连接）；异常结束前已收到的记录照常交给上层，排空后才报错。控制链路主循环的 `select!` 改为优先处理已收到的帧，并且在退出（写失败、判离线）时若有在途请求，最多再等 500ms 收它的 `res`——否则响应与连接关闭、心跳写失败同时发生时会被误报 `unknown`（WS 上一写就失败，比 TCP 更容易撞上；离线测试 `control_commands_status_and_entry_replacement` 曾 1/4 失败）。
- 依赖：WebSocket 服务端用 `tokio-tungstenite`（带 `WebSocketConfig` 的 `accept_hdr_async_with_config`，在回调里校验路径）；加密用 lock 里已有的 RustCrypto 版本：`x25519-dalek 2`、`chacha20poly1305 0.10`、`hkdf 0.12`、`sha2`。
- 离线测试：`fake_phone` 增加 WS + E2E 客户端，`tests/` 覆盖控制链路与转发通道都能经 WS 工作（含 >1MB 往返和半关闭）、错误 PSK 在 E2E 层被拒、错误路径 404、篡改/重放/乱序记录断开、text 消息断开、无 EOF 记录的关闭在上层表现为错误、超长消息在分配前被拒、upgrade 前连接数和请求头上限、`ws-bind` 非 loopback 报错、两个入口并存且控制链路可在两者之间替换。

### 15.7 cloudflared（Mac，运维）

- 单独配置文件 `~/.cloudflared/phone-gateway.yml`：`tunnel` = `be9c57e2-d34d-41b3-b08c-ddb21f41b5af`，`credentials-file` 指向同名 json，ingress `phone.hiyinni.cn` → `http://127.0.0.1:7383`，其余 `http_status:404`。**不改** `~/.cloudflared/config.yml`（ycodepro 旧配置），账号里其他隧道不动。
- launchd 用户代理 `com.at.cloudflared-phone`：`cloudflared tunnel --no-autoupdate --config ~/.cloudflared/phone-gateway.yml run`，RunAtLoad + KeepAlive，日志 `~/Library/Logs/cloudflared-phone.log`。
- gateway 的 launchd 参数加 `--ws-bind 127.0.0.1:7383`。

### 15.8 App（手机）

- 引入一个字节流传输抽象（建立、收、发、半关闭/结束、取消、状态回调），两个实现：现有 TLS over TCP（钉扎不变，行为不变）与 WSS + E2E。RemoteLink 与 RemoteTunnel 的帧解析、握手、对拷逻辑改为基于该抽象。
- WSS 实现要点：`NWConnection(to: .url(<ws_url>/selfrunner/v1/<route>))`，参数为 TLS（系统 CA 校验 Cloudflare 证书）+ `NWProtocolWebSocket.Options`（`autoReplyPing = true`，`maximumMessageSize` 设为 65536 + 17）；按**消息**收（`receiveMessage`，检查 metadata 的 opcode 只接受 binary），发送时带 `NWProtocolWebSocket.Metadata(opcode: .binary)` 的 context；「一条消息收完」不是流 EOF，流 EOF 只认 EOF 记录（15.4 第 6 条）。发送串行、seq 只在主队列分配。
- 对拷：WSS 上 Mac 侧 EOF 记录转成对 runner 的 TCP 半关闭；runner 侧 EOF 在剩余字节发完后发 EOF 记录，**继续**把 Mac 侧数据转给 runner，直到也收到对方 EOF 才关闭。TLS 通道保持现状（Network.framework 不能只关 TLS 写方向）。
- 超时：每次拨号尝试有自己的定时器（替换 RemoteLink 现有固定 5 秒那个），取消旧尝试并按 token 丢弃迟到回调。`lan` 预算 2 秒（含 TCP、TLS 和 welcome / 通道进入 idle），`wss` 8 秒（含 DNS、TLS、upgrade、E2E 和 welcome）。
- 路由选择（控制链路与转发通道各自按同一规则独立决定）：当前网络路径用 Wi‑Fi 时顺序为 `lan` → `wss`，否则 `wss` → `lan`；同一条路径上 `lan` 失败过一次后，直到路径变化（NWPathMonitor）前都把 `wss` 排前。两个都失败才进入原有退避。没有 `ws_url` 时只有 `lan`，行为同现在。注意 `link=lan` 只说明控制链路，转发通道可能走的是 wss。
- App 界面与 `status` 显示当前控制链路的路由（`lan` / `wss`）。
- 日志不记 route、密钥、记录内容。
- 真机发现并已修（2026-10-14）：切网后旧路径上的空闲通道成了僵尸（手机看来空闲、gateway 已为 0），现在路径变化即关闭空闲通道重拨，且手机侧 110 秒回收空闲通道；daemon 每个 runner 请求新开一条通道，经 WSS 一条要 1–2 秒，原来 2 条池 + 串行拨号会在 run_steps 突发时 `wait_timeout`，因此加深池并允许并行握手；「重新配对」只在有合法新文件时才替换配对；网络路径变化时若控制链路在线，立即发 ping，5 秒无任何回应即判定旧链路已死并重连（否则要等 45 秒数据超时，期间 status 会超时，重签脚本会因 vpn=unknown 跳过）。
- Mac 联调：Swift 源码可在 macOS 编译，对本机 gateway 的 `ws://127.0.0.1:7383` 跑通控制与转发（同 §13 的联调方式），并用 15.4 的测试向量做单元检查。

### 15.9 MJPEG 流量

经 WSS 时，所有字节都走 Cloudflare 免费套餐。截图走 runner HTTP，单张约 0.3–1 MB；MJPEG 按 10–30 fps、每帧 50–100 KB 估算为 0.5–3 MB/s，持续推流一小时就是 2–10 GB，且可能与 Cloudflare 对非网页大流量的条款冲突。约定：在外（`link=wss`）时只用 read / screenshot / run_steps，不长时间开 MJPEG 或录屏；真机实测数据补在 15.10。

### 15.10 验收（真机）

1. 手机走蜂窝或热点、只开 LocalDevVPN：`phone-gateway cmd status/read` 正常且 `link=wss`；daemon `phone_status` 为 `drivable:true`；`phone_run_steps` 能执行；
2. `selfrunner-autoresign --force` 重签重装后，不碰手机即自动恢复（经 wss）；
3. 回到家里 Wi‑Fi，App 重启后走 `link=lan`；
4. 端到端加密：错误 PSK、篡改记录被拒（离线测试证明）；Cloudflare 侧只看到 binary 密文（抓 cloudflared 到 7383 的明文 HTTP 可见只有 upgrade 头和二进制帧）。
