# 完整体验验收清单

目标是正式功能集中在一个 App，不能用旧 runner 或早期 prototype 的通过结果替代最终包验收。基线为语音整合候选 `37aeeae`；本轮工作位于独立 `review/experience-20261012`，不覆盖正在测试的候选和他人未提交改动。

| 要求 | 必须取得的证据 | 当前边界 |
| --- | --- | --- |
| 一个 App、自启动、停止后可恢复 | 最终包、无旧 App、无 USB/Mac 服务的完整重启和重复启动 | 单 App 冷启动仍待负责会话验收 |
| 手机与内置浏览器连续执行 | 最终包中真实 browser → phone → browser，结果与恢复均可读回 | 旧 build 15 通过不代替新包 |
| 免费模型开箱可用 | 新配置下无需私有密钥，真实执行与模型记录，网络失败能恢复 | 原免费模型任务已通过；新转发路径需跟候选复验 |
| chrome-use site 深度复用 | 手机更新目录、脚本校验、真实网站操作；不隐式转电脑 | 目录/合成任务通过；真实登录网站与最终包需补验 |
| Cookie 与资料隔离同步 | 用户选择账号/站点后导入，跨账号无泄漏、重启后恢复 | 本轮只使用合成资料，不自动迁移真实凭据 |
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
