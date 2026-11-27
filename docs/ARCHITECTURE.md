# MiKVM 架构与维护文档

本文档面向 **MiKVM** 的维护者与开源贡献者，介绍整体模块设计、直连 `scrcpy-server` 跨系统多屏 KVM 状态机、`MiTVDaemon` / `MiTVEdge` 通信协议，以及未来 macOS 移植的接入点。

---

## 1. 模块总览

MiKVM 刻意保持**零重型框架**架构（无 Electron / Tauri / WebView2 / `.NET`，也无需启动 `scrcpy.exe` / SDL3 / FFmpeg），仅依赖 `windows-sys` 一个 Rust crate 直接调用系统原生 API：

| 模块 | 职责 | 平台相关性 |
| :--- | :--- | :---: |
| `src/config.rs` | `config.ini` 读写（兼容 UTF-8 BOM）、`assets/` 与 `scrcpy/`（含系统 `PATH`）路径解析、系统浅色/深色主题查询、开机自启注册表 | 主体跨平台（仅自启/主题为 Win32） |
| `src/diag.rs` | 默认关闭的零开销按需诊断日志（`DebugLog=True` 或 `--debug-log`，写同目录 `mikvm.log`，512 KB 自动轮转） | 平台层（Windows 时间戳） |
| `src/ddc.rs` | 动态加载内置 `dxva2.dll`（VCP `0x10`），免 Root 直调 DP/HDMI 显示器物理背光 | 平台层（Windows） |
| `src/lan_scan.rs` | 动态调用 `iphlpapi.dll` 枚举私网 `/24` 网段、32 线程并发扫描 5555 端口并通过 `getprop` 优先识别小米/红米机型 | 平台层（Windows） |
| `src/adb.rs` | ADB 命令执行、设备状态合并查询、自动校验并推送 `assets/*.jar`、常驻按键 shell 管道、`MiTVDaemon` 单队列合并与按需保活工作线程（未 Root 时背光自动走 `ddc.rs`、音量走 `cmd media_session`） | **跨平台**（仅隐藏窗口 flag 为 Win32） |
| `src/scrcpy_proto.rs` | 纯 Rust 直连 `scrcpy-server` 控制通道（`adb forward` + TCP）、UHID 67 字节虚拟相对鼠标、VelocityControl 预激发单帧突发移动、Android 按键/组合键注入与双向剪贴板报文 | **100% 跨平台** |
| `src/display.rs` | `QueryDisplayConfig` 多屏拓扑检测、小米/红米 EDID（`0xA961`）识别、合盖与系统休眠/唤醒事件（`WM_POWERBROADCAST`）、多屏防幽灵弹窗定位、可选临时切「仅电脑屏幕」与还原 | 平台层（Windows） |
| `src/clipboard.rs` | Win32 `CF_UNICODETEXT` 读写与密码管理器隐私剪贴板格式过滤（`ExcludeClipboardContentFromMonitorProcessing` / `CanIncludeInClipboardHistory`） | 平台层（Windows） |
| `src/keymap.rs` | 12 个可映射动作模型（含 `ToggleFocus` 焦点切换）、`KeyChord` 解析与格式化、预设 1/2、冲突与防误录校验 | **跨平台**（键码表抽象即可移植） |
| `src/takeover/` | 模块化 4 态多屏 KVM 子系统：`mod.rs`（状态机总控与休眠/断网自愈）、`state.rs`（共享原子态与修饰键/吞键位图）、`hooks.rs`（`WH_KEYBOARD_LL`/`WH_MOUSE_LL`/`WM_INPUT`/粘滞键抑制）、`edge.rs`（电脑阻力墙 + `MiTVEdge` + `3.0x` 预激发单帧对齐）、`dp_watcher.rs`（常驻 shell DP 监听） | 平台层（Windows） |
| `src/gdi.rs` / `src/flyout.rs` | 双缓冲 GDI+ Win11 圆角浮窗（自动跟随系统浅色/深色主题与任务栏配色）、矢量图标绘制、35ms 实时节流滑块、红点点击重连 | 平台层（Windows） |
| `src/settings_ui.rs` / `src/keymap_ui.rs` | 原生 Win32 ComCtl32 v6 多屏设置窗口（含深色标题栏与控件主题）与临时低级钩子按键录制窗口 | 平台层（Windows） |
| `src/main.rs` | 单实例互斥锁、系统托盘消息循环（`WM_INPUT` / `WM_DISPLAYCHANGE` / `WM_POWERBROADCAST` / `WM_SETTINGCHANGE`）与各子系统调度中枢 | 平台层（Windows） |
| `assets/MiTVDaemon.java` | 电视端 3 KB 极轻常驻桥接进程，消除 `app_process` ART 虚拟机反复冷启动开销 | Android / MT9655 电视端 |
| `assets/MiTVEdge.java` | 电视端 8.5 KB 完全自包含免 Root 3px 透明边沿悬浮传感器（内置 `SafeContentResolver` / `ShellContext`，零 `scrcpy-server` 类依赖），实现电视 → 电脑双向滑边与精确 `y` 坐标回传 | Android TV（免 Root） |
| `extras/Fcitx5TvFix/` | 可选 Xposed / Vector 模块，修复 Fcitx5 在 HyperOS TV 非输入框界面的焦点与方向键拦截 | Android TV（仅 Root + Fcitx5） |

---

## 2. 核心工作流与时序

### 2.1 毫秒级背光与音量直调（按需常驻 + 自动回收）

```text
用户左键点开托盘浮窗（或切入电视控制）
   │
   ├─► DeviceController::set_keep_warm(true)
   │      └─ 后台工作线程拉起 1 条 adb shell -> su 1000 -> MiTVDaemon.jar
   │         （一次性加载 TVNative + AudioManager，输出 "READY" 后阻塞于 stdin，CPU 0%）
   │
   ├─► 拖动背光/音量滑块（或长按亮度 +/- 快捷键）
   │      ├─ flyout.rs 按 35ms 节流发最新值（按下立即发第一帧）
   │      ├─ adb.rs 单工作队列 coalescing（只保留最新目标值，杜绝并发堆积）
   │      └─ 经 stdin 写入 "B <v>\n" 或 "V <v>\n" -> 电视端内存反射直写（实测 6–9 ms）
   │
   ├─► 松开滑块（或亮度快捷键停止 450ms 后）
   │      └─ 补发一次 PIC_MODE_CHANGED 广播 + settings put global 同步系统记账
   │
   └─► 收起浮窗且未处于电视会话状态
          └─ DeviceController::set_keep_warm(false)
             └─ 空闲满 25 秒自动发送 "Q\n" -> MiTVDaemon 执行 System.exit(0) 退出（待机 0 额外内存）
```

> **免 Root 降级路径**：若电视未 Root 导致 `MiTVDaemon` 启动失败，电脑 DP/HDMI 模式下的背光自动通过原生 **DDC/CI（`dxva2.dll` VCP `0x10`）** 免 Root 直调，音量调节自动回退至免 Root 命令 `cmd media_session volume --stream 3 --set <v>`（实测 ~68 ms）。

### 2.2 `MiTVDaemon` 文本协议（stdin / stdout）

| 请求（stdin 一行） | 响应（stdout 一行） | 说明 |
| :--- | :--- | :--- |
| *(启动完成)* | `READY` | `TVNative` 与 `AudioManager` 反射初始化就绪 |
| `B <0..100>` | `OK B <v>` / `ERR ...` | 调用 `TVNative.setConfigValue_native(0, "g_disp__disp_back_light", v, 3)` |
| `V <0..100>` | `OK V <v>` / `ERR ...` | 调用 `AudioManager.setStreamVolume(3, v, 0)` |
| `G` | `G <bl> <vol>` | 一次读取当前硬件背光与媒体音量（失败项为 `-1`） |
| `Q` | *(进程立即退出)* | 调用 `System.exit(0)` 立即终止 Binder 线程与进程 |

### 2.3 直连 `scrcpy-server` 多屏软 KVM 与 4 态机

MiKVM 放弃了外挂 `scrcpy.exe` 隐形窗口方案，改由 Rust 原生客户端（`src/scrcpy_proto.rs`）通过 `adb forward` 直连电视端 `scrcpy-server` 控制通道（`video=false audio=false control=true`）：

- **零焦点/输入法闪烁**：不再创建任何抢焦点的 Windows 捕获窗，电脑端中文输入法（小狼毫 / 微软拼音）完全不受打扰，也省去 `scrcpy.exe` + SDL3 + FFmpeg 约 30 MB 内存开销。
- **4 态拓扑自适应**：
  - `Idle`：显示器在电脑 DP/HDMI，未建立电视控制通道，全部钩子与原始输入已卸载。
  - `TvLocked`：单屏模式（如笔记本合盖或「仅第二屏幕」），无可见电脑屏幕，键鼠严格锁定于电视端直至切回 DP 或开盖。
  - `TvFocused`：多屏模式且当前键鼠焦点在电视；安装 `WH_KEYBOARD_LL` + `WH_MOUSE_LL` + `WM_INPUT` 原始鼠标增量（通过 `HELD_MODS` 独立追踪修饰键并临时禁用 5 连 Shift 粘滞键弹窗），同时将 Windows 光标隐身停泊至不可见区。
  - `TvStandby`：多屏模式且焦点切回电脑（显示器画面仍保持在电视系统）；卸载鼠标钩子与原始输入，仅保留 `ReturnDp` 与 `ToggleFocus` 两个待命全局热键，并在电脑接缝边启用可调阻力的跨屏触发墙。
- **PC ↔ TV 双向无缝跨屏与单帧高度对齐**：
  - **PC → TV**：扩展屏下按两屏**实际重叠接缝区间（`seam_span`）**换算入口比例（支持双屏错位、上下叠屏及三屏组合拼接），覆盖幽灵电视屏区域实现带 `260ms` 停顿衰减的弹性推力墙（`EdgeResistancePc`, `0~300`）；扩展模式自动识别的 `TvEdgeSide` 在启动、排列变化及 `AutoInternalOnTv` 切单屏前实时落盘至 `config.ini`；仅电脑屏下采用 2px 边缘驻留触发。切入电视瞬间，利用实测的 Android 14 `VelocityControl`（`3.000x`）特性发送 **预激发单帧突发（Primed Burst）**——同一 TCP 写入内先发 1 包 `(pin_dx, 0)` 预热加速曲线，紧接 `(0, sy)` 纵向位移包，首帧即以 **`±1px` 精度**直达对齐高度（消除可见跳跃），并配 `160ms` 闭环校验与在线自适应校准。
  - **TV → PC**：免 Root 电视端 [MiTVEdge.java](file:///D:/develop/projects/mitv/MiKVM/assets/MiTVEdge.java) 监听 3px 透明边沿 `HOVER` 坐标，结合电脑端 `WM_INPUT` 外推位移累计超过 `EdgeResistanceTv`（`0~300`）即刻按实际重叠接缝区间精确映射滑回对应电脑屏。
- **对称位置记忆 & 按需剪贴板同步**：
  - 按 `ToggleFocus`（默认 `Ctrl+Alt+S`）在 PC 与电视间切换时，双方各自保留上次离开时的光标位置；进入电视前通过 `SendInput` 干净释放电脑端按住的组合修饰键，防止切屏卡键。
  - 每次切入电视按需将电脑新剪贴板推送至电视（跳过含密码管理器隐私标记的剪贴板），电视复制文本时通过 `DEV_TYPE_CLIPBOARD` 自动同步回电脑。
- **常驻 shell `DP Watcher`**：单条长连 `adb shell` 循环检测 `ExternalSourceActivity`，免除高频拉起短命 `adb.exe` 进程，在电视界面切回 DP 时毫秒级自动释控并还原临时显示配置。
- **休眠唤醒、Wi-Fi 瞬断与换 IP 自愈**：
  - `PBT_APMSUSPEND` 挂起前自动释放钩子与长连管道，防止唤醒卡死；`PBT_APMRESUMEAUTOMATIC` / `PBT_APMRESUMESUSPEND` 唤醒后（3 秒防抖）清理僵尸 ADB 管道，待网卡恢复后静默探测电视信号源，若仍在电视系统则自动恢复会话至安全的 `TvStandby` 待命态（不抢占电脑焦点）。
  - Wi-Fi 瞬断导致控制通道意外断开（`WM_CONTROL_EXITED`）时，立刻归还电脑键鼠控制权，后台按 `1s → 2s → 4s` 退避执行 `adb disconnect` + `connect` 强制重连；若原 IP 连续失败（如路由器重启 DHCP 重分 IP），自动按网段执行一次 `lan_scan::rediscover_xiaomi_tv`（仅采纳已验证的小米/红米机型），找回新 IP 后自动更新落盘 `config.ini`、托盘提示并静默重连（点击切源或手动重连红点遇到失联时同样自动找回）。

### 2.4 `MiTVEdge` 文本协议（stdin / stdout）

`MiTVEdge.jar` 完全自包含（仅需 `CLASSPATH=/data/local/tmp/MiTVEdge.jar`，不依赖 `scrcpy-server.jar` 内部类），在 `com.android.shell`（`uid 2000` + `appops set com.android.shell SYSTEM_ALERT_WINDOW allow`）下免 Root 运行，内置轻量 `SafeContentResolver`（动态 `IContentProvider` 代理）、`ShellContext` 与 `ActivityThread` 初始化逻辑，兼容 Android 10–15+ `app_process` 挂载 `TYPE_APPLICATION_OVERLAY`：

| 方向 | 报文 | 说明 |
| :--- | :--- | :--- |
| stdout | `READY <w> <h>` | 3px 透明悬浮边条已挂接，回报逻辑屏幕宽高（如 `1920 1080`） |
| stdout | `H <0..10000>` | 指针悬停在边条上，附归一化位置万分比；`HOVER_ENTER` 或变化 `≥ 0.6%` 立即上报不丢尾帧 |
| stdout | `L` | 指针离开边条 |
| stdin | `S LEFT\|RIGHT\|TOP\|BOTTOM\|HIDE` | 动态调整边条位置或隐藏（相同方向幂等跳过，避免冗余重排） |
| stdin | `Q` | 立即移除悬浮窗并 `System.exit(0)` 退出 |

---

## 3. 配置项参考（`config.ini`）

位于可执行文件同级目录（首次运行自动生成，Git 忽略以防泄露个人 IP；完整示例见 [config.example.ini](file:///D:/develop/projects/mitv/MiKVM/config.example.ini)）：

```ini
TvAddress=192.168.1.200:5555
AutoTakeoverOnTv=True
EdgeSwitch=True
EdgeResistancePc=120
EdgeResistanceTv=180
ClipboardSync=True
AutoInternalOnTv=False
DebugLog=False
TvMonitorHint=Mi Monitor
TvEdgeSide=Left
DpInput=29
Hdmi1Input=23
Hdmi2Input=24

Key.ReturnDp=Ctrl+Alt+D
Key.ToggleFocus=Ctrl+Alt+S
Key.Back=Esc
Key.Home=Home
Key.Menu=F12
Key.Voice=F11
Key.VolUp=PgUp
Key.VolDown=PgDn
Key.Mute=Ctrl+Alt+M
Key.BrightnessUp=Ctrl+Alt+Up
Key.BrightnessDown=Ctrl+Alt+Down
Key.Power=Ctrl+Alt+P
```

- `EdgeResistancePc` / `EdgeResistanceTv` 范围 `0~300`（`0` 表示一碰即过，越大越防误触，带 `260ms` 停顿清零）。
- 除 `Key.ReturnDp`（防止用户无快捷键退出控制）外，其余 `Key.*` 均可留空以禁用；鼠标滚轮默认永久映射为电视上下/左右方向键。
- 非 G Pro 27U 机型可通过 `adb shell settings get global mitv.tvplayer.hdmi.last.source` 查出对应端口编号并修改 `DpInput / Hdmi1Input / Hdmi2Input`。

---

## 4. macOS 移植接入指南（欢迎 PR）

`Cargo.toml` 已将 `windows-sys` 隔离在 `[target.'cfg(windows)'.dependencies]` 下。且直连控制协议层 `src/scrcpy_proto.rs` 与核心层 `adb.rs` / `config.rs` / `keymap.rs` 均不依赖重型桌面框架，移植 macOS 只需补齐 3 个原生适配模块：

1. **状态栏与浮窗**：使用 `NSStatusBar` + `NSPopover`（SwiftUI 或 `objc2-app-kit`）。
2. **多屏拓扑与键鼠捕获**：使用 `CGGetActiveDisplayList` + `CGEventTapCreate`（辅助功能授权）捕获相对鼠标增量与热键，直接喂给跨平台的 `ScrcpyControlClient`（无需再切换输入法或维护隐形 `scrcpy` 窗口）。
3. **剪贴板桥接**：使用 `NSPasteboard` 并过滤 `org.nspasteboard.ConcealedType` / `TransientType`。

---

## 5. 发布检查清单

1. 本地 `cargo test` 全部通过，`cargo build --release` 保证 **0 error、0 warning**，并通过 `objdump -p` 确认仅依赖 Windows 系统内置 DLL。
2. 确认没有把个人 `config.ini`、`scrcpy/`、`*.exe`、`*.jks` 提交进仓库（`git status` 干净）。
3. 更新版本号（`Cargo.toml`、`assets/app.manifest`），提交并推送目标分支。
4. 打附注标签推送：`git tag -a vX.Y.Z -m "..." && git push origin vX.Y.Z`，GitHub Actions 将自动编译并发布 `MiKVM-windows-x64-full.zip`、`MiKVM-windows-x64-lite.zip` 与 `SHA256SUMS.txt`。
