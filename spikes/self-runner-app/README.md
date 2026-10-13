# SelfRunnerSpike (S0b)

最小 SwiftUI 宿主：一个进程同时持有 Rust IDE-side DTX 会话与 DDI XCTest
runner-side daemon session。部署目标 iOS 17.0；不带 NetworkExtension entitlement，
可用免费 Apple ID 签名。当前只验证编译，私有 API 与 LocalDevVPN 协议需真机确认。

## 构建

需要 Xcode、xcodegen、`/opt/homebrew/opt/rustup/bin` 的 Rust toolchain，
并已安装 `aarch64-apple-ios` target。脚本使用 `--offline` 与 `CARGO_BUILD_JOBS=2`；
必要时自行指定可写的 `CARGO_HOME`。不设置全局 `CARGO_TARGET_DIR`。

```sh
spikes/self-runner-app/build-lib.sh
cd spikes/self-runner-app
xcodegen generate
xcodebuild -project SelfRunnerSpike.xcodeproj -scheme SelfRunnerSpike \
  -sdk iphoneos -configuration Debug -derivedDataPath build/DerivedData \
  CODE_SIGNING_ALLOWED=NO build
bash -n install.sh build-lib.sh
```

工程不写死 DEVELOPMENT_TEAM。签名安装时由环境提供并作为 xcodebuild 参数传入：

```sh
UDID=<USB-UDID> DEVELOPMENT_TEAM=<your-team-id> ./install.sh
```

`install.sh` 会 build-lib → xcodegen → 签名构建 → 安装 →（若缺失）USB RP 配对 →
推送配对文件。这是会操作手机的脚本，只应在主会话明确允许的真机实验中运行。
免费账号可能需先在 Xcode Settings 添加 Apple ID，签名约七天过期。
配对文件保存在 `build/pairing/rp_pairing.plist`，不会覆盖已有记录。

## 配对与独立诊断

从仓库根目录：

```sh
CARGO_BUILD_JOBS=2 cargo build -p self-boot --bin self-boot-probe --offline
./target/debug/self-boot-probe probe --udid <UDID> --observe-secs 60
./target/debug/self-boot-probe rp-pair --udid <UDID> --name my-selfrunner --out <new-file.plist>
./target/debug/self-boot-probe rp-connect --pairing <file.plist> --addr 10.7.0.1:49152 --observe-secs 60
```

原 USB 探针现在显式使用 `probe` 子命令。`rp-pair` 使用该 idevice revision 的
CoreDeviceProxy/RSD/untrusted tunnel service/RemoteXPC 流程。解锁手机、点「信任」、
按手机提示输入设备密码；协议若要求六位 PIN，终端会提示输入手机显示的配对 PIN。
iOS 的 consent 通常由 idevice 内部使用 000000，不是要求把设备锁屏密码输入终端。
总 deadline 180 秒。文件以 create_new + 0600 写入，已有文件拒绝覆盖。
配对文件含设备访问密钥，不要提交或分享；App 的文件共享功能也能访问它。
`rp-connect` 在 Mac 上使用手机/VPN可达的地址；默认回环地址通常只在手机内部可达。
两种探针都只读 DDI 状态、启动 IDE 会话、观察后关闭；不会挂载镜像或启动测试计划。

## 手机端步骤

1. 开启 Developer Mode。先由主会话用 Xcode 挂载 personalized DDI（本阶段不实现挂载）。
2. 从 App Store 安装 LocalDevVPN，开启 VPN；保持开启。不能同时使用另一 VPN。
3. 确认 App Documents 下有 `rp_pairing.plist`（安装脚本推送，或通过 Finder 文件共享放入）。
4. 打开 Self Runner，默认接口地址 `10.7.1.1`、对端地址 `10.7.0.1`，端口默认自动（Bonjour）。
   Start 记录全部接口并选择地址匹配的 utun；6 秒浏览 `_remotepairing._tcp`，
   解析并记录所有服务地址和端口，优先接口地址匹配、其次对端地址匹配，
   同优先级按服务名和端口排序。无匹配服务时明确记录回退 49152；也可选手动端口。
   pairing 与 TLS 隧道两条 TCP 连接均绑定所选 utun 和接口 IPv4；未找到接口会列出
   全部 utun 并停止。失败日志区分 pairing connect / pair verify / tunnel connect / TLS / RSD。
5. 默认关闭「授权自身 PID」，默认开启后台定位。点 Start，允许本地网络和定位。
   若要后台保活，到 Settings 将定位设为 Always。App 不存储或传输位置。
6. 等待 daemon session（首次可能约 50 秒，90 秒超时），自动化弹窗出现时输入手机密码。
7. ready 后点「读屏」「截图」「点自测按钮」。只有靶按钮计数恰好 +1 才记录 SUCCESS。
   自测必须让 App 保持前台、竖屏、靶按钮无遮挡；事件指定本进程 PID。
8. 点「停止」释放 runner daemon connection、Rust IDE DTX、心跳与隧道，并停止定位。
   XCTest framework 不 dlclose。XCTest 的进程全局 shared session 可能不能复用；
   若 Stop → Start 失败，退出并重新打开 App，记录日志供主会话分析。

## 日志拉取（主会话）

日志同时显示在面板、NSLog 与 `Documents/selfrunner.log`：

```sh
xcrun devicectl device copy from --device <UDID> --domain-type appDataContainer \
  --domain-identifier dev.iphoneuse.selfrunner.spike \
  --source Documents/selfrunner.log --destination /tmp/selfrunner.log
```

日志包括每次 dlopen 路径与 dlerror、session UUID、IDE initiate replies、可选 PID
授权回复、每五秒状态及收到的 DTX/Marco-Polo 事件、读屏节点数/前台 bundle id/耗时、
截图字节数/耗时、自测计数/耗时。KeepaliveDegraded 不等于 DTX 断开。

## 运行时桥接与局限

没有修改 runner 文件。`IPURBridge` 直接 import XCTest、IPURGeometry、IPURImageCodec，
具有大量 runner/测试 bundle 功能；spike 独立写 `SpikeBridge`，通过 NSClassFromString
和有签名的 objc_msgSend 使用相同 API 事实，不链接/打包 XCTest。
没有复制 TouchSynthesis/StikDebug 代码，也未引入它们的许可头。

Xcode 26.6 iPhoneOS 的 XCTestCore Objective-C 元数据确认
`+[XCTRunnerDaemonSession initiateSharedSessionWithCompletion:]` 的 callback 和
XCUIAutomation 确认 `-[XCTRunnerDaemonSession enableAutomationModeWithError:]` 返回 BOOL。
启动先设置 XCTestSessionIdentifier（大写 UUID）、XCTestManagerVariant=DDI、
XCTestConfigurationFilePath=""，再尝试加载 XCTestSupport、XCTTargetBootstrap、
XCTestCore、XCUIAutomation、XCTest（/System/Developer 优先，/Developer fallback）。
不调用 `_XCTestMain`、finishInitializationForUIAutomation 或 requestAutomationSession。
未证实需要其它环境变量，因此没有凭空设置 test bundle 路径。

与完整 IPURBridge 的差异：AX 单次深度 24、最多计数 2000，不做深树补全/缓存/树序列化；
从 activeApplications 选首个非系统 application（多前台/PiP 情况待验证）；
截图只走 PNG daemon request（10 秒 deadline），没有其它截图 fallback；
点击只支持固定的实际 UIKit 靶按钮、竖屏、50 ms 触摸，不提供任意坐标或其它动作。
对 daemon session 启动与截图 callback 做显式超时；RP connect 有 35 秒外层 deadline，
其内部协议步骤仍有 30 秒 deadline。同步 private AX/enableAutomation/synthesis 调用
本身没有可靠取消 API；Stop 会等待已进入的串行 Rust/AX/enable 操作结束，不能承诺即时停止。
所有 XCTest API 仍需 iOS 27.0.1 真机验证，编译成功不等于自动化已能运行。
