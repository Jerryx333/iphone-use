# 主人签名确认

HTTP 密码、agent token、Cookie 仅授权普通 API，不代表主人同意。主人在 iphone-use 自己的手机 App「Self Runner」→ 主人确认中，点击「绑定为主人设备（面容 ID）」。App 使用 Secure Enclave P-256 私钥；访问控制为 privateKeyUsage + biometryCurrentSet，不回退设备密码。更改生物识别登记后原密钥失效。

威胁模型不包括能在本机执行任意 shell 的程序；这类程序能修改二进制、主人公钥和流程许可文件。首次绑定采用现有 HTTP 鉴权下的 TOFU，因此主人必须在向不受信任客户端共享 HTTP 凭证前完成绑定；TOFU 本身不能防止抢先绑定。

公钥格式为 base64 X9.63 未压缩 P-256 点。首次 POST /owner/key 的 signature 是新钥对 `iphone-use-owner-key/v1\n` 加 base64 公钥的 DER ECDSA SHA-256 签名，以证明持有私钥。已绑定时，同一消息必须由旧钥签名，否则 409。GET /owner/key 返回绑定状态和 SHA-256 公钥指纹。状态保存为实例 state_dir 下的 owner-key.json（0600，原子替换）。生物识别变化导致旧钥不可用时，由能访问本地 state_dir 的主人手动恢复绑定。

GET /owner/pending 返回普通入口最多 16 张及 gateway 独立入口最多 8 张待确认单。每单有随机 id、nonce、kind、规范化 JSON 的 SHA-256 digest、daemon 生成的摘要、创建/过期时间和对象绑定。调用方标签仅以截断后 JSON 引号内的引用出现。批准需 POST /owner/confirm/<id>：

```json
{"approve":true,"signature":"base64 DER signature"}
```

签名消息（无末尾换行）为：

```text
iphone-use-owner-confirm/v1
<id>
<nonce>
<hex digest>
<kind>
```

Swift 使用 SecKeyCreateSignature(.ecdsaSignatureMessageX962SHA256)，daemon 用 ring ECDSA_P256_SHA256_ASN1 校验。签名尝试即消费确认单，签错、过期、重放都失败。拒绝不需签名。未绑定时提示先绑定主人设备；不发放敏感权限。

流程 grant 绑定 daemon 读出的保存流程及其输入，另绑定确切回放请求。未获主人签名的流程逐步执行敏感守卫；命中敏感步骤时返回 owner_confirm_id。主人确认过的流程对象摘要保存于 owner-approved-flows.json；更改内容或输入会产生不同摘要。既有 sensitive_guard::installed_flow_grants_are_bound_and_single_use 测试增加主人签名准备步骤，原有成功、过期、内容不符和一次性断言保持不变。

任务进入 needs_confirm 时创建带任务 id、当前操作及独立确认序号的确认单。agent 的 approve:true 始终返回 403。手机签名确认后 daemon 核对任务仍在等待同一问题，再在任务答案锁内核对当前问题并调用 answer_owner；confirm:"never" 不生成确认单，保留既有行为。任务放行只校验进程内令牌及有效期，不比对标签（标签仅供日志）；令牌仅附在任务当前步骤的 DaemonClient 上，步骤结束清除且只能消费一次。无令牌或错令牌不能按标签获得权限。

/control 在请求级和执行前检查敏感动作。确认能力绑定除 issued_at_ms/ttl_ms 外的完整控制消息、守卫实际读到的控件身份（label、identifier、rect）、整屏按树顺序排列的 (kind, label, value) 的 SHA-256 页面摘要（不含 rect），以及 daemon 已知的近期前台 bundle（未知绑定 null）。执行前重新解析；身份或前台变化，包括未知与已知互变，都需要重新确认。被拦后清除树缓存并重新读取；金额、收款方、订单号等任意页面文字变化均使旧批准不匹配，重新出单。详情显示「页面内容摘要」前八位。签名后两分钟内可消费一次。重发时执行字段必须保持一致，issued_at_ms/ttl_ms 可变化；服务器 freshness 仍按收到请求后的单调时钟检查。intent 忽略 operator_confirmed；operator 类型能力绑定 name + args，同样只能消费一次。

输入分发复用已有 apps/list、成功 launch_app 和 /source 中明确 bundleId 的近期前台证据（2 秒），不为每次动作新增 runner 请求。确认前台是主人 App 时拒绝；前台未知或证据过期时放行。遥控 App 的默认 bundle com.leeguoo.iphone-use.remote、Self Runner 的默认 bundle dev.iphoneuse.selfrunner.spike 及其点分签名后缀都受保护；自定义 Self Runner bundle 读取守护进程现有环境变量 IPU_SELFRUNNER_BUNDLE_ID（也保护其点分后缀），直接 launch_app 打开该 bundle 也拒绝，返回 owner_app_protected。保护独立于敏感步骤许可；它是尽力而为的第二层防线，主防线仍是生物识别签名。屏幕变更派发或 session 失效清除缓存。

主人确认入口是 Self Runner，不依赖遥控 App / Heya。手机通过已有 TLS 1.3 + PSK 控制连接发起下面的主人请求，gateway 代转到本机 daemon。既有 WSS 备用路由也经过相同的 welcome 认证边界。gateway 不生成或修改签名、不自动批准、不缓存签名、不自动重试；失联或超时表示结果可能未知，手机提示刷新，不能重放批准请求。

## 手机链路与 gateway 配置

仅在控制连接完成 hello/welcome 后接受（tunnel 连接不接受）以下长度前缀 JSON 帧：

```json
{"t":"owner_req","id":"请求关联 UUID","method":"POST","path":"/owner/key","body_b64":"原始 HTTP body 的标准 base64"}
{"t":"owner_res","id":"请求关联 UUID","status":200,"body_b64":"原始响应 body 的标准 base64"}
{"t":"owner_res","id":"请求关联 UUID","error":{"code":"owner_unconfigured"}}
{"t":"owner_pending","body_b64":"GET /owner/pending 原始响应 body 的标准 base64"}
```

`id` 是请求关联 ID，和 path 中的确认单 ID 不同；1–128 个 ASCII 字母、数字、`-_:`, confirm path 不接受查询串、百分号、斜线或点路径。允许的 method/path 仅为 GET/POST `/owner/key`、GET `/owner/pending`、POST `/owner/confirm/<id>`。GET 的 body_b64 是空字符串。请求/响应 body 在 base64 解码后逐字节转发，HTTP 状态码一并返回，不把 JSON 解析后重编码；HTTP 头和分块编码不属于 body。daemon 的 4xx/5xx 响应同样透传。

gateway `serve` 从环境读取（均无默认凭据）：

- `PHONE_GATEWAY_OWNER_URL`：例如 `http://127.0.0.1:18790`，填写实际实例地址；只允许 http + 数字回环 IP（支持 `[::1]`），不得含用户信息、非根路径、查询串、fragment。禁用代理、重定向和自动重试。
- `PHONE_GATEWAY_OWNER_TOKEN_FILE`：保存该 daemon Bearer 密码的 UTF-8 文件，最多 8192 字节，移除末尾 CR/LF。建议使用已有 0600 密码文件，不把密码写进 shell 命令、仓库或日志。
- `PHONE_GATEWAY_OWNER_TOKEN`：与 TOKEN_FILE 二选一，直接提供 Bearer 值。两者同时设置属于配置错误。

未配置返回 `owner_unconfigured`，部分/非法配置返回 `owner_config_invalid`；不妨碍普通动作。访问失败返回 `owner_daemon_unavailable`，错误不携带 URL、文件路径、密码或底层异常。请求体最多 16 KiB、owner 请求帧最多 24 KiB、响应体最多 256 KiB；超限分别为 `owner_request_too_large` / `owner_response_too_large`。每条连接最多一个手机请求在途（否则 `owner_busy`），gateway 全局每秒最多 4 个，超限返回 `owner_rate_limited` 并断开该连接；重连不重置频率预算。HTTP 超时 3 秒。现有 `/v1/command` 白名单没有新增主人操作，client-token 不能调用主人转发；gateway HTTP 也没有 `/owner/*` 路由。

gateway 仅在已认证控制连接在线期间每 3 秒查询 pending，按解析后的列表摘要判断变化；变化时携带完整原始响应体推送（包括清空列表）。连接结束取消查询/请求并丢弃连接内摘要，不离线排队、不重放历史帧；重连只查询 daemon 当前列表。第一次查询推送当前快照。签名和 pending body 不落盘，变化判断只保留 SHA-256 摘要。

Self Runner 前台收到新单直接弹出主人确认页；后台发不含详情/nonce 的本地通知，点开将对应单排在详情页首位并重新刷新。首次打开主人确认请求通知权限，也可手动启用。关闭通知权限或 iOS 挂起 App 时无法保证后台送达，打开页面可手动刷新。断线移除本地待确认状态/通知；daemon 的 pending 是唯一事实来源。

页面显示完整 summary 与所有 details，可滚动、不做客户端截断；daemon 已对不可信字段做的引用/截断保持原样。显示 daemon 与本机公钥的 SHA-256 小写 hex 指纹。新建密钥必须是 Secure Enclave P-256，访问控制 `.privateKeyUsage + .biometryCurrentSet`；每次绑定/允许使用新的 LAContext，只接受 `.deviceOwnerAuthenticationWithBiometrics` 且 biometryType 必须为 faceID，隐藏密码回退入口，随后使用同一 context 调用 SecKey 签名并立即使 context 失效。没有软件密钥或设备密码回退。「拒绝」不读私钥、不签名。另一把钥匙已绑定时不覆盖；换钥仍由既有协议要求旧钥签名，此 UI 不提供换钥流程。过期、消费、签错等 daemon HTTP 错误在页面原样显示，不写入 selfrunner.log / NSLog；签名请求绕开动作缓存和日志。

## 第二层防线

手机上的 RemoteActions 在入口及现有执行期限检查点检查确认页展示状态；确认页展示且 Self Runner 自身是前台、指定 bundle 或缓存 snapshot 的目标 PID 时，远程 tap/type/swipe/press/launch/open_url/unlock 返回 `owner_app_protected`。Face ID 导致 App 暂时 inactive 时仍保护。内嵌 RunnerCore 的远程变更入口也通过宿主回调使用相同保护，覆盖 daemon 经 8100 隧道发送的动作；普通 XCTest 宿主不安装此回调。既有本地自测按钮不经过远程守卫。

daemon 的 bundle 保护保持独立于敏感许可。已核实 setup 托管恢复 `setup/flow.rs:selfrunner_launch` 直接用 devicectl 打开 App，未经过 `/agent/input` / control 守卫，因此继续允许正常恢复；不为 agent 增加恢复豁免。第二层防线仍是尽力而为，无法消除跨进程前台证据和动作执行之间的竞争；主边界是 Face ID 保护的签名。

此前遥控 App 实现的验收记录（非本次 Self Runner 验收结果）：当时工作环境无法解析 crates.io，heavy-cargo 和 clippy 在缺少 fs2 时即退出，未进入 Rust 编译；离线模式同样缺少 fs2。XcodeGen 可生成工程，但 xcodebuild 的资产编译因 CoreSimulator runtime 服务不可用而失败。新增 OwnerConfirmation.swift 已单独使用 iPhoneOS SDK、Swift 6 严格并发做类型检查。全仓 cargo fmt --check 还报告已有文件的格式差异；未格式化与本任务无关的文件。

弹窗的指定 button、默认 accept 和 dismiss 均在守卫阶段读取正文和完整有序按钮列表，批准绑定正文、完整按钮列表及目标按钮。默认 accept/dismiss 分别取 runner 按钮顺序的最后/第一个，并按该名称派发，确保执行目标与确认对象一致。详情按引用转义规则展示正文、目标和按钮列表。正文或列表变化须重新确认；读不到完整弹窗身份不能消费批准，显式付款按钮仍按付款语义拦截。

/control 输入是否隐藏只依据守卫实际读到控件的 secure 判定，请求的 secure、target、locator 等声明不能隐藏普通输入。流程保存时无法观察目标，确认详情一律显示输入明文（流程文件原本也存储这些值）。引用除控制字符、行/段分隔符外，还转义 U+00AD、U+061C、U+180E、U+200B–U+200F、U+202A–U+202E、U+2060–U+2064、U+2066–U+206F、U+FEFF、U+FFF9–U+FFFB，防止不可见格式字符改变确认界面显示。

## Gateway 写动作（#387）

`tap/type/swipe/press/launch/open_url` 出队并持有 gateway 单命令执行闸后、派发手机之前，请求 daemon `POST /guard/gateway-command`。前一条手机响应已结束；判定和主人批准消费期间持续占闸。复用 OWNER 回环配置和 Bearer 鉴权，禁代理、重定向、重试。daemon 总预算 8 秒，单次完整读屏预算 2.5 秒；gateway 调用预算为 min(10 秒, 命令剩余 deadline)，期限含排队与判定。只有严格的 `200 {"allow":true}` 才派发。未配置返回 `guard_unconfigured`；配置错误、不可达、超时、非法响应返回 `guard_unavailable`（命令期限耗尽可返回 `expired`），均不派发。读动作和钥匙串 unlock 不变。

一次请求只取一次完整快照，保留 coverage、原始树和规范化行供 tap/type/swipe 复用。label/identifier 点按检查所有规范化匹配行及原始字段匹配所对应的行，忽略 index 的顺序；任一候选敏感即要求主人确认，绑定全部候选身份、原始树摘要、页面摘要和前台 bundle。不能对应到行或点按/滑动快照不完整时失败关闭。swipe 把手机绝对屏幕点按窗口大小换成 `/control` 归一化起止点，复用现有小位移按点按检查；缺参数或无法换算拒绝。长距离 swipe 与 `/control` 一致，这不是滑块支付保护。

输入对所有字段执行现有密码框检查。快照截断、coverage 未知/不完整或无法读到焦点字段归属时，要求主人确认；只有完整且焦点明确、无敏感字段的快照直接放行。press 只允许 home/音量键，以及名为「锁屏」的快捷指令或硬件锁屏路径；自定义 shortcut_name 和未知 press 参数均在 gateway 返回 `invalid_args`，daemon 同样校验。auto/shortcut 默认「锁屏」；hid/device_event/press_lock 为硬件路径。

敏感动作返回 `owner_confirm_required` 和 `owner_confirm_id`；请主人在手机 Self Runner 刷脸允许后，两分钟内原样重发同一命令。重发仍在出队后的闸内重新读屏判定并消费一次批准。完整 action/args、身份、页面或前台变化须重新确认。使用独立 `gateway_press` 能力池，待确认及已批准未消费合计最多 8 张，不占用 /control、任务、流程的 16 张配额。入口同一时刻只处理一个请求，多余直接返回 429 `guard_busy`。任务 confirm:"never" 和保存流程许可不变。

残余窗口仅为「daemon 判定读屏 → 手机执行」，与 daemon `/control` 的读屏→执行同级；不包含队列等待。gateway 给已判定 tap 强制设置内部 `gateway_guarded` 标志，手机遇遮挡立即返回 `occluded`，不再等待、自动上滑或重新定位。`expect_label/expect_identifier` 仍是执行后等待条件。手机本地 `snapshot_id` 无法由 daemon 解析，带该字段的写请求失败关闭。威胁模型不包含本机任意 shell 直接连接 runner 转发端口。
