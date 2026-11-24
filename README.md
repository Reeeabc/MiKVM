# MiTV-Switch — Redmi G Pro 27U Windows 信号源切换与键鼠接管工具

一款专为 **Redmi 显示器 G Pro 27U（MiTV-MFFU1 / beekeeper）** 打造的极轻量 Windows 11 托盘控制工具：无需遥控器、无需额外 USB 切换器，在电脑右下角即可一键切换 **电视系统 / 电脑 DP / HDMI 1 / HDMI 2**、调节**真实硬件背光与音量**，并在切入电视系统时**自动接管电脑的任意鼠标和键盘**。

## 核心特性

- **仿 Windows 11 原生圆角浮窗**：
  - 左键点击托盘图标弹出极简圆角控制面板，点外部自动收起；右键托盘提供快捷菜单。
  - 打开面板自动静默刷新，未连接（红点）时直接点击顶部状态行即可一键重连。
  - 实时高亮当前信号源（`电视系统` / `电脑 DP` / `HDMI 1` / `HDMI 2`）。
- **毫秒级硬件背光 & 音量直调（按需常驻桥接，待机零额外内存）**：
  - 内置轻量 `MiTVDaemon.jar`（3 KB，源码见 `assets/MiTVDaemon.java`）：打开面板或进入电视接管时按需预热，拖动滑块单次写入延迟仅 **~6–10 ms**，松手后自动同步系统 PQ/设置记账；收起面板空闲 25 秒自动退出，平时待机零额外进程与内存占用。
  - 兼容免 Root 音量回退（`cmd media_session`）。
- **零硬件共享电脑键鼠（支持任意有线 / 2.4G / 蓝牙键鼠）**：
  - 基于 `scrcpy` 无画面模式（`--no-video --no-audio --keyboard=sdk --raw-key-events --mouse=uhid`），几乎零带宽与 CPU 占用。
  - 捕获窗口完全隐形且不进任务栏，切回电脑 DP 时自动释放键鼠。
  - 支持在「ADB 设置...」中自由开关「切到电视时自动接管键鼠」。
- **可自定义键盘遥控器映射（仅在接管电视期间生效，回电脑自动注销零误触）**：
  - 在「ADB 设置... → 自定义快捷键...」中支持**点击录制按键**、冲突检测、防破坏打字保护及一键切换 **预设 1 / 预设 2**。
  - 默认键位（均支持自由修改或清空）：
    - `↑ / ↓ / ← / →` / `Enter`：方向键与确认
    - `Esc` / 鼠标后退侧键：返回（`KEYCODE_BACK`）
    - `Home` / 鼠标中键或前进侧键：主页（`KEYCODE_HOME`）
    - `F12`：小米遥控器菜单键 `≡`（`KEYCODE_MENU`）
    - `F11`：**小爱语音助手**（调用原厂 `ONBOARD_MIC_START` 直接启用机身远场麦克风收音，无需 Root）
    - `PgUp` / `PgDn`：电视音量 `+` / `-`（支持长按连发）
    - `Ctrl + Alt + ↑ / ↓`：显示器硬件亮度 `+` / `-`（步进 5%，支持长按连发）
    - `Ctrl + Alt + M`：静音；`Ctrl + Alt + P`：电源息屏待机
    - `Ctrl + Alt + D`：电视任意界面一键释放键鼠并切回电脑 DP
    - 鼠标滚轮上 / 下：可选映射为电视上下方向键（默认开启，可在设置中开关）
- **Windows 11 TSF 输入法自动保存与恢复**：
  - 接管电视期间自动将 Windows 切换为 `ENG`，避免与电视端输入法（如 Fcitx5 / Trime）抢占 `Shift` 与字母键；
  - 切回电脑 DP 时通过 Windows TSF COM 接口（`ITfInputProcessorProfileMgr`）自动还原回切换前的中文输入法（微软拼音 / 小狼毫等）。
- **极致轻量（Rust 原生重写版 v2.0）**：
  - 纯 Rust + Win32 / GDI+（仅单一依赖 `windows-sys`），零运行库依赖（不依赖 .NET / VC++ / WebView2），独立单文件仅 ~430 KB，任务管理器内存仅 **~2.6–5.4 MB**（较 C# 版 ~27 MB 降低 80%+），待机 CPU 0%。

## 环境要求

1. **电视端**：
   - Redmi G Pro 27U（已开启无线 ADB 5555 端口；若已 Root，推荐配合 `MiTV ADB Keeper` 模块保持开机常开 ADB）。
   - 电脑与电视处于同一局域网（建议在路由器后台为电视绑定静态 IP）。
   - **免 Root 可用**：信号源切换、无线键鼠接管、自定义快捷键、小爱机身远场语音（`F11`）、音量调节（`PgUp/PgDn` 与滑块回退）、Win11 TSF 输入法自动恢复。
   - **需 Root 进阶**：MTK 硬件背光寄存器直写（背光滑块 & `Ctrl+Alt+↑/↓` 亮度快捷键）与 `MiTVDaemon` 6–10 ms 毫秒级桥接。
2. **电脑端**：
   - Windows 10 / 11（纯原生 Win32 可执行文件，无需任何额外运行库；对电脑键鼠类型/连接方式/配列零要求）。
   - 下载 [scrcpy Windows 64-bit](https://github.com/Genymobile/scrcpy/releases/latest) 解压到本目录下的 `scrcpy/` 文件夹（确保存在 `scrcpy\scrcpy.exe` 和 `scrcpy\adb.exe`）。

## 电视端中文输入法推荐（配合电脑实体键盘打字）

电视出厂自带的搜狗 TV 输入法主要面向遥控器方向键，对电脑实体键盘中文输入支持较差。配合 `MiTV-Switch` 使用时，推荐按是否 Root 选择以下方案：

### 1. 免 Root 首选：同文输入法（[Trime](https://github.com/osfans/trime/releases) / Rime 引擎）
- **特点**：**完全不需要 Root，不需要任何 Xposed 模块**，开箱即支持电脑实体键盘中文输入。
- **配置步骤**：
  1. 在电视上安装 `com.osfans.trime-*-arm64-v8a-release.apk`；
  2. 打开一次「同文输入法」App 点击 **「部署」**，等待词库编译完成（约 10–20 秒）；
  3. 在电视「系统设置 → 通用设置 → 输入法」切换为 **同文输入法**，或免 Root 执行：
     ```powershell
     adb shell ime enable com.osfans.trime/.ime.core.TrimeInputMethodService
     adb shell ime set com.osfans.trime/.ime.core.TrimeInputMethodService
     ```
- **进阶词库**：可导入 [雾凇拼音（rime-ice）](https://github.com/iDvel/rime-ice)，与电脑端小狼毫（Weasel）共用同一套配置与词库。若需访问外部 `/sdcard/rime` 目录，免 Root 执行一次授权即可：
  ```powershell
  adb shell appops set com.osfans.trime MANAGE_EXTERNAL_STORAGE allow
  ```

### 2. 已 Root 进阶方案：小企鹅输入法（[Fcitx5 for Android](https://github.com/fcitx5-android/fcitx5-android/releases)）
- **特点**：启动快、自带现代简体拼音词库、实体键盘悬浮候选条精致小巧。
- **注意点**：在小米澎湃OS TV（HyperOS）上直接安装时，非输入框界面容易被系统触发抢焦点/拦截方向键，需在 Root + Zygisk（LSPosed / Vector）环境下搭配针对 `org.fcitx.fcitx5.android` 的焦点与非输入框避让 Hook 模块使用。

## 从源码构建

需要 Rust 工具链（MSVC 或 MinGW/GNU 均可）：

```powershell
cargo build --release
copy target\release\MiTV-Switch.exe .\MiTV-Switch.exe
```

编译完成后直接运行根目录的 `MiTV-Switch.exe`，在右下角托盘弹出面板中点击 **「ADB 设置...」** 填入电视 IP（如 `192.168.1.200:5555`）即可使用。

> 如需旧版无外部依赖的 C# (.NET Framework 4) 源码，请切换至 `v1.0-csharp` 标签或 `main` 分支。

## 致谢与许可

- 本项目采用 [MIT License](LICENSE) 开源。
- 信号源指令、面板交互灵感及 `VolDirectTool.jar` 来自 [HanHan666666/redmi-g-pro-27u-monitorswitch](https://github.com/HanHan666666/redmi-g-pro-27u-monitorswitch)（MIT）。
- `MtkDirectTool.jar` 及寄存器读写方案来自 [YiHoooong/Mimonitor_Toolbox](https://github.com/YiHoooong/Mimonitor_Toolbox)（MIT）。
- 无画面键鼠转发引擎基于 [Genymobile/scrcpy](https://github.com/Genymobile/scrcpy)（Apache-2.0）。
- 详细第三方声明见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
