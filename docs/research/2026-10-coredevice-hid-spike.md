# 不经 XCTest runner 的 CoreDevice 截图 / HID 通道（已搁置）

> 更新：2026-10-23 · 状态：已搁置（2026-10-11）

**要点**
- 思路：不起 XCTest runner，直接用 CoreDevice 的 `displayservice`、`hid.universalhidservice`、`screencaptureservice` 截图和点按，协议实现用 MIT 许可的 `idevice` crate。
- 结论：截图要 Xcode 27 的 DDI；触控要 iOS 27 以上的手机。iOS 27.0.1 上点按、拖动、Home、键盘输入、粘贴都能用，USB 和 Wi-Fi 都测过。
- 搁置原因：替代不了 runner（拿不到 AX 树）；被 iOS 27 + Xcode 27 DDI 卡门槛；Wi-Fi 隧道脆弱；用的是私有服务；要先得到上游认可。
- 除非维护者明确要求，不要重启；要重启先在上游 leeguooooo/iphone-use 开 issue，并确认手头有 iOS 27 的手机。AX 树仍然离不开 runner。

## 背景

runner 的痛点在启动和重连：每次启动弹密码、签名、Xcode 版本漂移。这条通道有可能绕开这些。灵感来自 github.com/ja1chen/actell，它是 GPL，只看事实，不抄代码。2026-10-10 核对上游（a0862d6）没有这类实现，视频、截图、点按都走 runner。

探针代码（`spikes/coredevice-probe/` 和它的 FINDINGS.md，分支 `spike/coredevice-hid`）没有提交，只留在当时那台开发机的 worktree 里；本文是从当时的工作笔记整理的摘要。

## 实测结论（2026-10-10 ~ 10-11，iPhone 14 Pro）

- **DDI**：三个服务来自 Xcode 27 的 DDI（`dtuhidd`、`dtremotedisplayd`、`dtscreencaptured`），Xcode 26.x 的 DDI 里没有。Xcode 27 要 macOS ≥ 26.6；DDI 可以从 doronz88/DeveloperDiskImage 取 Cryptex 版本，先 `mounter unmount`，再用 idevice-tools 的 `cryptexd install-ddi` 装上。系统升级或重启后会重新挂回 Xcode 26.x 的 DDI，要重装 27 的。
- **iOS 26.6.1**：截图可用（全分辨率 PNG 约 360 ms），媒体流被拒，CoreDeviceError 9021「Remote control requires iOS 27.0 or later」，没有 HID。
- **iOS 27.0.1**：点按、拖动、Home 不用 runner、不弹提示就能用。挂载后的第一下点按会丢；刚 `media_open` 就发的点按也可能丢，等约 500 ms 或者点完核对再重试。
- **macOS 系统隧道（`--rsd`）**：一个媒体会话可以连续点很多下（每下约 52 ms）；HID 键盘输入会经过当前输入法（拼音 + 空格出中文），要输入准确的拉丁字符用粘贴板 + Cmd+V（不弹粘贴提示）。没有 devicectl 命令在跑时系统隧道会断；RSD 端口每次隧道都变，要靠端口扫描加握手找。
- **自建隧道**（usbmux → CoreDeviceProxy → 用户态 TCP，不经 devicectl）：一个媒体会话撑了 366 秒，7/7 点按落地，长连接截图约 296 ms。拔插重连未测。
- **只有 Wi-Fi**：usbmux 看不到设备，自建隧道用不了；经 localNetwork 的 devicectl 系统隧道可用，430 秒内 8/8 点按，截图约 510 ms。RSD 端口用并发扫描（约 8 秒），握手约 30% 失败要重试；隧道可能断开后换个地址回来，要重新发现。
- **会话规则**：每台设备只能有一个媒体会话，新开会话会静默顶掉旧的（每秒 0 包、不报错）。会话开着时灵动岛显示一个小胶囊，并且会阻止自动锁屏（用 2 分钟自动锁屏验证过）。锁屏时点按和 Home 能唤醒，但过不了密码。
- **错误码**：9022 表示 Siri / 听写在前台时被拒（一次误点开启了听写后出现）。

（2026-10-23，迁自本机 Claude 记忆 coredevice-hid-channel）
