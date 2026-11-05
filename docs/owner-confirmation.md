# 主人签名确认

HTTP 密码、agent token、Cookie 仅授权普通 API，不代表主人同意。主人在 iPhone Use App 的设备设置 → 主人确认中，点击「绑定为主人设备（面容 ID）」。App 使用 Secure Enclave P-256 私钥；访问控制为 privateKeyUsage + biometryCurrentSet，不回退设备密码。更改生物识别登记后原密钥失效。

威胁模型不包括能在本机执行任意 shell 的程序；这类程序能修改二进制、主人公钥和流程许可文件。首次绑定采用现有 HTTP 鉴权下的 TOFU，因此主人必须在向不受信任客户端共享 HTTP 凭证前完成绑定；TOFU 本身不能防止抢先绑定。

公钥格式为 base64 X9.63 未压缩 P-256 点。首次 POST /owner/key 的 signature 是新钥对 `iphone-use-owner-key/v1\n` 加 base64 公钥的 DER ECDSA SHA-256 签名，以证明持有私钥。已绑定时，同一消息必须由旧钥签名，否则 409。GET /owner/key 返回绑定状态和 SHA-256 公钥指纹。状态保存为实例 state_dir 下的 owner-key.json（0600，原子替换）。生物识别变化导致旧钥不可用时，由能访问本地 state_dir 的主人手动恢复绑定。

GET /owner/pending 返回最多 16 张待确认单。每单有随机 id、nonce、kind、规范化 JSON 的 SHA-256 digest、daemon 生成的摘要、创建/过期时间和对象绑定。调用方标签仅以截断后 JSON 引号内的引用出现。批准需 POST /owner/confirm/<id>：

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

输入分发复用已有 apps/list、成功 launch_app 和 /source 中明确 bundleId 的近期前台证据（2 秒），不为每次动作新增 runner 请求。确认前台是遥控 App 时拒绝；前台未知或证据过期时放行。遥控 App 的默认 bundle com.leeguoo.iphone-use.remote 及其签名后缀都受保护，直接 launch_app 打开该 bundle 也拒绝，返回 owner_app_protected。保护独立于敏感步骤许可；它是尽力而为的第二层防线，主防线仍是生物识别签名。屏幕变更派发或 session 失效清除缓存。

App 前台期间每三秒查询待确认单，并发送本地通知。主人确认界面显示 daemon 摘要；「允许」调用生物识别保护的私钥签名，「拒绝」不触发签名。任务界面的同意按钮查找对应任务确认单并使用相同签名通道。

验收限制：此工作环境无法解析 crates.io，heavy-cargo 和 clippy 在缺少 fs2 时即退出，未进入 Rust 编译；离线模式同样缺少 fs2。XcodeGen 可生成工程，但 xcodebuild 的资产编译因 CoreSimulator runtime 服务不可用而失败。新增 OwnerConfirmation.swift 已单独使用 iPhoneOS SDK、Swift 6 严格并发做类型检查。全仓 cargo fmt --check 还报告已有文件的格式差异；未格式化与本任务无关的文件。

弹窗的指定 button、默认 accept 和 dismiss 均在守卫阶段读取正文和完整有序按钮列表，批准绑定正文、完整按钮列表及目标按钮。默认 accept/dismiss 分别取 runner 按钮顺序的最后/第一个，并按该名称派发，确保执行目标与确认对象一致。详情按引用转义规则展示正文、目标和按钮列表。正文或列表变化须重新确认；读不到完整弹窗身份不能消费批准，显式付款按钮仍按付款语义拦截。

/control 输入是否隐藏只依据守卫实际读到控件的 secure 判定，请求的 secure、target、locator 等声明不能隐藏普通输入。流程保存时无法观察目标，确认详情一律显示输入明文（流程文件原本也存储这些值）。引用除控制字符、行/段分隔符外，还转义 U+00AD、U+061C、U+180E、U+200B–U+200F、U+202A–U+202E、U+2060–U+2064、U+2066–U+206F、U+FEFF、U+FFF9–U+FFFB，防止不可见格式字符改变确认界面显示。
