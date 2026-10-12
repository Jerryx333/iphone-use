# Phone Use 单 App 真机验收

目标：助手、浏览器、远程控制与本机被控服务集中在一个 `com.leeguoo.iphone-use.remote` App。首次电脑安装和配对后，在「设置 → 本机控制」启用；系统网络授权和重启后的首次解锁由人完成。

## 已验证

- 保留原 SwiftUI 主程序与已有 App 数据，包内含一个本机网络扩展、实时活动扩展和 `PhoneUse.xctest`，没有嵌套 `.app`。
- 单包签名构建、深度签名校验与安装通过。普通启动仍显示助手、浏览器、远程控制、任务和设置。
- 主 App 复用设备 Keychain 中已有配对记录，用户完成新的 Phone Use 网络授权。实验 App 不参与启动。
- 正常 Mac daemon 与 supervisor 均停止，USB 设备列表为空。手机内启动同一个主 App 的 XCTest 会话，端口 18100 / 18787 提供控制与 daemon 服务。
- 初版将 `_XCTestMain(nil)` 放在主队列回调中，导致长期占用该队列，UI 请求超时。改为从主 RunLoop 进入后，主队列请求恢复，daemon 报告 `ready / drivable=true`。
- 主 App 内合成控件完成真实点击、输入、AX 后置条件和截图，1.92 秒。报告见 [control.json](2026-10-12-single-app-control/control.json)。本轮后端为 0.17.18，不混用正常 Mac 0.17.19 的证据。
- 主 App 在后台、计算器在前台 169 秒后，daemon 仍可驱动且 AX 读屏成功，USB 与正常 Mac 服务仍断开。见 [background.json](2026-10-12-single-app-control/background.json)。
- 主 App 的二维码经真实截图识别后，另一端成功兑换并配对。报告仅记录成功状态，不保存连接码，见 [qr.json](2026-10-12-single-app-control/qr.json)。

## 本轮发现与修复

后台返回后的第二次合成控件验收，点击通过但输入后置条件失败，见 [原始报告](2026-10-12-single-app-control/after-background-control.json)。验收页已在每轮重新创建输入控件并退出旧焦点，修改后待复测，不将失败记录改写为通过。

停止本机控制会结束同进程的 XCTest 主 App。原顺序可能让关闭网络扩展的后续代码无法执行；改为先保存停止意图、禁用自动重连，再停止网络扩展，由扩展收尾控制会话。修订候选 `PhoneUse-verified.app` 已签名安装，当前手机锁屏，系统拒绝启动，等待人解锁后验收。

## 尚待验收

最终修订的重复输入验收、完整重启后的独立恢复、停止/重新启用，以及删除旧自有实验 App 后的最终状态。先前两 App 实验的验收不能代替这些项目。

源码位于 `feat/single-app-control` 独立 worktree。协调会话更新的 `db9897c/libipu_embed.a` 与 loopback 配置原样保留；不修改 Mac 的代理出口。正常 Mac supervisor 当前暂停，恢复它会重新安装旧 runner，须与最终只留一个 App 的迁移一起处理。
