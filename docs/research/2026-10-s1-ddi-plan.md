# S1 — App 自行挂载 DDI：可行性与分阶段方案（2026-10-12）

> 来源：codex-sol 只读分析。主会话已核实：
> Mac 主机 DDI 为 17F42（Xcode 26.5；此前文档的 17F113 未在本机核实）；BuildManifest cryptex Variant 为
> `Cryptex Developer Disk Image`，与 `tss.rs:741` 期望的 `Developer Disk Image Cryptex` 不符；
> `tss.rs:17` TSS URL 为明文 `http://gs.apple.com/...`；`cryptexd.rs:274` persistence 取 nonce_persistence。
> 其余结论为该分析的源码推断，未经真机验证。`I/` 指 idevice patch rev 41c388b 的 cargo git checkout。

结论：**S1 有可实施的协议入口，但 iOS 27.0.1 上由普通开发签名 App 完成挂载仍「未核实」**。推荐先探测两条服务，再优先试 `mobile_image_mounter`；若真机能力和现有挂载信息指向 cryptex，再走 cryptex。不能把「Mac 预取一次 ticket」当作重启后离线启动的保证。

本次只读核对了指定文档、patch 源码和本机 DDI；未改文件、未连接设备、未构建、未联网。设备实测采用主会话已核实事实。

以下引用缩写对应实际文件：

- `I/` = `~/.cargo/git/checkouts/idevice-c5aa721665d26949/41c388b/idevice/`
- `M` = `I/src/services/mobile_image_mounter.rs`
- `C` = `I/src/services/cryptexd.rs`
- `A` = `I/src/services/cryptexd/assets.rs`
- `T` = `I/src/tss.rs`
- `D/` = `/Library/Developer/DeveloperDiskImages/iOS_DDI/`

**先纠正三个影响方案的差异**

1. 本机 `D/version.plist` 与 `D/Restore/BuildManifest.plist` 的 `ProductBuildVersion` 均为 **17F42**，不是研究文档 `docs/research/2026-10-self-runner.md:947` 所述 17F113。此前机器或素材版本是否不同：未核实。本报告体积均针对本机 17F42。
2. 本机 cryptex identity 的 `Info.Variant` 是 **`Cryptex Developer Disk Image`**；`T:739–763` 要求以 **`Developer Disk Image Cryptex`** 结尾。因此当前 `Cryptex1Assets::load` 会在选择 identity 时返回 `BadBuildManifest`，尚未进入设备操作。证据：`D/Restore/BuildManifest.plist`、`A:35–42`。
3. “`ddi_status` 检查失败”需区分：Rust 查询可成功返回 `mounted=false`，随后 Swift 拒绝启动，并非必然是 Rust 返回错误。证据：`crates/self-boot/src/lib.rs:265–278`、`spikes/self-runner-app/Sources/SelfRunnerSpike.swift:358–361`。

**1．两条入口的完整输入与素材**

| 项目 | `mount_personalized_rsd` | `cryptexd::install_ddi` |
|---|---|---|
| 已建立连接 | RSD provider、handshake、mounter client | RSD provider、handshake；每 routine 新连接 |
| 镜像 | `PersonalizedDMG` 字节 | `Cryptex1,GenericDmg` 字节 |
| TrustCache | `LoadableTrustCache` 字节 | `Cryptex1,GenericTrustCache` 字节 |
| Manifest | 完整 BuildManifest；TSS 回退时按 BoardId/ChipID 选 identity | `Cryptex1Assets.build_identity`；loader 从 BuildManifest 选择 |
| 其他素材 | `info_plist: Option<plist::Value>`，CLI 示例传 `None` | `CryptexInfoPlist` 原始字节、`GenericVolume` root hash |
| 设备输入 | 外部传入 `unique_chip_id`，作为 ECID；服务查询 identifiers 和 nonce | 服务查询 chip instance 和指定 nonce-domain handle 的 nonce |
| 签名 | 设备现有 personalization manifest，或 Apple 返回 `ApImg4Ticket` | Apple 返回 `Cryptex1,Ticket`，兼容回退 `ApImg4Ticket` |
| 写操作 | `ReceiveBytes` 上传，然后 `MountImage("Personalized")` | RemoteXPC install，传四份素材＋IM4M，共五个 payload |

证据：`M:574–595,637–768`、`C:380–433`、`A:15–59`；CLI 的 `None` 示例见依赖根目录 `tools/src/mounter.rs:197–204`。

可通过手机 RSD 获取：

- mounter：挂载列表、Developer Mode、personalization identifiers、`DeveloperDiskImage` nonce、按镜像 SHA-384 查询的现有签名。证据：`M:60–72,306–405,637–645`。
- cryptex：`img4_chip_*` identifiers、nonce、已安装 cryptex 列表。证据：`C:156–213`。
- **mounter identifiers 是否包含 ECID：未找到保证**；该自动入口仍要求调用方传入 ECID。CLI 用 lockdown `UniqueChipID` 获取它，见 `tools/src/mounter.rs:180–195`。
- cryptex 的 `img4_chip_ecid` 可用于 ECID；能否在这台手机的 tunnel 上读取：未核实。也可先检查 RSD properties，不能预设某键一定存在。证据：`C:156–166`、`T:720–736`、`I/src/services/rsd.rs:148–160`。

必须预置在手机、不能指望这些入口从 RSD 下载的内容：镜像、TrustCache、BuildManifest／选定 identity，以及 cryptex info/root hash。本方案建议先作为 App 资源打包；放入 App 私有容器也可，接口接受字节，并不要求素材必须位于 bundle。证据：`M:578–582`、`A:64–76`。

本机素材逻辑大小如下，均直接读取文件大小：

| 文件，均相对 `D/Restore/` | 字节 |
|---|---:|
| `022-21793-047.dmg`：personalized image | 11,452,141 |
| `Firmware/022-21793-047.dmg.trustcache` | 1,455 |
| `022-21771-047.dmg`：generic cryptex image | 11,570,297 |
| `Firmware/022-21771-047.dmg.trustcache` | 1,455 |
| `Firmware/022-21771-047.dmg.cryptex_info` | 428 |
| `Firmware/022-21771-047.dmg.root_hash` | 229 |
| `BuildManifest.plist` | 804,143 |

mounter 素材约 **11.69 MiB**；cryptex 素材约 **11.80 MiB**；两套共用 Manifest 约 **22.73 MiB**。不含 App 本体、ticket、安装存储和内存复制开销；ticket 实际体积未找到。

本机 Manifest 包含 `iPhone15,2` identity，BoardID `0x0C`、ChipID `0x8120`；这证明素材有对应条目，不能证明适配 iOS 27。证据：`D/Restore/BuildManifest.plist`。

**2．TSS、设备标识与缓存**

- mounter 先查设备签名缓存；查不到则重新连接 mounter，查询 identifiers/nonce，再由调用进程请求 Apple TSS。设备返回的 personalization manifest 是签名数据，**不是预置 BuildManifest**。证据：`M:637–655,682–768`。
- cryptex 的 `install_ddi` **每次请求 TSS**，当前没有 ticket 缓存分支。证据：`C:394–417`。
- 请求由运行库的进程直接联网发送；若库在 App 内运行，就是手机 App 发出，不需 Mac 中转。实现使用系统 TCP，而非 RSD provider。**手机网络＋LocalDevVPN 同开时实际可达性未核实**。证据：`T:77–83`、`I/src/http.rs:232–246`。
- `@HostPlatformInfo="mac"` 是请求常量，不能据此断言必须在 Mac 上执行。证据：`T:33–38`。

mounter 请求包含：BoardID、ChipID、ECID、实时 `ApNonce`、`Ap,*` identifiers、生产／安全模式与 Manifest 的 Trusted 组件及规则；代码还设置 `@BBTicket=true`，`SepNonce` 用 20 个零。**这不表示需要读取真实 SEP nonce 或 baseband nonce。**证据：`M:689–755`、`T:258–277,303–307,502–556`。

cryptex 请求包含：identity 的 ChipID、ProductClass、Type/SubType、UseProductClass、NonceDomain、Version、PreauthorizationVersion；实时 nonce、设备 `img4_chip_cpro`、空 UniqueTagList、需要 personalization 的组件 Digest。证据：`T:174–245`。

其中 `Cryptex1,UDID` 是 **ChipID 与 ECID 各 8 字节大端拼接**，不是通常的 USB UDID 字符串；BoardID 虽可能在响应中出现，当前 ticket 生成并未使用它。证据：`T:215,716–736`。

**发现实际隐私风险：现有 TSS 默认明文 HTTP，并在 debug 日志输出完整请求和响应。**此外 HTTP 客户端允许跟随跨主机重定向；307/308 会保留请求正文。因此直接复用现有 `send()` 无法保证私密标识仅发往 Apple。证据：`T:17,71–86`、`I/src/http.rs:79–97,213–215`。本次没有触发该请求；后续联网实验应在修复前停止。

方案隐私边界：ECID、派生 UDID、nonce、配对凭据与 ticket 仅存设备本地；必要标识仅通过验证证书的 HTTPS 发往核实过的 Apple TSS 主机，禁止第三方代理、跨域重定向、原文日志及同步备份。HTTPS endpoint 当前可用性：未核实；库已有 TLS 支持，见 `I/src/http.rs:410–429`。

缓存可行性：

- **推测可预取并缓存**：Mac 在场时，用当前设备、当前 nonce、指定素材 identity 获取 ticket，再安全导入手机私有容器；也可由手机先预取。
- 应以入口、设备、nonce/domain、identity、组件摘要、DDI 版本为缓存键；每次挂载前重新查询，任一不符即失效。这是方案要求，现有自动入口未实现。
- cryptex 源码明确说明 roll nonce 会使旧 personalization 失效。证据：`C:216–218`。
- **重启后 nonce 是否变化、设备签名缓存是否保留、ticket 是否存在其他有效期限制：未找到真机证据。**DDI 掉挂不等于 ticket 必然失效。
- 因此一次预取仅是优化；实现必须能在重启后重新向 Apple 请求，不能承诺长期离线自挂。

**3．沙箱、entitlement 与服务开放性**

- mounter 的 RSD shim 查询已由主会话实测成功，证据为研究文档 `docs/research/2026-10-self-runner.md:1110–1112`；对应服务名和 check-in 见 `M:772–780`。这仅证明查询可达，未证明 mount 权限。
- cryptex 服务为 `com.apple.security.cryptexd.remote`，源码定义 `ReadIdentifiers` / `CryptexInstall` capability。真机是否发布、允许读或 install：未核实。证据：`C:19–21,121–136`。
- RSD handshake 保存服务 `Entitlement`、Features、端口。**不能直接把远程服务 Entitlement 字段解释为 App 必须签入的本地 entitlement**；具体授权判定未找到。证据：`I/src/services/rsd.rs:59–114`。
- 项目配置未声明专用挂载 entitlement，见 `spikes/self-runner-app/project.yml`；设备 daemon 的 mount/install 授权实现未找到。因此不能断言普通开发签名一定允许或一定禁止。
- 沙箱内读取预置资源、发起网络连接在代码层有实现路径；潜在失败包括配对授权、Developer Mode、服务未发布、daemon 拒绝、素材不兼容、存储不足和 App 被终止。具体 iOS 27 限制未核实。
- mounter 文件顶部注释要求建立后查询 lockdown，否则设备可能不响应；现有 RSD 查询成功，但该注释是否同样约束 RSD mount：未核实。证据：`M:24–26`。

**4．最小真机实验：先只读，最后 mount**

以下是后续实验设计，本次未执行。

| 步骤 | 操作与预期输出 | 失败信号／判定 |
|---|---|---|
| ① 素材离线检查 | 记录实际 DDI build、identity、路径、大小、摘要；两入口都能选中 identity | Variant 不匹配、缺文件、摘要不符；禁止继续 |
| ② App 内 RSD 探测 | 复用 RP/VPN 链，记录两服务存在性、Entitlement、Features、版本 | 连接失败；服务缺失须再排除 parser 丢弃条目 |
| ③ mounter 只读 | CopyDevices、Developer Mode、identifiers、`DeveloperDiskImage` nonce；记录类型和长度 | 缺字段、拒绝、超时；ECID 未取得则不进入 TSS |
| ④ cryptex 只读 | 每次新连接，read identifiers、copy-installed、按 Manifest handle get nonce | 缺 ECID/cpro、MalformedNonce、routine error；handle 不可用常量代替 |
| ⑤ 签名缓存只读 | 对 personalized image SHA-384 查询设备 manifest | `NotFound` 是缓存未命中；查询失败后关闭并重建连接 |
| ⑥ TSS 独立验证 | 修复隐私问题后，由手机请求；输出成功状态、ticket 键和长度 | DNS/TLS/拒签、缺 ticket；此时仍不 upload/install |
| ⑦ 一次冷启动 | 所有准备完成后，Mac 关闭、USB 拔掉，重启一次；解锁、开 VPN、点开 App，重跑②–⑤ | 若 mounter/cryptex 必需服务仅在 DDI 已挂时存在，则对应路线冷启动阻塞 |
| ⑧ 真正挂载 | 优先经证据支持的路线；mounter 预期上传 ACK、上传 Complete、mount Complete；cryptex 预期 install 无错误且 installed_ddi 返回 DDI | 协议错误、cferr/errno、超时、挂载后仍无 DDI |
| ⑨ 功能验收 | 刷新 RSD，再建立 shared session、E1c、读屏、截图、自身 counter+1；杀 App 再打开复验 | 仅 ready／返回成功不能算通过；节点、截图或计数失败即未验收 |

依据：① `A:35–59,T:660–763`；② RSD parser `:59–114`；③ `M:60–72,335–405`；④ `C:139–213,A:80–89`；⑤ `M:292–325,637–650`；⑥ `T:87–109,C:412–417`；⑧ `M:164–207,233–255,C:304–305,431–450`。

这里需保留原始 RSD 结构的**脱敏字段摘要**：parser 会忽略无 Entitlement 或合法字符串端口的条目，解析表中缺失不能立即认定服务未发布。证据：`I/src/services/rsd.rs:59–75`。cryptex `copy_installed` 对缺数组返回空列表，也须区分“确实为空”与响应形状变化，见 `C:169–195`。

读探测、TSS、素材和代码准备都不需重启。**一次重启足以验证首次冷启动自挂，并同时比较重启前后 nonce/cache**。若第一路线明确失败且无部分安装，可在同一冷启动尝试另一条；若已成功，另一条独立验收需另一个干净窗口，不能覆盖现有挂载。

验证“成功自挂后再次重启仍能恢复”需第二次重启；不作为首次可行性判别的前置条件。实验不调用 roll nonce、unmount 或 uninstall。

**5．风险边界**

- **变砖风险推测较低，但不能保证为零**：两入口上传／安装 DDI，没有调用整机 restore 或固件刷写。错误签名预期被 daemon 拒绝；iOS 27 的事务性、回滚及异常行为未找到。证据：`M:659–665`、`C:259–306`。
- 更现实的风险是服务挂起、部分安装、空间占用、App 内存压力和原 XCTest 会话失效。cryptex 自动入口会克隆素材，峰值内存高于包大小，见 `C:421–427`；应前台单次执行，失败后重新查询状态，不盲目重试。
- Apple 账户登录/token 在这两条 TSS 请求中未找到；开发签名与 Developer Mode 是另一个边界。ECID／派生 UDID／ticket 仍属私密设备数据，按上面的本地＋Apple 边界处理。证据：`T:33–38,174–245,258–277`。
- 对 Xcode 的干扰主要是共享系统 DDI 状态及 nonce：不要与 Mac 挂载并行，不卸载现有 DDI、不 roll nonce；已挂载时直接跳过写操作。USB devicectl 会启动 Mac DDI 服务，不能用于冷启动窗口验收。证据：`docs/research/2026-10-self-runner.md:950`。
- 两入口 persistence 的协议语义未找到；不能凭常量名承诺跨重启保留。当前 `C:274` 还把 persistence 写成 nonce_persistence，必须先核实并纠正字段映射。

**6．推荐路线与分阶段计划**

推荐 **mounter 优先、cryptex 保留为独立后备**：前者已有 App 内查询证据、素材更少、支持设备签名缓存；后者能读取 ECID，但当前 loader 和 persistence 映射都有问题。最终选择由冷启动服务能力决定，不能按“iOS 27”名称硬编码。

| 阶段 | 后续代码范围 | 真机验收标准 |
|---|---|---|
| P0 只读探测 | `crates/self-boot/src/lib.rs`、`src/ffi.rs`、`include/self_boot.h`；spike 增独立 probe 动作；cryptex feature 会连带启用 tss，但禁止调用 send/install | 当前挂载状态下，两入口分别报告服务能力、字段形状、nonce/cache；无写操作 |
| P1 素材和隐私 | spike 资源配置；隔离依赖补丁处理 Variant、TSS HTTPS／主机限制／日志脱敏；核实 persistence 后正确映射 | 本机素材全部校验；手机能安全取得 ticket；日志、共享 Documents、备份无原始标识或 ticket |
| P2 手动单次挂载 | self-boot 增分步签名、上传、挂载接口与私有缓存；Swift 在 `mounted=false` 时执行显式实验动作 | 一次重启、Mac off、无 USB；从未挂载变为实际可用，shared session＋E1c＋读屏／截图／counter+1 |
| P3 自动恢复 | Swift `connect` 在现有 DDI gate 前加入 ensure-DDI 状态机；单任务、阶段 deadline、失败有限重试、刷新 RSD；保留 generation 校验 | 第二次重启仍恢复；杀 App 重开跳过重复挂载；TSS 不可达时明确失败且不死循环 |

feature 依据：`crates/self-boot/Cargo.toml:12`、`I/Cargo.toml:144,256`；状态机插入点：`SelfRunnerSpike.swift:356–373`。敏感缓存不要放入当前开启 File Sharing 的 Documents，配置证据：`project.yml:39–40`。

验收仍需解锁后有人启动 App、LocalDevVPN 能连接；无人启动和签名过期不由 S1 解决。现有“会话建立后可关 VPN”仅适用于已测操作，不能推广到挂载／重建链。证据：研究文档 `:1079–1081,1116–1118`。