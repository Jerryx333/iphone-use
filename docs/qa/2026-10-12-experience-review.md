# 完整体验验收清单

目标是正式功能集中在一个 App，不能用旧 runner 或早期 prototype 的通过结果替代最终包验收。基线为语音整合候选 `37aeeae`；本轮工作位于独立 `review/experience-20261012`，不覆盖正在测试的候选和他人未提交改动。

| 要求 | 必须取得的证据 | 当前边界 |
| --- | --- | --- |
| 一个 App、自启动、停止后可恢复 | 最终包、无旧 App、无 USB/Mac 服务的完整重启和重复启动 | 单 App 冷启动仍待负责会话验收 |
| 手机与内置浏览器连续执行 | 最终包中真实 browser → phone → browser，结果与恢复均可读回 | 旧 build 15 通过不代替新包 |
| 免费模型开箱可用 | 新配置下无需私有密钥，真实执行与模型记录，网络失败能恢复 | 原免费模型任务已通过；新转发路径需跟候选复验 |
| chrome-use site 深度复用 | 手机更新目录、脚本校验、真实网站操作；不隐式转电脑 | 目录/合成任务通过；真实登录网站与最终包需补验 |
| Cookie 与资料隔离同步 | 用户选择账号/站点后导入，跨账号无泄漏、重启后恢复 | build21模拟器合成账号隔离、Legacy回退及资料库读回通过；真机仍待验收 |
| 连续中文语音 | 识别、真实执行、播报、中断/换任务、后台与重连 | 由语音会话占用 17 继续验收 |
| 灵动岛及旧设备状态 | 当前/下一步、暂停/风险提示、无重复活动；无岛设备有 App 内状态 | 新最终包需复验，不能只看源码 |
| 风险允许/拒绝 | 提示匹配任务，拒绝 0 步、允许只执行对应步骤，回前台可操作 | 原 build 17 两分支通过，新包仍需覆盖 |
| 手动录制 flow | 从 App 遥控录制、保存、读回步骤、真实重放 | 仍需真机覆盖 |
| 一台 iPhone 控另一台 | 两台真实设备扫码、授权、点击输入、失联与重连 | QR 兑换通过不代替两台手机互控 |
| iOS 15/16 | 最终 App 真机安装、浏览器/远程入口及无岛回退 | 类型检查/最低目标不代替旧机安装 |
| 正式交付 | 集成所有确认修复、提交归属、最终构建、验收与旧产物清理 | 未宣称整体完成，不恢复第二个 App |

本轮补充：`6a48f83` 纳入已过 8 项隔离回归的认证修复；`9bcc2f0` 给连接二维码增加明确重试、保守有效期、过期隐藏和离开页面取消刷新，使用滚动布局避免小屏幕裁切。

连接码改动用 Swift 6、iOS 15 Simulator target 完成局部类型检查，使用真实 ProtectedStore/RunnerSigning 源码及抽出的现有导航容器，日志 `/tmp/iphone-qr-ios15-typecheck.log`。此结果不是完整 App 构建或真机视觉验收，尚未安装到 17。

未改动语音会话正在修改的 AgentLiveStatus.swift、AppModel.swift、UnifiedAssistantView.swift；也未操作 17、启动正常 Mac 服务、改 VPN 或读取真实凭据。

本机设置补充：组件不完整时禁止启用，缺少配对时明确提示导入；旧系统与不完整版本分别说明能力。网络正在启用时禁止重复启动，控制活跃期间禁止更换配对记录，导入回调再次检查；文件授权失败有提示，取消选择不显示失败。使用真实 LocalTunnel、DeviceStore、ProtectedStore、PairRecordImport 和 RunnerSigning 源码完成 Swift6/iOS15 局部类型检查；导航和 AppModel 仅用类型检查适配器，未做完整 App/真机 UI 验收。日志 `/tmp/iphone-setup-ios15-typecheck.log`。

网络扩展回复恢复：新增 ProviderMessageRequest 的一次完成保护，覆盖超时、取消、重复/晚到回调、同步发送失败和空回复。状态读取限时 8 秒，控制命令限时 30 秒；命令进行时不发状态轮询，旧请求结果不能覆盖更新请求。超时/不完整回复进入 awaiting_status，仅只读核对，不自动重发启动，保留停止入口。

完整工程构建验证：已合入语音会话已提交的 `b778987`，整合提交 `99e46e9`，没有覆盖其未提交代码。主 App build、控制 bundle build-for-testing、单包组装和签名通过；主工程完整测试目标 build-for-testing 也通过。未安装或运行设备测试。

封存候选 `/tmp/iphone-experience-candidate/PhoneUse.app`：标识 `com.leeguoo.iphone-use.remote`、build18、MinimumOSVersion15.0、IPUSingleAppControl=true；含两个扩展和唯一 PhoneUse.xctest，没有嵌套 .app，deep/strict 签名校验通过。验证 JSON 同目录，记录二进制 SHA256 及未安装状态。后续测试目标构建没有改变封存候选哈希。

固定原生库位于 `/tmp/iphone-experience-native-libs-01a12715`，来源及哈希见 inputs.json；现有开发镜像及浏览器库只读引用。日志 `/tmp/iphone-experience-single-app-build.log`、工程 main-build.log/control-build.log、`/tmp/iphone-experience-test-build.log`。这些结果不代替二维码视觉、网络恢复、语音、冷启动和旧系统真机安装验收；没有操作17或启动旧 normal 服务。

真实新增 XCTest 文件在独立无依赖 Swift Package 中执行 6 项通过（复制的源码/测试与提交文件相同），日志 `/tmp/iphone-provider-xctest.log`。实际 LocalTunnel + 设置页完成 Swift6/iOS15 局部类型检查，日志 `/tmp/iphone-provider-ios15-typecheck.log`；不是完整 App XCTest 或真机故障注入。未操作17、未部署候选。

手动录制恢复修复：服务端保留最后一次停止的录制草稿，直到新录制开始，GET 状态可以读回；重复停止不会清掉保留草稿。App 不自动重发录制 POST，开始前先读取远端状态，避免清空已有录制。停止回复丢失后只读恢复草稿，断线显示状态未确认并保留检查入口；旧轮询回复不能覆盖停止结果，已关闭的保存页不会因再次同步重复弹出。

真实 FlowRecorder 源码及 XCTest 在独立 Swift Package 执行 8 项通过，模型直接从 Flows.swift 提取，日志 `/tmp/iphone-flow-recorder-tests.log`。完整主 App 和测试目标 build-for-testing 通过，日志 `/tmp/iphone-recording-test-build.log`；这仍是编译证据。服务端定向测试已提交 heavy-cargo 机器限流队列，当前等待执行，不能记作通过。封存的99e46e9签名包不含本轮录制修复；需要更新原生嵌入库后重新组装、安装及真机录制/保存/重放验收。没有操作17、没有重新启用正常服务。

保存链路继续修复：不传 flow 的保存现在优先使用停止后保留的录制，避免误存最近任务；提供了 flow 但 JSON 损坏时会报错，不会悄悄改存其他步骤。成功回复必须包含与请求相同的 saved.id，缺失或错误回复不能显示已保存。保存页读取失败可以重试，重复保存有进行中保护，空白名称校验覆盖换行。

新增 5 项真实 FlowSavePayload XCTest，与录制恢复 8 项共同执行 13 项通过，日志 `/tmp/iphone-flow-recorder-tests.log`；完整 App 和测试目标 build-for-testing 通过，日志 `/tmp/iphone-recording-save-test-build.log`。Rust 保留草稿测试补充了停止后隐式保存的断言，仍等待同一 heavy-cargo 队列执行。已核对并合入语音会话已提交的 3fd3815，整合提交5bc65d7；不覆盖其工作区。上述源码尚未更新到签名包或17，最终验收矩阵仍未完成。

取消保存后的草稿入口：关闭保存页保留 pendingDraft，遥控页和菜单可继续保存；新录制前必须先保存或明确忽略已有草稿。重新打开遥控页时只读取回服务端停止草稿，以按钮提示，不自动弹页。成功保存会按原文档及录制版本清除服务端待保存状态，避免后台写入期间把新录制清掉；保存其他文档和失败写入都不清除它。

新增 4 项 FlowRecorder XCTest 覆盖关闭重开、阻止覆盖、重新进入遥控页和保存后再次同步，当前独立 Package 共 17 项通过，源码/测试复制文件均一致；当前日志 `/tmp/iphone-flow-recorder-tests.log`。完整 App/test build-for-testing 通过，日志 `/tmp/iphone-recording-pending-test-build.log`。Rust 对同一保留草稿测试补充成功保存/错误文档/旧版本断言，尚未执行完成；同一个 heavy-cargo session 持续存活，没有因观察超时重启。签名、部署和真机录制重放均未宣称完成。

浏览器 localStorage 恢复：此前导入脚本一直留在 WebView 中，后续导航可能把新登录或退出后的状态改回旧备份。现在选定 HTTPS 原站点（默认端口）第一次加载完成后移除本次恢复脚本，保留其他用户脚本；错误站点、子域、HTTP 或其他端口不会提前消耗恢复。账号选择持久化失败时不再先移除旧浏览器的 Cookie 观察器和停止其任务。

真实 BrowserStorageRestore 源码和 3 项 XCTest 在 macOS 独立 Package 中执行通过，使用 WebKit 的真实 WKUserContentController 和 JavaScriptCore 运行合成状态；JSONValue 从现有源码提取，没有读取用户浏览器、Cookie 或 Keychain。日志 `/tmp/iphone-browser-storage-tests.log`。完整 App/test build-for-testing 通过，日志 `/tmp/iphone-browser-storage-test-build.log`；这不代替手机真实网站登录/退出和 iOS15/16 安装验收。封存签名包尚未更新。

已核对并合入语音会话已提交的 f107594/d3d04c9，本分支整合提交 b766fdb，版本变为 build19。显式启动返回前台及实际前台状态记录，与本分支已有的 ProviderMessageRequest 超时/取消/旧回复保护共存。重新生成工程后完整 App/test build-for-testing 通过，日志 `/tmp/iphone-experience-build19-test-build.log`；未安装这个整合版本，封存 build18 包仍是旧候选。Rust 定向回归继续等待原队列，不能宣称这批录制服务端修改已通过或已进入当前真机嵌入库。

账号范围改变的导入保护：同一个外部账号 ID 换了网站范围，会过滤旧 Cookie 并重新应用所选账号的网站数据；首个 HTTPS 网站改变时清除旧 localStorage，不把原网站的登录数据写入新网站。超出新范围的启动地址不再恢复。范围未变时保留现有会话，不触发额外清理；确认导入页说明范围变化的处理。

真实 BrowserAccountScope 源码及 5 项 XCTest 在独立 Package 通过，BrowserAccount/BrowserCookie/JSONValue/域名检查从源码提取，均用合成数据，日志 `/tmp/iphone-browser-scope-tests.log`。另新增完整 BrowserLibrary 的“只导入书签但网站范围改变”回归，完整 iOS App/test build-for-testing 通过，日志 `/tmp/iphone-browser-scope-test-build.log`；此资料库回归仅编译，尚未在 App XCTest 或手机运行。没有读取真实账号或修改其他会话的浏览器资料。签名候选与当前真机均未包含本次改动。

资料库运行回归补强：实际 BrowserLibrary、BrowserAccountScope、网站目录和 ChromeBinding 源码在 macOS 独立 Package 中运行 3 项 XCTest 通过，包括损坏文件保留、范围更改后加密写入并重新加载、合成 Chrome 配对密钥加密持久化，日志 `/tmp/iphone-browser-vault-tests.log`。测试方法从 App 的 BrowserLibraryTests 提取；只排除未使用的原生包解密桥，Keychain 适配器会在任何读写时立即失败，测试始终显式提供合成密钥，未访问用户 Keychain。使用真实 CryptoKit/AES-GCM 和临时文件。该结果覆盖资料库实际导入/保存/读回，仍不等于 WebKit 账号切换或手机真实登录验收。

整合 e0ad3a1 的本机重启入口时补齐正式接线：该提交只更新实验目录的 PhoneTunnelProvider，正式 App 当时编译独立旧副本。正式工程现直接编译统一的 experiments/phone-self-host/Tunnel/PhoneTunnelProvider.swift，删除重复副本，后续更新不会漏进正式扩展。主 App/test build-for-testing 通过，日志 `/tmp/iphone-restart-canonical-test-build.log` 明确记录 PhoneLocalControlTunnel 目标编译统一源文件。

重启检查不会再忽略任务查询失败：进入操作即设 busy，当前服务不可连接或任务读取失败时保留服务并显示原因，只有成功读取且所列任务全部终态时才发 restart。任务列表客户端不再把坏 JSON 当空列表、也不再丢弃无法解码的行。真实 AgentTaskList 和现有任务模型提取到独立 Package，4 项 XCTest 通过，日志 `/tmp/iphone-agent-task-list-tests.log`。完整 App/test 编译覆盖该入口与之前的资料库读回测试，但本轮未签名安装或执行真机重启。Rust 录制回归仍是原来的排队 session，未报通过。

最终包前的结构核对：用真实 build19 主 App 与现有控制产品执行 assemble/validate_main，确认 IPUSingleAppControl=true、唯一 PhoneUse.xctest、两个扩展、没有嵌套 runner App；报告 `/tmp/iphone-experience-build19-packaging-check.json`。主可执行文件及控制测试 binary 的 LC_BUILD_VERSION 均为 minos15.0/sdk27.0。这只验证组装和最低链接目标，控制产品仍是旧嵌入库；临时组装包已删除，未签名或安装，不能作为包含录制修复的最终候选。

原生更新以干净源码1a31311提交到 heavy-cargo 队列：`IPHONEOS_DEPLOYMENT_TARGET=15.0 heavy-cargo build -p ios-embed --release --target aarch64-apple-ios`，独立 target 在本 worktree，日志 `/tmp/iphone-experience-native-build19.log`。原定向 Rust 测试已进入实际编译，沿用原 session；原生库排在其后，不并行启动重型构建。旧系统实机型号/版本已向用户询问，尚未确认设备；最低链接版本不是旧系统实机安装证据。

录制服务端定向回归现已执行通过：原 heavy-cargo session57764 正常退出0，1项通过、821项过滤，运行0.17秒，冷编译29分钟。该原始测试新增断言实际验证停止后保留、重复停止、隐式保存、错误文档/旧版本不确认、正确保存清除及新录制无旧草稿。结果与当前 Rust 源文件哈希记录在 `/tmp/iphone-experience-recording-rust-result.json`。这不是822项完整回归；完整 `heavy-cargo test -p server --lib` 已排队，日志 `/tmp/iphone-experience-server-full-regression.log`，将在当前 iOS 嵌入库构建后执行。

当前新嵌入库已进入实际 Cargo/rustc 编译，沿用 session88521；不因没有即时输出重启。尚无新库完成/签名安装证据。语音会话2bb607e已明确其 build19 候选尚未安装且后续读屏出现 wda_source_failed，记录中的 ready/serving 不替代控制结果；本分支不把该报告作为已完成真机验收。

原生库与完整服务端回归现在均完成：iOS release 构建24分16秒成功，库60944280字节，已固定至 `/tmp/iphone-experience-native-libs-1a31311/libipu_embed.a`，来源/哈希见同目录 native-build.json；录制 Rust 源哈希与定向通过时一致。完整 server lib 回归821通过、0失败、1忽略（redaction::sample::render_sample），日志 `/tmp/iphone-experience-server-full-regression.log`。尚未组装或安装使用这份新库的最终签名包。

默认 Zen 的实时兼容问题：官方目录当前仍列 Big Pickle 免费（https://opencode.ai/docs/zen/），仅用合成提示和 public 预设执行真实工具调用，HTTP200返回完整 fixture_result，但终态 tool_calls 没有 index，旧 collapse 因而报 invalidResult。现在仅在明确 tool_calls 终态、完整 id/type/name/对象参数齐全时兼容完整列表；无终态、缺标识、参数截断或与旧片段冲突继续拒绝。5项真实 ZenStreamTests 在独立 Package 通过；刚才捕获的真实响应经新源码离线重放可返回正确工具结果，没有再次执行 API 调用或工具。

相关证据：原始实时调用 `/tmp/iphone-zen-live-probe.json`（HTTP200、旧解析失败），修复后的捕获响应验证 `/tmp/iphone-zen-captured-response-validation.json`（passed=true、replayed_captured_response=true），测试 `/tmp/iphone-zen-stream-tests.log`，完整 App/test build-for-testing `/tmp/iphone-zen-stream-test-build.log` 通过。仅发合成文本，没有手机/浏览器资料，也未使用个人密钥。这证明该实时响应可兼容，不代替 iPhone 原生模型转发或整套真机任务验收。

完整 build19 签名候选现在已生成：`/tmp/iphone-experience-candidate-build19/PhoneUse.app`，Swift/App 源码65d6b82，Rust 嵌入库源码1a31311，使用新库SHA256 387bb9bb3f354fffa19211fcda6f48f24acfb7b90b3a7107ed02ad51b4bababc。含当前录制恢复、保存、账号范围、Zen 完整调用、统一正式网络扩展及重启检查修复。主 App build、控制 build-for-testing、组装/签名成功，独立 deep/strict 校验通过；唯一 PhoneUse.xctest、两个扩展、无嵌套 App，主程序与控制 binary 的 minos15.0/sdk27.0。完整验证 JSON 在候选同目录 build-verification.json，含主/控制 SHA256 与 installed=false/physical_acceptance=false。日志 `/tmp/iphone-experience-build19-signed.log` 及工程 main-build.log/control-build.log。未覆盖99e46e9旧封存候选，未操作17，尚未安装；最终验收矩阵仍保持未完成。

候选收敛：已核对语音会话干净且已提交的 a2a01d6，其完整源码包含本轮所有修复，实际构建号21。整合至本分支 dadeb2c，功能源码与对方一致；仅合入提交，没有操作其工作区。当前正式验收候选为 `/tmp/iphone-experience-candidate-build21/PhoneUse.app`，独立 deep/strict 校验通过，验证 JSON 同目录；19/20 仅作历史记录，不用于下一次安装。此前按旧候选称为20，独立读取 Info.plist 发现实际21后已改正目录和记录，没有修改二进制版本。

build21 使用已通过821项 server 回归的新1a31311嵌入库及既有固定 host 库，主可执行 SHA256 8986be3705a53d87c6d9d23dfae88547c1c8ba2f5acdc18c0b51013f81b85495，控制 SHA256 bb94bab90ab8dca08fd984ca2ab64f7f8e57b263bbdef99e283ef6174d75d761。包内身份/minos15.0/单App标识、唯一控制测试 bundle、两个扩展及无嵌套 App 均核对。构建日志沿用 `/tmp/iphone-experience-build20-signed.log`（命名早于版本读取）；主/control build 成功。installed=false/physical_acceptance=false，17 仍由语音会话占用，CoreDevice不可连接的报告未解除。原始语音链路记录 voice-chain.json 为 blocked/goal_completed=false，只证明真实麦克风进入原生工具链，不证明微信目标完成。


## 手动录制与两机互控的待执行验收

执行前条件：17 由当前语音会话明确交还；控制端与目标端分别确认设备归属和空闲状态。目标端安装核验后的 build21；不恢复正常 Mac supervisor 或安装第二个 runner。两台手机使用各自 App，通过目标端连接码配对。当前尚未具备这些条件，本节是准备步骤，不是通过记录。

1. 在目标端打开「本机控制 → 开发验收 → 验证本机控制」，使用自己的合成页面。确认「验收点击」按钮、输入框及点击计数；控件标识为 selfhost-smoke-button/selfhost-smoke-text。控制端遥控该目标，核对设备身份及页面标题。
2. 在控制端 App 遥控菜单开始录制。只操作合成页面：点击验收按钮，再聚焦输入框，输入唯一的 phoneuse-flow 测试文字。每次导航后重新读屏，核对计数和文字；不以 batch applied 或 ready 标志代替实际后置条件。
3. 停止录制，检查保存页步骤数。先取消保存，再用「继续保存录制」重新打开，确认同一草稿仍在。用唯一 qa/record-fixture-* ID 保存；记录实际输入参数列表，确认输入被参数化，没有依赖真实聊天、短信或凭据。
4. 从「我保存的」读回 ID、应用和步骤数。目标端离开并重新打开合成验收页面，观察计数归零及输入框清空；无需 fresh launch 主 App，避免终止同进程控制服务。使用合成参数重放一次，核对点击增加1、输入内容符合参数及终态，没有额外步骤或错误。
5. 在独占测试窗口下补断线恢复：录制进行中暂时中断控制端连接，恢复后检查状态；停止结果不明确时只读核对及恢复同一草稿，禁止自动重复开始/停止。截图和记录须包含实际状态未确认与恢复结果。任何写入结果未知立即停止重放，不通过多次尝试凑通过。
6. 测试结束清理仅本轮 qa/record-fixture-* 流及合成输入，确认两端配对/会话停止或按交接要求保留，设备租约释放。收集目标 build/包哈希、设备型号/系统、配对结果、录制前后计数与文字、保存 ID/步骤/参数、重放后置条件、断线恢复和清理结果；不公开原始图片、连接码或凭据。

独立复核语音会话 build21：现场 codesign deep/strict 校验通过，实际控制二进制 SHA256 73903bf5dace68dab6e8781817ac6707879601a64468a26492a1baae5d1fba12 与其已提交 build21-candidate.json 一致，唯一控制 bundle、无嵌套 App。对方仍保留17且明确未安装。报告声称114项合并回归，原始执行日志/xcresult路径已请求，尚未取得，当前只记为待范围核对，不用该数字证明手机端完成。


build21 原始 iOS 回归已独立核对：日志 `/tmp/iphone17-build21-regression.log` 包含 only-testing 的15个测试类和114项0失败/TEST SUCCEEDED；xcresult `/Users/leo/github.com/iphone-voice-single-app/apps/ios/build-single-voice-sim/Logs/Test/Test-IPhoneUseRemote-2026.10.12_14-29-16-+0900.xcresult` 经当前 xcresulttool 读取为114通过、0失败、0跳过。平台明确为 iOS Simulator 26.5，模拟型号iPhone14（Phone Voice Regression），不是实机14，也不是iOS15/16。

重点运行用例实际 Passed：BrowserLibrary 的加密导入与账号隔离、Legacy ephemeral恢复、只导入书签但网站范围改变；FlowRecorder 的晚到轮询保护与未知停止后恢复；ZenStream 的完整终态工具调用缺index兼容。其余选择范围为模型转发、统一助手、语音会话/通话、模型payload、任务解码、设备身份、任务列表、provider超时、账号scope、storage恢复、录制和保存。独立导出的 summary/tests JSON 在 `/tmp/iphone-experience-build21-xctest-summary.json` 与 `/tmp/iphone-experience-build21-xctest-tests.json`。这将对应App运行测试从未核对升级为已核对；不改变设备安装、声音/麦克风实机、连续读屏、最终风险确认、两机互控和旧系统安装均未完成的边界。
