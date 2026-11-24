# Fcitx5TvFix（小米澎湃OS TV / HyperOS 小企鹅输入法焦点修复模块）

一个仅作用于 `org.fcitx.fcitx5.android`（**不 Hook `system_server`**）的极简 LSPosed / Vector 模块（基于 Modern Xposed API），用于修复 **Fcitx5 for Android（小企鹅输入法）** 在小米电视 / 红米 G Pro 27U（HyperOS TV）上的以下问题：

1. **非输入框强制弹出 / 抢焦点**：当当前界面没有聚焦文本输入框（`InputType.TYPE_NULL`）时，`onEvaluateInputViewShown()` 返回 `false`、`onStartInput()` 主动隐藏软键盘、`onComputeInsets()` 清空触摸区域。
2. **拦截遥控器方向键与系统按键**：在非输入框场景下放行方向键 / 确认 / 返回键，并对 `HOME / MENU / POWER / VOLUME / VOICE_ASSIST` 等电视系统键始终放行。
3. **Android 14 输入法切换空转**：在非输入框场景下屏蔽冗余的 `switchInputMethod` 调用。

## 安装与启用
1. 在电视上安装 [Fcitx5 for Android](https://github.com/fcitx5-android/fcitx5-android/releases) 与 `Fcitx5TvFix` APK（可从本仓库 Releases 下载，或自行编译签名）。
2. 在 **Vector / LSPosed 管理器** 中启用 **Fcitx5TvFix** 模块。
3. 作用域仅勾选 **`小企鹅输入法 (org.fcitx.fcitx5.android)`**，无需勾选系统框架。
4. 强行停止一次小企鹅输入法（或重启电视）即可生效。

> 不想折腾 Root / Xposed 的用户，推荐直接使用免 Root 开箱可用的 **同文输入法（Trime / Rime）**，详见仓库根目录 [README.md](../../README.md)。
