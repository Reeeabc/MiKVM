# MiKVM 架构与维护文档

本文档面向 **MiKVM** 的维护者与开源贡献者，介绍整体模块设计、核心数据流、`MiTVDaemon` 通信协议以及未来 macOS 移植的接入点。

---

## 1. 模块总览

MiKVM 刻意保持**零重型框架**架构（无 Electron / Tauri / WebView2 / `.NET`），仅依赖 `windows-sys` 一个 Rust crate 直接调用系统原生 API：

| 模块 | 职责 | 平台相关性 |
| :--- | :--- | :---: |
| `src/config.rs` | `config.ini` 读写（兼容 UTF-8 BOM）、`assets/` 与 `scrcpy/`（含系统 `PATH`）路径解析、开机自启注册表 | 主体跨平台（仅自启为 Win32） |
| `src/adb.rs` | ADB 命令执行、设备状态合并查询、常驻按键 shell 管道、`MiTVDaemon` 单队列合并与按需保活工作线程 | **跨平台**（仅隐藏窗口 flag 为 Win32） |
| `src/keymap.rs` | 11 个可映射动作模型、`KeyChord` 解析与格式化、预设 1/2、冲突与防误录校验 | **跨平台**（键码表抽象即可移植） |
| `src/takeover.rs` | `scrcpy` 隐形捕获窗口生命周期、表驱动 `WH_KEYBOARD_LL`、滚轮 `WH_MOUSE_LL`、DP 快速回切监听 | 平台层（Windows） |
| `src/ime.rs` | 动态加载 `msctf.dll`，通过 Win11 TSF COM（`ITfInputProcessorProfileMgr`）保存并还原中文输入法 | 平台层（Windows） |
| `src/gdi.rs` / `src/flyout.rs` | 双缓冲 GDI+ Win11 圆角浮窗、矢量图标绘制、35ms 实时节流滑块、红点点击重连 | 平台层（Windows） |
| `src/settings_ui.rs` / `src/keymap_ui.rs` | 原生 Win32 ComCtl32 v6 设置窗口与临时低级钩子按键录制窗口 | 平台层（Windows） |
| `src/main.rs` | 单实例互斥锁、系统托盘消息循环与各子系统调度中枢 | 平台层（Windows） |
| `assets/MiTVDaemon.java` | 电视端 3 KB 极轻常驻桥接进程，消除 `app_process` ART 虚拟机反复冷启动开销 | Android / MT9655 电视端 |
| `extras/Fcitx5TvFix/` | 可选 Xposed / Vector 模块，修复 Fcitx5 在 HyperOS TV 非输入框界面的焦点与方向键拦截 | Android TV（仅 Root + Fcitx5） |

---

## 2. 核心工作流与时序

### 2.1 毫秒级背光与音量直调（按需常驻 + 自动回收）

```text
用户左键点开托盘浮窗（或切入电视接管）
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
   └─► 收起浮窗且未处于电视接管状态
          └─ DeviceController::set_keep_warm(false)
             └─ 空闲满 25 秒自动发送 "Q\n" -> MiTVDaemon 执行 System.exit(0) 退出（待机 0 额外内存）
```

> **免 Root 降级路径**：若电视未 Root 导致 `MiTVDaemon` 启动失败，音量调节自动回退至原生免 Root 命令 `cmd media_session volume --stream 3 --set <v>`（实测 ~68 ms）。

### 2.2 `MiTVDaemon` 文本协议（stdin / stdout）

| 请求（stdin 一行） | 响应（stdout 一行） | 说明 |
| :--- | :--- | :--- |
| *(启动完成)* | `READY` | `TVNative` 与 `AudioManager` 反射初始化就绪 |
| `B <0..100>` | `OK B <v>` / `ERR ...` | 调用 `TVNative.setConfigValue_native(0, "g_disp__disp_back_light", v, 3)` |
| `V <0..100>` | `OK V <v>` / `ERR ...` | 调用 `AudioManager.setStreamVolume(3, v, 0)` |
| `G` | `G <bl> <vol>` | 一次读取当前硬件背光与媒体音量（失败项为 `-1`） |
| `Q` | *(进程立即退出)* | 调用 `System.exit(0)` 立即终止 Binder 线程与进程 |

### 2.3 切入电视、键鼠接管与无感切回 DP

1. **保存输入法**：切电视前在主线程调用 `save_current_ime()` 通过 Win11 TSF `ITfInputProcessorProfileMgr` 记录当前激活的中文输入法 Profile（如小狼毫 / 微软拼音）。
2. **启动隐形 `scrcpy`**：以 `--no-video --no-audio --keyboard=sdk --raw-key-events --mouse=uhid --mouse-bind=+hbh:b+++ --window-borderless` 启动，捕获窗口设为 `WS_EX_LAYERED (alpha=1) | WS_EX_TOOLWINDOW` 使其完全隐形且不占任务栏，并将 Windows 切至 `ENG (0x0409)` 防止与电视端输入法抢 `Shift`。
3. **安装临时钩子**：仅在接管期间安装表驱动 `WH_KEYBOARD_LL`（配合 256-bit 无锁吞键位图）与 `WH_MOUSE_LL`（滚轮 → 上下方向键，50ms 节流）；所有遥控按键与小爱远场语音（`ONBOARD_MIC_START`）通过常驻 `adb shell` stdin 管道注入（~15 ms）。
4. **双路径自动释放**：
   - 路径 A：按下自定义的 `ReturnDp` 快捷键（默认 `Ctrl+Alt+D`）→ 立刻停止接管并发送 DP 切源指令。
   - 路径 B：在电视界面直接用鼠标/遥控器切回 DP → 后台 `DP Watcher`（500ms 稳定期 + 450/650ms 轮询）检测到 `ExternalSourceActivity` 后自动发 `WM_DP_RETURNED` 停止接管、乐观更新面板为 DP 并静默刷新。
5. **还原输入法**：退出接管时立即卸载全部钩子，关闭 `scrcpy`，并调用 `restore_saved_ime()`（立即 + 200ms 重试一次）恢复切电视前的中文输入法。

---

## 3. 配置项参考（`config.ini`）

位于可执行文件同级目录（首次运行自动生成，Git 忽略以防泄露个人 IP）：

```ini
TvAddress=192.168.1.200:5555
AutoTakeoverOnTv=True
WheelAsDpad=True
DpInput=29
Hdmi1Input=23
Hdmi2Input=24

Key.ReturnDp=Ctrl+Alt+D
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

- 除 `Key.ReturnDp`（防止用户无快捷键退出接管）外，其余 `Key.*` 均可留空以禁用。
- 非 G Pro 27U 机型可通过 `adb shell settings get global mitv.tvplayer.hdmi.last.source` 查出对应端口编号并修改 `DpInput / Hdmi1Input / Hdmi2Input`。

---

## 4. macOS 移植接入指南（欢迎 PR）

`Cargo.toml` 已将 `windows-sys` 隔离在 `[target.'cfg(windows)'.dependencies]` 下。若希望贡献 macOS 版本，核心层（`adb.rs` / `config.rs` / `keymap.rs`）可直接复用，只需补齐 4 个 macOS 原生模块：

1. **状态栏与浮窗**：使用 `NSStatusBar` + `NSPopover`（SwiftUI 或 `objc2-app-kit`）。
2. **全局键鼠拦截**：使用 `CGEventTapCreate`（提示用户在「隐私与安全性 → 辅助功能」中授权），仅在接管电视期间启用。
3. **输入法保存与还原**：使用 Carbon `TISCopyCurrentKeyboardInputSource` / `TISSelectInputSource` 切换至 ABC 并在退出接管后还原。
4. **`scrcpy` 隐形聚焦捕获**：处理 macOS 下 `scrcpy --no-video --mouse=uhid --keyboard=sdk` 窗口置前与指针捕获逻辑。

---

## 5. 发布检查清单

1. 本地 `cargo build --release` 保证 **0 error、0 warning**。
2. 确认没有把个人 `config.ini`、`scrcpy/`、`*.exe`、`*.jks` 提交进仓库（`git status` 干净）。
3. 更新版本号（`Cargo.toml`、`assets/app.manifest`），提交并推送 `main`。
4. 打附注标签推送：`git tag -a vX.Y.Z -m "..." && git push origin vX.Y.Z`，GitHub Actions 将自动编译并发布 `MiKVM-windows-x64-full.zip`、`MiKVM-windows-x64-lite.zip` 与 `SHA256SUMS.txt`。
