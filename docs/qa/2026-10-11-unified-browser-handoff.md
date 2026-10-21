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
- 2026-10-11 允许分支因 Wi-Fi 中断没有通过证据；2026-10-12 已补过该分支，见下方。build 17 的完整跨工具流程仍未重跑，不能把 build 15 报告算作 build 17 全量验收。
- 新启动的 v0.17.18 MCP 对 Codex 客户端返回有效内联 PNG（242002 字节），报告 `/tmp/iphone17-mcp018-image-result.json`。没有改写其他会话的 MCP 配置。
- 交还前 Mac 状态报告 v0.17.19、`drivable=true`、`owner=null`；本轮后端回归证据仍对应 v0.17.18，未把运行时版本变化视为 v0.17.19 全量验收。
- 已恢复手机服务仅监听 `127.0.0.1`，恢复操作日志 `/tmp/iphone17-restore-loopback.log` 返回 0。临时合成网页服务和 USB 观察 relay 在交还设备前关闭。设备转交独立控制实验会话；Wi-Fi 稳定性仍需观察。

没有采集或迁移真实 Chrome 登录资料。手机保留本轮 Home-only 私有 QA flow，不发布到公共目录。旧版硬件安装、App 遥控手动录制和另一台手机控制仍待独立验收。

本轮编译两次遭遇 ENOSPC，均作为失败保留；恢复了被磁盘中断的 fast-forward，核对工作树完全匹配整合提交后才恢复 Git 索引与分支。只清理可再生成的本项目缓存，保留签名设备 App、测试日志与实际报告。重型 Rust 仍通过 heavy-cargo 串行限流；缓存服务挂起时，本次构建临时跳过缓存。

## 2026-10-12 继续同步

已 fetch 全部远端并将 `origin/main` 的 `78f2461` 合入，整合提交 `94e3639`。包含 v0.17.19、runner 端口占用恢复及设备诊断/录制/模拟器等新增能力。唯一合并冲突是同一调用的格式差异，已按主分支格式解决，保留 build 17 确认弹窗与实时活动修复。没有重新安装 App 或替换正在实验的 runner。

无需设备的检查：setup shim 15 项、客户端配置 7 项、agent-bench 检查通过。服务端/MCP 回归尝试在 rustup 代理阶段无输出；直接使用已安装工具链并跳过 sccache 后仍排队等待另一个 release 构建。本轮取消了自己的等待任务，没有计作回归通过。先前 886 项通过只对应 v0.17.18。

iPhone 17 最后明确分配仍属于独立实验会话。尚未收到结束、normal supervisor/VPN 恢复及设备交还确认，因此没有操作手机或补跑允许分支。请求状态已存入 OCS；后续编译诊断消息存储成功但唤醒因 IPC ECONNREFUSED 失败，不能当作已读确认。

## build 17 允许分支复测与换行查询修复

2026-10-12 独立实验会话明确交还设备，报告正常 v0.17.19 ready、phone daemon serving，supervisor 稳定库及 loopback 配置保留。随后发现 Pocket Gamepad 会话同时安装/启动 App，已协调独占窗口；没有把这次前台切换算作助手缺陷。

复测尚未提交新的流程任务。旧 QA 草稿值为 `\n0342e6。`。snapshot-bound 的输入框操作返回 `wda_pre_dispatch_failed`、`outcome=not_sent`、`retry_safe=true`。直接向同一原生 runner 做只读查询确认：NSPredicate 不能解析含原样换行的字符串；把换行写为 `\\n` 后，同一 TextField 查询返回一个匹配。

已修改 `wda_predicate_literal`，在反斜杠和单引号之后转义 LF、CR、TAB，并添加多行值查询回归。本轮 Rust 回归及修复后的本地 CLI 构建还在运行，没有提前记录通过。现有文件存在大量旧格式差异，全文件 rustfmt check 未通过；本轮未扩大格式修改。

手机临时交给 Pocket Gamepad 会话约 15 分钟；我的控制租约已释放，USB 观察 relay 已停止，未变更 VPN 或稳定嵌入库。等待对方明确结束后，用修复后的服务继续 App 的允许分支。证据目录 `/tmp/iphone17-build17-approval-evidence`。

## 2026-10-12 最新确认验收

换行修复提交 `db9897c` 的回归已结束：server lib 820 通过、1 忽略；MCP lib 208 通过、1 忽略，包含 `locator_wda_query_escapes_multiline_field_value`。日志 `/tmp/iphone-continue-regression.log`。本地 CLI 编译通过，已复制、签名并更新默认 Mac 服务，原 plist 备份保留在 `~/.iphone-use/local-builds/db9897c/daemon-original.plist`。

App build 17 普通模式允许分支已通过。输入完整观察后仅提交一次 `Run flow qa/confirm-nav-sync-dd0342e6.`，从手机操作回到 App 显示相同合成流程的确认，点击允许后，App 原生任务详情读回：完成、1 步 `RUN_FLOW` 成功、使用保存的 `qa/confirm-nav-sync-dd0342e6`、`assist: mimo-v2.6-flash-free`，约 14 秒。此次没有 Mac 侧原生 task JSON，也未记录原生任务 ID；证据来自 App 拉取的原生任务详情，不能假称取得原始 API 快照。

最初脚本只识别中文结果，漏掉英文完成文案；原失败报告保留，随后只读回同一任务详情确认，没有再次提交任务。验证报告 `/tmp/iphone17-build17-approval-evidence/verified-result.json`，原生详情 `native-task-detail.json`。

清理读回确认：草稿为空、执行方式恢复自动选择、租约释放、Mac drivable=true、phone daemon linked/serving/8787。没有启用勿扰（释放前 focus_engaged=false），没有修改沙盒账号或 VPN。

iOS 内嵌库的换行修复构建仍在隔离工作树运行，尚未安装，原稳定 `69829e8/libipu_embed.a` 保留。因此本次 App 允许验收使用更新后的 Mac 输入服务和原有手机嵌入服务，不能宣称全部手机服务已更新。

另已定位并提交认证源码修复：`4a02197` 补充 Apple 登录字段的中文邮件占位符，并在完成响应后释放单次 CLI 的自身租约。其他所有者拒绝及传输结果未知时不提前释放，避免服务器可能仍在填写凭据。新增隔离 HTTP/合成占位符测试已排队，未操作真实凭据、未部署该认证修复。

## 手机内嵌库更新与单 App 会话接管

`db9897c` 的 iOS 静态库以部署目标 15.0 编译通过，耗时 36 分 45 秒，保存在 `~/.iphone-use/local-builds/db9897c/libipu_embed.a`；构建元数据及 SHA256 保存在同目录 `native-build.json`。旧 `69829e8` 稳定库未覆盖。该库只包含换行查询修复，不包含后续认证提交 `4a02197`。

强制重建正常 runner 的 managed setup 返回 0，操作期间核验了 runner、relay、Mac drivable=true。supervisor 持久配置为新库路径、IPU_EMBED=1、IPU_DAEMON_HOST=127.0.0.1，原 plist 备份在同目录 `wda-original.plist`。日志 `/tmp/iphone-db9897c-managed-update.log`。

后续读回发现正常服务因 Mac 空闲释放策略停止，故 setup 当时通过不能视为持续运行通过。准备恢复时已有 `codex-single-app` 租约并重连；用户明确答复让该会话继续占用，已停止所有手机操作和恢复请求。最后稳定物理验收仍是前述 App 允许任务；新库安装后的持续运行及任务复验交由单 App 会话窗口再核对。

认证修复的隔离回归 `/tmp/iphone-auth-regression.log` 仍在运行，未部署该改动，也未操作真实凭据。

## 单 App 产品边界与认证回归结束

用户确认 `codex-single-app` 应继续占用 17；该会话说明正式功能将合入同一个 App，停止时先禁自动重连再停 NE，避免主进程退出造成重启循环。我方剩余测试不依赖 17 的旧 normal runner，已明确反馈不要为我恢复或重新安装第二个 App。旧配置及库只保留为磁盘上的回退资料，不启动。另向语音/主项目相关会话询问依赖，未收到答复前不代它们声明。

`4a02197` 的认证隔离回归结束：server lib 筛选 7 项通过、CLI 筛选 1 项通过，日志 `/tmp/iphone-auth-regression.log`。新用例覆盖中文/繁体邮件或电话占位符，以及本地合成 HTTP 响应成功、失败、其他所有者拒绝和请求接收后断线；后两种情况不释放未知/其他租约。没有读取真实凭据，没有进行真机认证，也没有部署该认证修复。

单 App 冷启动、后台维持及数据清理结果由该会话继续给出；它报告的 9 项测试、签名、真机 1.92 秒 smoke 与后台 169 秒分别保留其证据层级，不由本记录代作源码/硬件复验。最低 iOS 15/16 的 App 安装兼容及无灵动岛设备的 App 内状态约束仍保留，需独立覆盖。
