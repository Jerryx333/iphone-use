# Self Runner 主人确认交付记录（2026-11-05 WIB）

实现已完成，待独立模型评审、主会话补跑受沙箱限制的 Rust 验收和主人确认。没有 commit、push、安装手机或重启 launchd；按文件暂存，由派单方提交。本次未访问 pal 仓库，也未访问运行中的 daemon 或读取生产密码。当前 daemon 是否仍为 `bound:false` 未核实，沿用任务提供的背景。

## 改动文件与建议提交

建议单个提交：`feat: 将主人面容 ID 确认接入 Self Runner 并封闭远程动作旁路`。

逐文件暂存范围如下（不使用 add -A，不提交生成工程、DDI、静态库或构建产物）：

- `Cargo.lock`、`crates/phone-gateway/Cargo.toml`：复用 workspace 已有 reqwest 0.12；lock 只新增 phone-gateway 的依赖引用。
- `crates/phone-gateway/src/owner.rs`：新增受限本机 HTTP 转发、固定错误码、请求大小限制、pending 变化摘要、原始 body/配置/边界测试。
- `crates/phone-gateway/src/gateway.rs`：welcome 后接入手机主人消息、全局频率预算、连接内请求/轮询生命周期；认证、HTTP 白名单和限频测试。
- `crates/phone-gateway/src/lib.rs`、`src/main.rs`：模块注册及 serve 环境配置接入。
- `crates/phone-gateway/tests/logging.rs`：假密码、签名、nonce 不出现在响应/捕获日志中的断言。
- `crates/phone-gateway/README.md`：配置、协议及 heavy-cargo 验收说明。
- `crates/server/src/owner_confirm.rs`：保护默认 Self Runner、点分后缀、既有自定义 bundle 环境变量；保留旧遥控 App 保护。
- `crates/server/tests/owner_confirm.rs`：默认/后缀/自定义 bundle 的 launch 与前台输入拒绝测试；不删改既有测试。
- `spikes/self-runner-app/Sources/OwnerConfirmation.swift`：新建 Secure Enclave Face ID 密钥、绑定与指纹、完整可滚动详情、签名允许/无签名拒绝、通知和第二层保护状态。
- `spikes/self-runner-app/Sources/RemoteLink.swift`：独立手机请求通道、关联响应/超时/断线、不自动重试、不进动作缓存；手机请求间隔至少 300ms，避免刷新链撞到网关每秒 4 次限制。
- `spikes/self-runner-app/Sources/RemoteActions.swift`：远程变更入口及期限检查点应用主人界面保护；保留原本地自测路径。
- `spikes/self-runner-app/Sources/SelfRunnerSpike.swift`：主界面入口和根部 sheet。
- `spikes/self-runner-app/RunnerCore/RunnerCoreHost.swift`、`runner/IPhoneUseRunner/IPhoneUseRunnerUITests/RunnerTests.swift`：补上 daemon 经 RunnerCore 8100 API 的手机侧保护；普通 XCTest 宿主不安装此回调。
- `spikes/self-runner-app/Info.plist`、`project.yml`：Face ID 用途说明，避免 xcodegen 覆盖。
- `spikes/self-runner-app/README.md`、`docs/owner-confirmation.md`：入口、协议、推送、配置、防线及真机检查说明。
- 本交付记录。

## 消息与配置

```json
{"t":"owner_req","id":"关联UUID","method":"POST","path":"/owner/key","body_b64":"原始body的base64"}
{"t":"owner_res","id":"关联UUID","status":200,"body_b64":"原始响应body的base64"}
{"t":"owner_res","id":"关联UUID","error":{"code":"owner_unconfigured"}}
{"t":"owner_pending","body_b64":"当前GET /owner/pending原始响应的base64"}
```

只允许 GET/POST `/owner/key`、GET `/owner/pending`、POST `/owner/confirm/<id>`，仅已认证控制连接；HTTP 白名单未开放。体积限制为请求 16 KiB、owner 请求帧 24 KiB、响应 256 KiB；全局 4 次/秒、单连接一个手机请求在途；超限/失败固定代码。HTTP 3 秒超时，禁用代理、重定向、重试、自动解压。每 3 秒在线查询 pending，列表变化才推，断开取消，无离线队列或历史重放。

配置：`PHONE_GATEWAY_OWNER_URL`（http 数字回环地址）、`PHONE_GATEWAY_OWNER_TOKEN_FILE` / `PHONE_GATEWAY_OWNER_TOKEN` 二选一；无默认秘密。未配置/坏配置只使主人消息失败。自定义主人 App bundle 复用 daemon 的 `IPU_SELFRUNNER_BUNDLE_ID`。

签名消息、TOFU、旧钥换钥规则未变。指纹为 X9.63 公钥原始字节 SHA-256 小写 hex。每次签名使用新 LAContext，仅 `.deviceOwnerAuthenticationWithBiometrics` 且 `.faceID`；SE ACL 仅 `.privateKeyUsage + .biometryCurrentSet`。不使用设备密码或软件私钥回退。拒绝不读钥、不签名。签名不缓存、不重试、不落日志。

## 实际验证

1. 按 AGENTS 选择全局/仓库 heavy-cargo，执行 `test -p server -p iphone-use-mcp -p phone-gateway --no-fail-fast`。首次因系统临时目录不可写停在 ring C 编译；改用已授权 `/private/tmp` 作为 TMPDIR 后完成编译及全部测试目标运行，退出 101：**1429 passed / 533 failed / 4 ignored**。528 个失败为 `Operation not permitted`（主要本地监听），另 5 个为 `/bin/ps` 受限或其空输出造成的断言。没有跳过、删除或放宽用例。
2. 已通过的新测试：未认证主人帧拒绝；真实 hello/welcome 后接收主人请求及第 5 次限频；HTTP command/owner 路由不可达；配置错误脱敏；请求路径/方法/大小/不透明字节；pending 变化/不变/空列表；默认/自定义/后缀/旧遥控 App bundle 判定。真实 HTTP 转发和 HTTP 输入拒绝集成测试因 bind 被沙箱拒绝，**未验证运行结果**。日志测试中新增假密码断言已执行到后续原有 listener 前，但整个日志测试因该 listener 失败，不能标为整体通过。
3. 本地 `bash scripts/research-stage-ddi.sh` 准备已有 DDI；`xcodegen generate` 成功。未连接手机。仓库 build-lib.sh 有裸 cargo 回退，因此未调用它；等效使用 `TMPDIR=/private/tmp IPHONEOS_DEPLOYMENT_TARGET=17.0 CARGO_BUILD_JOBS=2 "$heavy_cargo" build -p self-boot --release --target aarch64-apple-ios --lib --offline`，成功，并复制 libself_boot.a 到 App build 目录。
4. `TMPDIR=/private/tmp xcodebuild -project spikes/self-runner-app/SelfRunnerSpike.xcodeproj -scheme SelfRunnerSpike -sdk iphoneos -configuration Debug -derivedDataPath spikes/self-runner-app/build/DerivedData CODE_SIGNING_ALLOWED=NO build`：**BUILD SUCCEEDED**（Xcode 26.5，iPhoneOS 26.5 SDK）。此前缺静态库的链接失败已解决。App 和 RunnerCore 均编译。
5. `swiftc -typecheck -swift-version 5 -target arm64-apple-ios17.0`，指定 iPhoneOS SDK、桥接头、include 和可写 module-cache，覆盖全部 App Swift 源：通过，无输出诊断。
6. `TMPDIR=/private/tmp bash spikes/self-runner-app/tests/remote-transport-check.sh`：已有向量/路由/配对/记录拒绝检查通过；原 opt-in gateway 集成未启用。
7. `cargo fmt --check`：全仓失败，106 个既有文件存在格式差异，与本次改动文件交集为空；对全部 7 个改动 Rust 文件执行 `rustfmt --edition 2021 --check --config skip_children=true ...` 通过。`git diff --check` 通过。

原始日志（当前会话机器本地，不含生产凭据）：`/private/tmp/owner-selfrunner-cargo-final.log`、`/private/tmp/owner-selfrunner-xcode.log`、`/private/tmp/owner-selfrunner-swift.log`、`/private/tmp/owner-selfrunner-transport.log`、`/private/tmp/owner-selfrunner-ios-lib.log`、`/private/tmp/owner-selfrunner-fmt.log`。

## 需主会话补跑

在允许本地监听和进程查询的主会话（按派单要求加 --net，保留测试原样）：

```sh
heavy_cargo=./scripts/heavy-cargo
[ ! -x ~/.local/bin/heavy-cargo ] || heavy_cargo=~/.local/bin/heavy-cargo
TMPDIR=/private/tmp "$heavy_cargo" test -p server -p iphone-use-mcp -p phone-gateway --no-fail-fast
```

必须全过后才能认定 Rust 验收完成；不能把这次沙箱失败计为通过。随后按既定流程独立模型评审、主人确认，再决定安装。Face ID 成功/取消/锁定/变更登记、无密码回退、后台通知、绑定与真实指纹、过期/消费/签错原样显示，以及 direct gateway / RunnerCore 两路动作保护，均需真机确认，当前**未核实**。

## 边界与原文核对

- 手机端第二层防线必须覆盖 RunnerCore：daemon 的 8100 API 直接调用 RunnerCore，不全经 RemoteActions。已修复宿主回调旁路，主边界仍为 SE Face ID 签名。
- 托管恢复需要 launch Self Runner，原文 `crates/server/src/setup/flow.rs:713` 的 selfrunner_launch 使用 devicectl，未经过 agent/input 保护；此路径未改，不新增 agent 豁免。
- pending 返回原文是 `{"pending":[...]}`，见 `crates/server/src/http.rs:33236`，本实现转发整个原始响应，不误当顶层数组。
- 「完整详情」指完整显示 daemon 已生成的 summary/details；daemon 对不可信文本已有引用和截断，客户端不恢复被服务器截断的内容。签名消息使用完整 nonce/digest，UI 不展示或记录 nonce。
- 当前 UI 不提供换钥操作；已绑定另一钥时不覆盖，协议仍要求旧钥签名。首次绑定 TOFU 抢占风险保持原有设计。
- iOS 挂起或通知拒绝时后台送达无法保证；在线前台自动弹出，手动刷新仍可用。跨线程/跨进程前台状态与动作之间竞争无法靠第二层完全消除。

## 范围外问题（只记录）

- `spikes/self-runner-app/build-lib.sh:15` 在没有全局 heavy-cargo 时回退裸 cargo，与 AGENTS.md 不一致；本次绕开该脚本并直接用仓库 heavy-cargo 构建，未修改脚本。
- 全仓已有 106 文件 fmt 差异；未格式化无关文件。`crates/server/src/flow_grants.rs:80` 的 ring deprecated API 警告未修改。

## 本轮评审修复（2026-11-05 WIB）

只修本单两项；未暂存、commit、push、安装手机或重启服务。上文“实际验证”是上一轮记录，不能当作本轮验收结果。

### 修改文件和行为

- `runner/IPhoneUseRunner/IPhoneUseRunnerUITests/RunnerActions.swift`：统一 `OwnerMutationProtection.check`，传入实际 bundle/PID；未安装回调时保留普通 XCTest 行为。提取原有 session bundle 解析，保持 alwaysMatch → firstMatch 首项 → desiredCapabilities 优先级。
- 同目录 `RunnerRoutes.swift`：在实际 activate 入口检查，覆盖 POST /session（上述三种 capabilities）、WDA launch/activate；对终止、URL、文本、元素调整、焦点合成、全选、复杂手势、滚动与解锁重复按键补检查。
- 同目录 `RunnerTests.swift`：native launch 复用 activate；统一手势和 Home（含延后 SpringBoard activate）检查；keep-awake 定时合成按键执行前检查，即使此前已经续期也不会越过保护。
- `spikes/self-runner-app/RunnerCore/RunnerCoreHost.swift`、`Sources/OwnerConfirmation.swift`：跨动态 bundle 回调接收实际 bundle/PID，不再从 HTTP 顶层猜目标；找不到保护类/方法继续 fail closed。
- `spikes/self-runner-app/Sources/RemoteActions.swift`：按键、Shortcuts fallback launch、解锁 swipe/数字 tap 的输入前增加期限/保护检查点；复用 execute 的保护回调。
- `runner/unit-check/main.swift`：新增三种 capabilities、原优先级、目标透传、确认页出现前后及无回调行为测试。
- `crates/phone-gateway/src/protocol.rs`：读帧保留原始 JSON 字节长度，原 read_frame 接口行为不变；长度不含四字节传输前缀。
- `crates/phone-gateway/src/gateway.rs`：识别 owner_req 后使用原始长度，超过 24 KiB 返回 owner_request_too_large，不再重新序列化计量。仍先遵循全局限频预算；解析前保留既有 16 MiB 通用硬上限。duplex 认证集成测试加入恰好 24 KiB、空白超一字节、重复键、Unicode 转义膨胀帧；后三者解析序列化后均小于限制，仍应拒绝。
- 本 README：本轮变更、核对与验收记录。协议原文 `docs/owner-confirmation.md:56` 的“owner 请求帧最多 24 KiB”与修复后行为一致，无需改协议。

### 逐入口核对表

| 入口 | 执行前保护 |
| --- | --- |
| RunnerCore POST /session：alwaysMatch / firstMatch / desiredCapabilities | sessionBundle → activate → check(bundle) |
| RunnerCore native /launch、WDA apps/launch、apps/activate | 统一 activate → check(bundle) |
| RunnerCore Home、SpringBoard fallback activate | 每次 checked 动作调用 check |
| RunnerCore apps/terminate | check(bundle) |
| RunnerCore URL：system.open、旧 Safari activate/tap/type fallback | openURL → check |
| RunnerCore tap/swipe/longpress/alert tap、坐标 fallback | gesture → check |
| RunnerCore 元素 click/focus/keyboard dismiss/picker | tapPoint → gesture → check |
| RunnerCore replacement focus tap | 入口 check(pid)，allowDispatch 再检查 |
| RunnerCore keys/type/clear、XCUI type fallback | 每次 typeIntoFocus → check(pid)；selectAllText 单独 check(pid) |
| RunnerCore slider/picker adjustment | adjust → check(pid) |
| RunnerCore element touchAndHold/drag/多指、W3C actions | elementGesture / synthesizePaths / actions dispatch → check |
| RunnerCore scrollTo（可多轮） | 每轮合成前 check |
| RunnerCore pressButton / lock / unlock | pressButton、lockScreen 检查；unlock 每次 Home 前检查 |
| RunnerCore inline keepawake 续期及既有 timer | 续期仅改 deadline；真正 beat/resetIdleTimer 前主线程检查 |
| RemoteActions tap/type/swipe、press、launch、open_url | execute 初检与合成前 expired 包装检查；UIKit open 在主线程内检查 |
| RemoteActions lock 的 Shortcuts URL/fallback launch/UI taps/锁屏策略 | 同一保护包装覆盖；fallback launch 增补即时检查 |
| RemoteActions unlock 的 Home/swipe/每个数字 tap | 每次输入前 expired 包装检查 |
| read/status/screenshot/settle、查询/设置/查找、配对码 | 无前台切换或合成输入，保留行为 |
| RunnerXCTestShell 的启动 activate、本地 UI 自测 | 本地 XCTest 启动/用户按钮，非 Self Runner 远程动作入口，保持原流程 |

第二层检查仍不能消除跨进程前台状态变化与私有 API 派发之间的竞争；Face ID 签名边界未变。真机行为未核实。

### 本轮实际验收

- 指定 heavy-cargo `test -p server -p iphone-use-mcp -p phone-gateway --no-fail-fast`：退出 101，尚未编译或执行测试。依赖 fs2 未缓存，crates.io 域名无法解析；本轮没有运行到监听/ps 测试，不能沿用上一轮通过数。
- 按构建前置执行本地 DDI staging、xcodegen：成功。遵守 AGENTS，未使用 build-lib.sh 的裸 cargo fallback；等效 heavy-cargo 离线构建 self-boot：退出 101，离线缓存缺 fs2。
- README 所述无签名 iPhoneOS xcodebuild：最终 App 和 RunnerCore Swift 编译未报告错误；整包链接失败 `ld: library 'self_boot' not found`，因为上述静态库未构建。**整包 BUILD SUCCEEDED 未取得**。本机 `xcodebuild -version` 返回 Xcode 26.6 / 17F113，构建使用 iPhoneOS26.5.sdk。
- 尝试独立 -target RunnerCore 构建：被 module session cache 写入错误阻塞；没有为绕过而更改工程或测试。
- `TMPDIR=/private/tmp CLANG_MODULE_CACHE_PATH=/private/tmp/owner-review-clang SWIFT_MODULECACHE_PATH=/private/tmp/owner-review-swift bash runner/unit-check.sh`：335/336 通过，新增 owner 检查均通过。唯一失败是既有 `VideoToolbox H.264 encoder starts`，失败根因未核实；未删改或跳过测试。
- `cargo fmt --check`：全仓既有格式差异，输出不含本轮两个 Rust 文件；`rustfmt --edition 2021 --check --config skip_children=true crates/phone-gateway/src/gateway.rs crates/phone-gateway/src/protocol.rs` 与 `git diff --check` 通过。
- 日志：`/private/tmp/owner-review-{cargo,ios-lib,xcode,core,swift-unit,fmt,ddi,xcodegen}.log`。不把编译或测试阻塞记作通过。

### 本轮需主会话补跑

需有依赖缓存或网络，并允许本地监听、进程查询及 VideoToolbox 服务的主会话：

```sh
heavy_cargo=./scripts/heavy-cargo
[ ! -x ~/.local/bin/heavy-cargo ] || heavy_cargo=~/.local/bin/heavy-cargo
TMPDIR=/private/tmp "$heavy_cargo" test -p server -p iphone-use-mcp -p phone-gateway --no-fail-fast
TMPDIR=/private/tmp IPHONEOS_DEPLOYMENT_TARGET=17.0 CARGO_BUILD_JOBS=2 "$heavy_cargo" build -p self-boot --release --target aarch64-apple-ios --lib --offline
mkdir -p spikes/self-runner-app/build
cp target/aarch64-apple-ios/release/libself_boot.a spikes/self-runner-app/build/libself_boot.a
TMPDIR=/private/tmp xcodebuild -project spikes/self-runner-app/SelfRunnerSpike.xcodeproj -scheme SelfRunnerSpike -sdk iphoneos -configuration Debug -derivedDataPath spikes/self-runner-app/build/DerivedData CODE_SIGNING_ALLOWED=NO build
TMPDIR=/private/tmp bash runner/unit-check.sh
```

本轮派单转述与原文不符之处：无。
