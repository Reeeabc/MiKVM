# MiTV-Switch — Redmi G Pro 27U Windows 信号源切换与键鼠接管工具

一款专为 **Redmi 显示器 G Pro 27U（MiTV-MFFU1 / beekeeper）** 打造的极轻量 Windows 11 托盘控制工具：无需遥控器、无需额外 USB 切换器，在电脑右下角即可一键切换 **电视系统 / 电脑 DP / HDMI 1 / HDMI 2**、调节**真实硬件背光与音量**，并在切入电视系统时**自动接管电脑的任意鼠标和键盘**。

## 核心特性

- **仿 Windows 11 原生圆角浮窗**：
  - 左键点击托盘图标弹出圆角控制面板，点外部自动收起；右键托盘提供快捷菜单。
  - 实时高亮当前信号源（`电视系统` / `电脑 DP` / `HDMI 1` / `HDMI 2`）。
- **背光 & 音量直调**：
  - 背光采用 `MtkDirectTool.jar` 直写 MTK 寄存器 + PQ 广播刷新 + 系统记账。
  - 音量采用 `VolDirectTool.jar` 直调系统 `AudioManager` 瞬时生效。
- **零硬件共享电脑键鼠（支持任意有线 / 2.4G / 蓝牙键鼠）**：
  - 基于 `scrcpy` 无画面模式（`--no-video --no-audio --keyboard=sdk --raw-key-events --mouse=uhid`），几乎零带宽与 CPU 占用。
  - 捕获窗口完全隐形且不进任务栏，切回电脑 DP 时自动释放键鼠。
  - 支持在「ADB 设置...」中自由开关「切到电视时自动接管键鼠」。
- **完整键盘遥控器映射（仅在接管电视期间生效，回电脑自动注销零误触）**：
  - `↑ / ↓ / ← / →`：方向键
  - `Enter`：确认（OK）
  - `Esc` / 鼠标后退侧键：返回（`KEYCODE_BACK`）
  - `Home` / 鼠标中键或前进侧键：主页（`KEYCODE_HOME`）
  - `F12`：小米遥控器菜单键 `≡`（`KEYCODE_MENU`）
  - `PgUp` / `PgDn`：电视音量 `+` / `-`（支持长按连发）
  - `Ctrl + Alt + M`：静音
  - `Ctrl + Alt + P`：电源 / 息屏待机（组合键防误触）
  - `Ctrl + Alt + D`：电视任意界面一键释放键鼠并切回电脑 DP
  - 鼠标滚轮上 / 下：可选映射为电视上下方向键（默认开启，可在设置中开关）
- **Windows 11 TSF 输入法自动保存与恢复**：
  - 接管电视期间自动将 Windows 切换为 `ENG`，避免与电视端输入法（如 Fcitx5 / Trime）抢占 `Shift` 与字母键；
  - 切回电脑 DP 时通过 Windows TSF COM 接口（`ITfInputProcessorProfileMgr`）自动还原回切换前的中文输入法（微软拼音 / 小狼毫等）。
- **极致轻量**：
  - C# (.NET Framework 4) 单文件仅 ~73 KB，后台常驻内存约 27 MB，待机 CPU 0%。

## 环境要求

1. **电视端**：
   - Redmi G Pro 27U（已开启无线 ADB 5555 端口；推荐已 Root 并配合 `MiTV ADB Keeper` 模块保持开机常开 ADB）。
   - 电脑与电视处于同一局域网（建议在路由器后台为电视绑定静态 IP）。
2. **电脑端**：
   - Windows 10 / 11（自带 .NET Framework 4，无需额外运行库）。
   - 下载 [scrcpy Windows 64-bit](https://github.com/Genymobile/scrcpy/releases/latest) 解压到本目录下的 `scrcpy/` 文件夹（确保存在 `scrcpy\scrcpy.exe` 和 `scrcpy\adb.exe`）。

## 从源码构建

无需安装 Visual Studio 或任何第三方 SDK，直接运行：

```powershell
.\build.ps1
```

编译完成后直接运行根目录生成的 `MiTV-Switch.exe`，在右下角托盘弹出面板中点击 **「ADB 设置...」** 填入电视 IP（如 `192.168.1.200:5555`）即可使用。

## 致谢与许可

- 本项目采用 [MIT License](LICENSE) 开源。
- 信号源指令、面板交互灵感及 `VolDirectTool.jar` 来自 [HanHan666666/redmi-g-pro-27u-monitorswitch](https://github.com/HanHan666666/redmi-g-pro-27u-monitorswitch)（MIT）。
- `MtkDirectTool.jar` 及寄存器读写方案来自 [YiHoooong/Mimonitor_Toolbox](https://github.com/YiHoooong/Mimonitor_Toolbox)（MIT）。
- 无画面键鼠转发引擎基于 [Genymobile/scrcpy](https://github.com/Genymobile/scrcpy)（Apache-2.0）。
- 详细第三方声明见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
