# MiKVM

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform: Windows 10/11](https://img.shields.io/badge/Platform-Windows%2010%2F11-0078D6.svg)](#环境要求)
[![Rust](https://img.shields.io/badge/Built%20with-Rust%20%2B%20Win32-orange.svg)](#从源码构建)

一款专为 **Redmi 显示器 G Pro 27U（MiTV-MFFU1 / beekeeper）** 及小米澎湃OS智能显示器打造的极轻量 **Windows 11 托盘切换器与软件 KVM**。

无需遥控器、无需硬件 USB 切换器、不挑电脑键鼠——在电脑右下角即可一键切换 **电视系统 / 电脑 DP / HDMI 1 / HDMI 2**、**6–10 ms 毫秒级调节真实硬件背光与音量**，并在切入电视系统时**自动隐形接管电脑的任意鼠标和键盘**；切回电脑时瞬间释放并自动还原你的中文输入法。

---

## 核心特性

- **极简 Windows 11 原生圆角浮窗**
  - 纯 Rust + Win32 / GDI+ 打造（仅单一依赖 `windows-sys`），**零运行库依赖**（不依赖 `.NET` / VC++ / WebView2 / Electron）。
  - 单文件仅 **~430 KB**，常驻内存仅 **~2.6–5.4 MB**，待机 CPU **0%**。
  - 左键托盘图标弹出圆角控制面板，打开自动静默刷新，点外部自动收起；未连接（红点）时直接点击顶部状态行即可一键重连。
- **零硬件共享电脑键鼠（支持任意有线 / 2.4G / 蓝牙键鼠与任意配列）**
  - 基于 `scrcpy` 无画面模式（`--no-video --no-audio --keyboard=sdk --raw-key-events --mouse=uhid`），几乎零带宽与 CPU 占用。
  - 捕获窗口完全隐形且不进任务栏；在电视界面按快捷回切键或直接点击桌面 DP 图标切回电脑时，自动释放键鼠并更新面板状态。
  - 可在「ADB 设置...」中自由开关「切到电视时自动接管键鼠」。
- **毫秒级硬件背光 & 音量直调（按需常驻，待机 0 额外内存）**
  - 内置轻量 `MiTVDaemon.jar`（3 KB，开源源码见 [assets/MiTVDaemon.java](assets/MiTVDaemon.java)）：打开浮窗或进入电视接管时自动预热，拖动滑块单次写入延迟仅 **~6–10 ms**，松手后自动同步系统 PQ / 设置记账；收起面板空闲 25 秒自动退出，待机零残留。
  - 兼容免 Root 音量调节回退（`cmd media_session`）。
- **可视化自定义遥控快捷键 + 原生小爱远场语音**
  - 在「ADB 设置... → 自定义快捷键...」中支持 **点击录制按键**、冲突检测、防误录普通打字/方向键保护，以及一键切换 **预设 1 / 预设 2**。
  - 所有快捷键**仅在接管电视期间生效**，切回电脑 DP 自动卸载低级钩子，绝不影响游戏与日常办公。
  - 默认键位（均支持自由修改或清空）：
    - `↑ / ↓ / ← / →` / `Enter`：方向键与确认
    - `Esc` / 鼠标后退侧键：返回（`KEYCODE_BACK`）
    - `Home` / 鼠标中键或前进侧键：主页（`KEYCODE_HOME`）
    - `F12`：小米遥控器菜单键 `≡`（`KEYCODE_MENU`）
    - `F11`：**小爱语音助手**（调用原厂 `ONBOARD_MIC_START` 直接启用显示器自带远场麦克风收音，**免 Root**，单按即说、说完自动断句）
    - `PgUp` / `PgDn`：电视音量 `+` / `-`（支持长按连发）
    - `Ctrl + Alt + ↑ / ↓`：显示器硬件亮度 `+` / `-`（步进 5%，支持长按连发）
    - `Ctrl + Alt + M`：静音；`Ctrl + Alt + P`：电源息屏待机
    - `Ctrl + Alt + D`：电视任意界面一键释放键鼠并切回电脑 DP
    - 鼠标滚轮上 / 下：可选映射为电视上下方向键（默认开启，可在设置中开关）
- **Windows 11 TSF 输入法精确保存与还原**
  - 接管电视期间自动将 Windows 切换为 `ENG`，避免与电视端中文输入法抢占 `Shift` 与字母键；
  - 切回电脑 DP 时通过 Windows TSF COM 接口（`ITfInputProcessorProfileMgr`）精确还原切换前的中文输入法（微软拼音 / 小狼毫等）。

---

## 环境要求与兼容性

### 1. 电视 / 智能显示器端
- **Redmi G Pro 27U**（其他小米澎湃OS / MIUI TV 显示器或电视也可在 `config.ini` 中调整 `DpInput / Hdmi1Input / Hdmi2Input` 适配）。
- 已开启无线 ADB（5555 端口），与电脑处于同一局域网（建议在路由器为显示器绑定静态 IP；若已 Root，推荐配合 `MiTV ADB Keeper` 保持开机常开 ADB）。

| 功能 | 免 Root | 已 Root |
| :--- | :---: | :---: |
| 信号源切换（电视 / DP / HDMI 1 / HDMI 2） | ✅ | ✅ |
| 无线键鼠自动接管 + 滚轮方向键 + 自定义快捷键 | ✅ | ✅ |
| 小爱机身远场语音助手（`F11`） | ✅ | ✅ |
| Windows 11 TSF 输入法自动保存与恢复 | ✅ | ✅ |
| 电视音量调节（`PgUp/PgDn` 及面板音量滑块） | ✅ | ✅（6–10 ms 桥接） |
| 真实硬件背光直写（面板背光滑块 + `Ctrl+Alt+↑/↓`） | — | ✅（6–10 ms 桥接） |

### 2. 电脑端
- **Windows 10 / 11**（纯原生 Win32 可执行文件，零运行库；对电脑键鼠类型、连接方式与配列无任何限制）。
- Releases 提供 **完整开箱版（`full.zip`，已内置官方 `scrcpy` 与 `adb`）** 和 **精简版（`lite.zip`，适合已装过 `scrcpy` 的用户，支持自动识别同级 `scrcpy/` 目录或系统环境变量 `PATH`）**。

---

## 快速使用

1. **新手推荐（开箱即用）**：从 [Releases](https://github.com/Reeeabc/MiKVM/releases) 下载 **`MiKVM-windows-x64-full.zip`**，解压到任意文件夹。
   > 若下载精简版 `MiKVM-windows-x64-lite.zip` 或从源码编译，可将 [scrcpy Windows 64-bit](https://github.com/Genymobile/scrcpy/releases/latest) 解压到同级 `scrcpy/` 目录（或确保 `scrcpy.exe` / `adb.exe` 已在系统 `PATH` 中）。
2. 双击运行 **`MiKVM.exe`**，左键点击右下角托盘图标打开浮窗 → 点击 **「ADB 设置...」** → 填入显示器无线 ADB 地址（如 `192.168.1.200:5555`）并保存即可使用。

---

## 电视端中文输入法推荐（配合电脑实体键盘打字）

电视自带的搜狗 TV 输入法主要面向遥控器五向键，用电脑实体键盘输入中文体验较差。推荐按是否 Root 选择以下方案：

### 1. 免 Root 首选：同文输入法（[Trime](https://github.com/osfans/trime/releases) / Rime 引擎）
- **特点**：**完全不需要 Root，不需要任何 Xposed 模块**，实测开箱即可配合 `MiKVM` 正常使用电脑键盘打中文。
- **配置步骤**：
  1. 在电视上安装 `com.osfans.trime-*-arm64-v8a-release.apk`；
  2. 打开一次「同文输入法」App 点击 **「部署」**，等待词库编译完成（约 10–20 秒）；
  3. 在电视「系统设置 → 通用设置 → 输入法」切为 **同文输入法**，或免 Root 执行：
     ```powershell
     adb shell ime enable com.osfans.trime/.ime.core.TrimeInputMethodService
     adb shell ime set com.osfans.trime/.ime.core.TrimeInputMethodService
     ```
- **进阶词库**：可导入 [雾凇拼音（rime-ice）](https://github.com/iDvel/rime-ice)，与电脑端小狼毫（Weasel）共用同一套配置与词库。若需访问外部 `/sdcard/rime` 目录，免 Root 授权一次即可：
  ```powershell
  adb shell appops set com.osfans.trime MANAGE_EXTERNAL_STORAGE allow
  ```

### 2. 已 Root 进阶方案：小企鹅输入法（[Fcitx5 for Android](https://github.com/fcitx5-android/fcitx5-android/releases)）
- **特点**：启动快、自带现代简体拼音词库、实体键盘悬浮候选条精致小巧。
- **注意点**：在小米澎湃OS TV 上直接安装时，非输入框界面容易抢焦点/拦截方向键，需在 Root + Zygisk（LSPosed / Vector）环境下搭配本仓库附带的轻量修复模块 **[extras/Fcitx5TvFix](extras/Fcitx5TvFix/README.md)**（仅 Hook `org.fcitx.fcitx5.android`，不改系统框架）。

---

## 从源码构建

需要 Rust 工具链（Windows MSVC 或 MinGW/GNU 均可）：

```powershell
cargo build --release
copy target\release\MiKVM.exe .\MiKVM.exe
```

> 如需早期无外部依赖的 C# (.NET Framework 4) 原型版本源码，请切换至历史标签 `v1.0-csharp`。

---

## macOS 支持计划

目前正式发布版为经过真机充分验证的 **Windows 10 / 11 原生版**。项目已在 `Cargo.toml` 中隔离 Windows 依赖，欢迎拥有 macOS 与 Redmi G Pro 27U 的开发者通过 PR 贡献 macOS 适配层（`NSStatusBar`/`NSPopover`、`CGEventTap`、`TIS` 输入法切换与 `scrcpy` 聚焦）。急用的 macOS 用户现阶段也可参考同生态的 [redmi-g-pro-27u-monitorswitch](https://github.com/HanHan666666/redmi-g-pro-27u-monitorswitch)。

---

## 致谢与许可

- 本项目采用 [MIT License](LICENSE) 开源。
- 信号源指令、面板交互灵感及 `VolDirectTool.jar` 来自 [HanHan666666/redmi-g-pro-27u-monitorswitch](https://github.com/HanHan666666/redmi-g-pro-27u-monitorswitch)（MIT）。
- `MtkDirectTool.jar` 及 MTK 背光寄存器读写方案来自 [YiHoooong/Mimonitor_Toolbox](https://github.com/YiHoooong/Mimonitor_Toolbox)（MIT）。
- 无画面键鼠转发引擎基于 [Genymobile/scrcpy](https://github.com/Genymobile/scrcpy)（Apache-2.0）。
- 详细第三方声明见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
