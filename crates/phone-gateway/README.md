# phone-gateway

Mac 主机上的 TLS 1.3 手机入口与本机 HTTP 网关。设计依据：[self-runner-remote.md](../../docs/design/self-runner-remote.md) 第 2–5、8 节，冲突时以第 11.A/C/D/F 节为准。手机主动连接；此 crate 不部署手机 App、不安装 launchd、不下发配对文件。

```sh
cargo run -p phone-gateway --offline -- pair --host '<mac-host>.local' --out /tmp/gateway-pair.json
cargo run -p phone-gateway --offline -- serve
cargo run -p phone-gateway --offline -- cmd status
cargo run -p phone-gateway --offline -- cmd tap --args '{"label":"靶按钮"}' --deadline-ms 10000
cargo run -p phone-gateway --offline -- cmd screenshot --save-screenshot /tmp/screen.png
cargo run -p phone-gateway --offline -- cmd read --target system --format compact
cargo run -p phone-gateway --offline -- cmd wait_for --target foreground --args '{"label":"Ready"}'
cargo run -p phone-gateway --offline -- cmd status --endpoint 127.0.0.1:17382
cargo run -p phone-gateway --offline -- cmd screenshot --format compact
```

`pair` 默认 host 为 localhost（实际手机配对请指定 Mac 主机局域网主机名/IP），port 为 7381，输出默认位于配置目录 `gateway-pair.json`。已有配置默认复用，只有 `--rotate` 才重新生成全部秘密；轮换时应先停服务并让手机重新配对。配对 JSON 含 PSK，应通过 devicectl 下发，然后由手机存入 Keychain 并删除导入文件（第 11.C 节）。终端只打印输出路径，不打印配对秘密。

配置目录默认 `~/.config/phone-gateway/`，可用 `PHONE_GATEWAY_HOME` 覆盖。目录 0700，文件从创建起为 0600：`server.crt`、`server.key`、`psk`（base64）、`device_id`（32 位小写 hex）、`client-token`（base64）。使用系统 `/usr/bin/openssl` 生成 P-256、CN=phone-gateway、3650 天自签证书。随机源为 `/dev/urandom`。配置残缺时拒绝默认复用，需要显式轮换。轮换由多个文件 rename 完成，不保证进程被中途终止时跨文件原子性。

`serve --bind 0.0.0.0:7381 --http 127.0.0.1:7382`：HTTP 地址必须是 loopback；`cmd --endpoint <host:port>` 指定 HTTP 入口，优先级为命令行 > `PHONE_GATEWAY_ENDPOINT` > 默认 `127.0.0.1:7382`。支持回环 IP、`localhost` 和 IPv6（如 `[::1]:7382`）；解析结果必须全部为回环地址，连接时只使用已校验的地址。格式错误、解析失败或非回环地址均报错并退出 64。持有 client-token 的任何程序都有现有动作白名单内的控制权（不含主人签名确认），调用方的审批不能约束其它持有 token 的程序（第 11.C 节）。只接受 POST、不开 CORS，两条路由均需 Bearer token：

- `/v1/command`：`{"action":"read","args":{},"deadline_ms":30000,"id":"可选稳定ID"}`，默认 deadline 30 秒，id 默认 UUID。动作限于设计第 5 节白名单；结果是否支持由手机决定。id 最多 128 字符，可含 ASCII 字母、数字、`-_:`, gateway 不缓存/去重，重复 id 透传手机。
- `/v1/status`：不需要手机在线，返回 `transport`、`last_pong_at`（Unix 毫秒或 null）、`device_id`（前 8 位）、`queue_length`。手机 XCTest/VPN 状态应使用 command 的 `status`，两层状态不同（第 11.D 节）。

token 在读取 body 前校验，body 最大 1 MiB。FIFO 排队上限 8（不计正在执行的一个请求），同一时刻仅派发一个；deadline 从 HTTP handler 收到请求起算，含读取 body 和排队，派发给手机的是剩余毫秒。派发前超时返回 `expired`；派发后由手机负责执行期限，gateway 保持执行闸直到响应或连接断开，断开/替换返回 `unknown`，永不自动重发。gateway 进程重启时 HTTP 连接也会消失，调用方应把未完成调用视为 unknown；纯内存实现无法在重启后追补旧 HTTP 响应（第 11.A 节）。

协议：4 字节大端长度 + UTF-8 JSON，认证前最大 4096 字节、认证后最大 16 MiB。仅 TLS 1.3；未认证连接最多 2，包含 TLS 握手在内限时 5 秒。challenge 为每连接独立的 32 字节随机 nonce（base64），一次性消费。

手机入口 accept 的暂时性错误会记录一行 stderr 日志并继续监听：ECONNABORTED/EINTR 立即重试，EMFILE/ENFILE/ENOBUFS/ENOMEM（以及 WouldBlock）退避 100 ms；不可恢复的监听错误才会从 accept 循环上抛并使服务退出。

HMAC 消息字节顺序为 ASCII `selfrunner-v1` + 解码 nonce 的 32 字节 + 配对文件中 device_id 的 **32 字符 UTF-8 hex 文本**（不再 hex 解码）；key 是解码 PSK 的 32 字节。hello 必须带 `t=hello`、`v=1`、匹配的 `device_id`、base64 `mac`；成功后回 welcome，才替换旧连接。每来源连接尝试至少间隔 100 ms，来源跟踪最大 1024 条。

主人确认使用独立的手机发起消息 `owner_req` / `owner_res`，仅在控制连接 welcome 后可用；`owner_pending` 每约 3 秒检查列表变化后推送。body_b64 封装原始 HTTP body，逐字节转发到本机 daemon 的 GET/POST `/owner/key`、GET `/owner/pending`、POST `/owner/confirm/<id>`，不生成、修改、缓存或重试签名。HTTP `/v1/command` 白名单不包含这些操作，client-token 不能调用它们。

启用配置：`PHONE_GATEWAY_OWNER_URL`（仅 http + 数字回环 IP + 根路径），以及 `PHONE_GATEWAY_OWNER_TOKEN_FILE` 或 `PHONE_GATEWAY_OWNER_TOKEN` 二选一。未配置/配置错误使主人请求返回 `owner_unconfigured` / `owner_config_invalid`；写动作按下述守卫规则失败关闭。请求体 16 KiB、owner 帧 24 KiB、响应体 256 KiB、每秒最多 4 请求（重连不重置）、每连接最多一个手机请求在途；HTTP 超时 3 秒，禁用代理、重定向和自动重试。只在线查询，不离线排队；断线取消任务，新连接只读当前 pending。完整协议、错误码、指纹及配置说明见 [主人确认](../../docs/owner-confirmation.md)。这些消息和凭据不进入动作日志。

gateway 每 15 秒发 `{"t":"ping","seq":N}`；仅未确认序号对应的 pong 能更新活性，已确认序号不能重放。45 秒未确认则断开。手机主动 ping 也回同序号 pong，但不会刷新 gateway 判活。

叶证书钉扎算法及 Swift 对照见 `src/fake_phone.rs:Pin::verify_server_cert` 文档注释：对**叶证书完整 DER** 做 SHA-256，比较配对文件 cert_sha256，不使用系统 CA、hostname 或 SPKI 替代。测试客户端还校验 TLS 1.3 握手签名。

`cmd <action> [--endpoint HOST:PORT] [--target foreground|system] [--args JSON] [--deadline-ms N] [--save-screenshot PATH] [--format json|compact] [--then-read]`：默认 `json` 保留完整响应。协议 `ok:true` 与 `ok:false` 均沿用原有退出码 **0**（包括本地 `invalid_args`）；命令行参数解析错误（含无效 endpoint、target、JSON 和 target 冲突）退出 **64**；HTTP/文件/解码等运行错误退出非零。调用方应检查 JSON 的 `ok` 或 compact 首行的 `ok/error`，不能仅凭退出码判断动作成功。

`--then-read` 仅适用于 `tap` / `swipe` / `type` / `press` / `launch` / `unlock`，且必须使用 `--format compact`；其它组合报错退出 64，在读取 client-token 前拒绝。动作收到协议响应后（包括 `ok:false`）先照常输出动作结果，等待 400 ms，再向同一 endpoint 发送 args 为空、deadline 为 8000 ms 的前台 `read`。随后输出分隔行 `--- screen after <action>` 和与 `cmd read --format compact` 相同的读屏内容。跟随 read 的 HTTP 调用或渲染失败时，分隔行后输出 `error read_unavailable 0ms`，仍退出 0，不改变动作结果。本地 `invalid_args` 或动作 HTTP 调用失败时不跟随读屏；unlock 沿用闭合响应，跟随 read 不携带密码相关字段。

`--target` 仅支持 `read` / `wait_for`，等价于向 args 加入 `"target":"foreground"` 或 `"target":"system"`。与 `--args` 合并并保留其它字段；已有 target 同值可合并，异值（包括非字符串）报错退出 64。与 `--target` 同用时 `--args` 必须为 JSON 对象。其它动作使用 `--target` 或传入其它 target 值也退出 64。

`compact` 首行为 `ok <action> <ms>ms` 或 `error <code> <ms>ms`，耗时为 CLI 的 HTTP 调用耗时（含网关排队，不含保存截图）。本地参数校验失败为 `0ms`。result 中除节点和图片外的字段以单行 JSON 跟随，最多 2000 个 Unicode 字符；超长时直接截断，不能保证截断后的行仍是合法 JSON。

`read` / `wait_for` 首行后输出 `pid=<pid> snapshot=<snapshot_id> truncated=<bool> nodes=<总数> shown=<行数>`。nodes 为收到的数组长度（wait_for 的单个 `result.node` 计 1），shown 为最多 400 个有非空 label、identifier 或 value 的节点。`truncated` 沿用手机字段，不代表 CLI 的 400 行裁剪；超限尾行 `... <n> more` 计未展示的有文字节点。`wait_for` 当前只返回 found/node，缺失元数据展示 pid=null、snapshot=空、truncated=false。节点使用原始 `i`，缺失时使用数组下标，不因过滤重新编号。格式为 `[i] Type "label" id=identifier val="value" @x,y,w,h`；文本移除 Unicode 格式字符和零宽字符，换行/控制字符转空格，各截断至 80 个 Unicode 字符（identifier 同样处理），label/value 使用 JSON 引号转义，frame 四舍五入为整数。

类型映射以本机 Xcode `XCUIAutomation.framework/Headers/XCUIElementTypes.h` 为准：Other=1、Application=2、Window=4、Button=9、NavigationBar=21、Switch=40、Image=43、SearchField=45、StaticText=48、TextField=49、SecureTextField=50、Cell=75；其它写 `t<n>`。

```text
ok read 12ms
pid=37 snapshot=snap-1 truncated=false nodes=2 shown=1
{"bundle":"com.apple.Preferences"}
[1] Button "通用" id=general val="" @16,200,361,44
```

截图结果图片字段尚未在设计里命名，CLI 支持 `result.image/base64/png/data` 的 base64 字符串。json 下指定保存路径后替换为 `{"saved":"路径","bytes":N}`，未指定路径时保持原始 JSON。compact 成功截图总会保存，未指定 `--save-screenshot` 时保存到 `$TMPDIR/phone-gateway/shot-<随机UUID>.png`（TMPDIR 未设时使用系统临时目录），自动目录 0700、图片 0600；可用 `--save-screenshot PATH` 指定已有父目录内的路径。compact 只输出 `ok screenshot <ms>ms saved=<path> <width>x<height>`，绝不打印 base64；compact 失败截图只按常规 error 格式输出，不尝试保存。json 加 `--save-screenshot` 的失败响应沿用原行为：缺少图片 result 时 CLI 报错并退出非零。

日志到 stderr，记录白名单动作、id、耗时、经筛选的结果码，以及 accept 的系统错误和重试退避时间，不记录 args、PSK、token、截图或读屏文本。unlock 在两种输出格式下都沿用网关闭合响应，只转发固定布尔字段及允许的错误码，绝不输出密码或手机自由文本。

离线验收（build/test 必须使用全局或仓库 heavy-cargo）：

```sh
heavy_cargo=./scripts/heavy-cargo
[ ! -x ~/.local/bin/heavy-cargo ] || heavy_cargo=~/.local/bin/heavy-cargo
"$heavy_cargo" test -p phone-gateway --offline
cargo clippy -p phone-gateway --offline --all-targets -- -D warnings
"$heavy_cargo" build -p phone-gateway --release --offline
```

测试全部使用临时配置目录；真实 TLS 集成测试需要本地监听权限。假手机客户端在 `src/fake_phone.rs`，可用来对照 Swift 与离线验证，测试不依赖手机。CLI 集成测试使用 `127.0.0.1:0` 自动分配端口并传入 `--endpoint`，不会占用生产 gateway 的 7382 端口。launchd 可设置 ProgramArguments 为可执行文件路径和 `serve`，KeepAlive=true、RunAtLoad=true；仅文档化，不自动安装。

`cmd unlock --args '{"timeout_ms":4000}'` 使用 Mac 登录钥匙串的 `phone-gateway.passcode`（account=完整 device_id）；密码仅允许 4–6 位数字，调用方不得传 passcode，不自动重试，日志/结果/缓存不含密码；交互写入命令和内存清零限制见设计文档「数字密码解锁」。

## Gateway 写动作守卫

`tap/type/swipe/press/launch/open_url` 出队并持有 gateway 单命令执行闸后、派发手机之前，请求 daemon `POST /guard/gateway-command`。前一条手机响应已结束；判定和主人批准消费期间持续占闸。复用 OWNER 回环配置和 Bearer 鉴权，禁代理、重定向、重试。daemon 总预算 8 秒，单次完整读屏预算 2.5 秒；gateway 调用预算为 min(10 秒, 命令剩余 deadline)，期限含排队与判定。只有严格的 `200 {"allow":true}` 才派发。未配置返回 `guard_unconfigured`；配置错误、不可达、超时、非法响应返回 `guard_unavailable`（命令期限耗尽可返回 `expired`），均不派发。读动作和钥匙串 unlock 不变。

一次请求只取一次完整快照，保留 coverage、原始树和规范化行供 tap/type/swipe 复用。label/identifier 点按检查所有规范化匹配行及原始字段匹配所对应的行，忽略 index 的顺序；任一候选敏感即要求主人确认，绑定全部候选身份、原始树摘要、页面摘要和前台 bundle。不能对应到行或点按/滑动快照不完整时失败关闭。swipe 把手机绝对屏幕点按窗口大小换成 `/control` 归一化起止点，复用现有小位移按点按检查；缺参数或无法换算拒绝。长距离 swipe 与 `/control` 一致，这不是滑块支付保护。

输入对所有字段执行现有密码框检查。快照截断、coverage 未知/不完整或无法读到焦点字段归属时，要求主人确认；只有完整且焦点明确、无敏感字段的快照直接放行。press 只允许 home/音量键，以及名为「锁屏」的快捷指令或硬件锁屏路径；自定义 shortcut_name 和未知 press 参数均在 gateway 返回 `invalid_args`，daemon 同样校验。auto/shortcut 默认「锁屏」；hid/device_event/press_lock 为硬件路径。

敏感动作返回 `owner_confirm_required` 和 `owner_confirm_id`；请主人在手机 Self Runner 刷脸允许后，两分钟内原样重发同一命令。重发仍在出队后的闸内重新读屏判定并消费一次批准。完整 action/args、身份、页面或前台变化须重新确认。使用独立 `gateway_press` 能力池，待确认及已批准未消费合计最多 8 张，不占用 /control、任务、流程的 16 张配额。入口同一时刻只处理一个请求，多余直接返回 429 `guard_busy`。任务 confirm:"never" 和保存流程许可不变。

残余窗口仅为「daemon 判定读屏 → 手机执行」，与 daemon `/control` 的读屏→执行同级；不包含队列等待。gateway 给已判定 tap 强制设置内部 `gateway_guarded` 标志，手机遇遮挡立即返回 `occluded`，不再等待、自动上滑或重新定位。`expect_label/expect_identifier` 仍是执行后等待条件。手机本地 `snapshot_id` 无法由 daemon 解析，带该字段的写请求失败关闭。威胁模型不包含本机任意 shell 直接连接 runner 转发端口。
