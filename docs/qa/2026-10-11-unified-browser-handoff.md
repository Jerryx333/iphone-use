# 统一助手与 Chrome 会话交接（2026-10-11）

当前验收设备已切回 iPhone 17 Pro Max（iOS 27.0）。App 当前本机测试构建号为 17，最低 iOS 15。此前 iPhone 14 的 build 11 结果保留为历史证据；不能据此宣称 iOS 15/16 真机安装已经通过。

## 已实现

- 统一助手按顺序执行手机 App、手机内置浏览器和兼容的 chrome-use site 脚本，并传递已观察到的结果。
- 本机助手不会自动把任务交给电脑。网站脚本复用 chrome-use 的元数据与原始函数；需要 Node、电脑或文件的脚本不在手机上执行。
- cookie-use v1/v2 `.cusession` 解密、按账号隔离的 Cookie/localStorage、书签、历史和填写资料；凭据与任务日志在手机加密保存。
- iOS 17+ 使用独立持久网站存储；iOS 15/16 使用隔离会话并恢复已选网站的数据。IndexedDB 等数据不保证跨重启恢复。
- 浏览器完成前独立核验当前页面并再次读取现场，拒绝未经核验的完成声明。
- 持久化请求 ID、动作前检查点、暂停/恢复和未知写入的人工核对。用户已停止的任务不会在核对后继续剩余步骤。
- 同一个实时活动跟随跨工具步骤，显示当前操作和下一步；点击兼容活动回统一助手。
- Mac 导出助手与可选的独立 Chrome 授权 API：指定 profile/域名、脚本指纹、独立加密请求、操作前保存执行记录，拒绝重复写入和隐式读取 `@file`。
- 配置兼容 `IPHONE_USE_*` 与旧 `PHONE_REMOTE_*`；setup 从新版 plist 读取正确端口与认证信息。

## 验收证据

| 项目 | 结果 |
| --- | --- |
| 服务端回归 | 655 通过，1 跳过 |
| MCP 回归 | 162 通过，1 跳过 |
| 加密、授权与去重 | 7 通过，包括独立生成的 v1 互操作数据和禁止 `@file` 展开 |
| iOS 基础相关回归 | 50 通过，含免费模型实际网页任务、旧版会话恢复、脚本执行、停止后的恢复边界 |
| 独立提交树补充验证 | 18 项会话/恢复边界测试与 8 项浏览器测试通过，包含早报完成的反例和免费模型实际任务 |
| iPhone 14 独立浏览器验收 | build 11 真机再次通过：实际输入值 `Phone Use`，实际段落 `Form updated`，统一任务状态 `done` |
| iPhone 14 会话解密 | 真机通过：build 11 合成 v1 会话解密约 133 ms；导入一个隔离账号及一个合成 Cookie |
| iPhone 17 本机任务 | 从 App 提交并完成读取电量；20.258 s，模型 `mimo-v2.6-flash-free`，原生请求 ID 有返回 |
| iPhone 17 跨工具连续任务 | 最新 build 15 与 v0.17.18 后端通过，详见下方；较早的设备争用结果不算通过 |
| iPhone 14 跨 App 自动化 | 等待 USB：Wi-Fi 启动 XCTest 返回 74，testmanagerd 未提供 IDE 通道；不要重复 Wi-Fi 重试 |

真机浏览器验收采用 DEBUG 专用隔离模式，只运行本地合成页面和新的临时配置，不修改生产草稿或真实登录状态。报告由手机生成并读取实际 DOM 后写入：`Documents/acceptance/browser-result.json`。已恢复普通 App 启动模式。

## 后续步骤

iPhone 17 当前通过 Wi-Fi 连接，用户没有 USB 线。确认允许分支暂缓稳定性复测。iPhone 14 留给独立会话；iOS 15/16 实机安装、App 遥控手动录制及另一台 iPhone 控制仍需验收。

不联系已耗尽 token 的 Claude 会话。真实 Chrome 登录状态迁移仍需用户选择 profile 与网站；本轮只使用合成会话。

## iPhone 17 最新同步与验收

已同步 `origin/main` 的 `aa1679c`（源码版本 v0.17.18），包括系统弹窗手势保护和 Codex 截图内容修复；整合提交为 `69829e8`。手机网站目录交接提交 `33cd121` 已包含在分支内。自行启动 runner 的实验分支没有混入正式 App。

- App build 15 与手机、Mac v0.17.18 后端真机通过 `browser → phone → browser`。实际字段为 `Phone Use`，段落为 `Form updated`，状态 `done`，原生任务 ID `t-49188846d1`。报告：`/tmp/iphone17-build15-cross-result.json`。
- 报告只记录一个实时活动 ID `0A7F6E62-4B78-4873-BBAD-7134D2B71373`，跨工具自动暂停与恢复复用同一活动。下一步已观察到实际阶段目标，部分模型目标为英文，尚未保证全部中文。
- continued processing 在较早真机测试中导致原生请求取消。默认改为 ActivityKit，DEBUG 可显式启用 continued processing。iOS 15 使用 App 内状态，iOS 16.1+ 支持实时活动。
- 自动恢复回归：13 项执行，12 通过、1 项联网下载跳过。独立网站目录验证另有 8 项通过，包括公开包下载（160 可用、18 不支持）；日志已读回。
- v0.17.18 回归：server lib 694 通过、1 忽略；MCP lib 169 通过、1 忽略；core lib 23 通过。手机嵌入库已编译，managed setup 返回 0。
- 最新 App build 17 已编译并安装。风险确认自动弹出系统对话框，收起键盘，并按任务 ID、提示内容及步骤数绑定回应，防止旧确认作用于新任务。拒绝按钮使用普通按钮，避免被系统隐藏为取消项。
- build 17 普通 App 真机拒绝分支通过：`t-773d8cef2d` 从手机操作回到 App 显示确认，拒绝后 `blocked`、0 步，模型 `mimo-v2.6-flash-free`。日志 `/tmp/iphone17-risk-ui-build17-test.log`。测试仅使用 Home 动作的合成风险 flow；任务准备导航不等于执行 flow。
- 允许分支没有取得有效通过证据。Wi-Fi runner 多次连接中断，用户没有 USB 线，因此停止重试。build 17 的完整跨工具流程也未重跑，不能把 build 15 的报告算作 build 17 全量验收。
- 新启动的 v0.17.18 MCP 对 Codex 客户端返回有效内联 PNG（242002 字节），报告 `/tmp/iphone17-mcp018-image-result.json`。没有改写其他会话的 MCP 配置。
- 交还前 Mac 状态报告 v0.17.19、`drivable=true`、`owner=null`；本轮后端回归证据仍对应 v0.17.18，未把运行时版本变化视为 v0.17.19 全量验收。
- 已恢复手机服务仅监听 `127.0.0.1`，恢复操作日志 `/tmp/iphone17-restore-loopback.log` 返回 0。临时合成网页服务和 USB 观察 relay 在交还设备前关闭。设备转交独立控制实验会话；Wi-Fi 稳定性仍需观察。

没有采集或迁移真实 Chrome 登录资料。手机保留本轮 Home-only 私有 QA flow，不发布到公共目录。旧版硬件安装、App 遥控手动录制和另一台手机控制仍待独立验收。

本轮编译两次遭遇 ENOSPC，均作为失败保留；恢复了被磁盘中断的 fast-forward，核对工作树完全匹配整合提交后才恢复 Git 索引与分支。只清理可再生成的本项目缓存，保留签名设备 App、测试日志与实际报告。重型 Rust 仍通过 heavy-cargo 串行限流；缓存服务挂起时，本次构建临时跳过缓存。
