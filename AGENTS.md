# AGENTS.md — MiKVM Maintenance & Engineering Guide

> This file is the canonical context handoff for AI coding agents and maintainers working on **MiKVM** across sessions. Read this before modifying Win32 hooks, GDI+ UI, ADB/daemon logic, or release workflows.

---

## 1. Project Identity & Design Invariants

- **Project**: `MiKVM` (`https://github.com/Reeeabc/MiKVM`, MIT License, owner `Reeeabc`)
- **Target Device**: Redmi Display G Pro 27U (`MiTV-MFFU1`, codename `beekeeper`, MT9655 SoC, HyperOS TV / Android 14). Also compatible with other Xiaomi/Redmi HyperOS / MIUI TV displays via configurable `DpInput / Hdmi1Input / Hdmi2Input`.
- **Tech Stack**: Pure **Rust** + native **Win32 / GDI+ / DWM / TSF COM** with a single crate dependency (`windows-sys = "0.59"`).
- **Non-Negotiable Quality Targets**:
  - **Zero external runtime dependencies** (only built-in Windows system DLLs — never link MinGW runtime DLLs, VC++ redist, `.NET`, or WebView2).
  - **Ultra-small binary**: ~430–460 KB stripped LTO release `.exe` (`opt-level = "z"`, `lto = true`, `codegen-units = 1`, `panic = "abort"`, `strip = true`).
  - **Ultra-low memory**: ~2.6 MB cold / ~5.4 MB steady-state Private Working Set, **0% idle CPU**, **0 extra idle child processes** on both PC and TV.
  - **Clean C: drive policy (local machine)**: Never install toolchains, SDKs, or build caches on `C:`.

---

## 2. Local Environment & Build Commands

### Local Toolchain Paths (Windows)
- Rust: `RUSTUP_HOME=D:\develop\rustup`, `CARGO_HOME=D:\develop\cargo` (`stable-x86_64-pc-windows-gnu`)
- MinGW GCC / `windres` / `objdump`: `D:\develop\mingw64\bin`
- JDK (`javac` / `java`): `D:\develop\Java\jdk\bin`
- Repository root: `D:\develop\projects\mitv\MiKVM` (remote `origin` uses SSH: `git@github.com:Reeeabc/MiKVM.git`)
- Legacy C# v1.0 reference is preserved permanently on git tag `v1.0-csharp`.

### Build `MiKVM.exe` Locally (PowerShell)
```powershell
$env:RUSTUP_HOME = "D:\develop\rustup"
$env:CARGO_HOME = "D:\develop\cargo"
$env:Path = "D:\develop\cargo\bin;D:\develop\mingw64\bin;$env:Path"
Set-Location "D:\develop\projects\mitv\MiKVM"
cargo build --release
Copy-Item "target\release\MiKVM.exe" ".\MiKVM.exe" -Force
# Verify zero non-system DLL dependencies:
objdump -p ".\MiKVM.exe" | Select-String "DLL Name"
```
Expected DLL list ONLY: `advapi32`, `api-ms-win-core-synch-l1-2-0`, `dwmapi`, `gdi32`, `gdiplus`, `imm32`, `KERNEL32`, `msvcrt`, `ntdll`, `ole32`, `shell32`, `user32`.

### Rebuild `assets/MiTVDaemon.jar` (only if modifying `assets/MiTVDaemon.java`)
Compile with Java 8 bytecode (`D:\develop\Java\jdk\bin\javac.exe --release 8`) and convert to DEX jar using `dalvik-dx` (download temporarily into `target/dex_tmp/` on `D:` and delete immediately afterward — never cache on `C:`).

### Release Process (GitHub Actions)
- Pushing any `v*` tag (e.g., `git tag -a v2.0.1 -m "..." && git push origin v2.0.1`) triggers `.github/workflows/release.yml` on `windows-latest` (MSVC).
- Automatically produces and attaches to GitHub Releases:
  - `MiKVM-windows-x64-full.zip` (bundles latest official `Genymobile/scrcpy` win64 under `scrcpy/`)
  - `MiKVM-windows-x64-lite.zip` (~130 KB binary + `assets/*.jar` + docs)
  - `SHA256SUMS.txt`
- `build.rs` embeds `assets/app.manifest` (ComCtl32 v6 + PerMonitorV2 DPI) via `windres` (GNU) or `rc` (MSVC).

---

## 3. Repository & Module Map

```text
MiKVM/
├── Cargo.toml                     # Package config; windows-sys scoped under [target.'cfg(windows)'.dependencies]
├── build.rs                       # Embeds assets/app.manifest (ComCtl32 v6 + PerMonitorV2)
├── assets/
│   ├── app.manifest               # Win11 visual styles + PerMonitorV2 DPI manifest
│   ├── MiTVDaemon.java / .jar     # 3 KB on-demand stdin/stdout TV daemon (6–9ms backlight/volume)
│   ├── MtkDirectTool.jar          # Fallback MTK hardware backlight tool (MIT, Mimonitor_Toolbox)
│   └── VolDirectTool.jar          # Fallback AudioManager volume tool (MIT, redmi-g-pro-27u-monitorswitch)
├── docs/
│   ├── ARCHITECTURE.md            # Detailed human-facing architecture & macOS porting guide
│   └── screenshots/               # README showcase screenshots (flyout.jpg, settings.png, keymap.jpg)
├── extras/
│   └── Fcitx5TvFix/               # Optional Xposed/Vector module source fixing Fcitx5 on HyperOS TV
├── .github/workflows/release.yml  # CI build + automatic dual-zip GitHub Release publisher
└── src/
    ├── main.rs                    # Single-instance mutex, tray icon host, WM_* message hub, app lifecycle
    ├── config.rs                  # config.ini loader/saver (UTF-8 BOM safe), PATH scrcpy discovery, Run registry
    ├── adb.rs                     # ADB runner, status query, persistent key shell, MiTVDaemon coalesced worker
    ├── takeover.rs                # Invisible scrcpy launcher, unified WH_KEYBOARD_LL, WH_MOUSE_LL, DP watcher
    ├── keymap.rs                  # KeyChord, KeyAction (11 actions), parser/formatter, presets 1/2, validation
    ├── keymap_ui.rs               # Native Win32 click-to-record shortcut editor (temporary WH_KEYBOARD_LL)
    ├── flyout.rs                  # Double-buffered GDI+ Win11 rounded flyout, 35ms throttled sliders
    ├── gdi.rs                     # GDI+ lifecycle guard, custom vector icons, rounded rect/text primitives
    ├── settings_ui.rs             # Native Win32 ADB & input settings dialog
    └── ime.rs                     # Dynamic msctf.dll loader + ITfInputProcessorProfileMgr TSF IME save/restore
```

---

## 4. Critical Engineering Invariants & Pitfalls (DO NOT BREAK)

1. **Zero `RefCell` Re-entrancy Across Win32 Calls (`panic = "abort"` crashes immediately!)**
   - `FLYOUT_STATE`, `SETTINGS_STATE`, `KEYMAP_STATE`, and `APP_HOST` are `thread_local! { RefCell<Option<...>> }`.
   - **Rule**: NEVER call any Win32 API that synchronously dispatches window messages (`ShowWindow`, `SetWindowPos`, `SetForegroundWindow`, `DestroyWindow`, `ReleaseCapture`, `SetCapture`, `SetWindowTextW`, `Shell_NotifyIconW`, `emit_action`) while holding `.borrow()` or `.borrow_mut()`. Extract needed fields/handles, drop the borrow, THEN invoke Win32.

2. **Win32 Window Class Brush Lifetime**
   - Register each window class (`MiKVMSettingsWnd`, `MiKVMKeymapWnd`, etc.) once via `std::sync::Once` with a permanent class `hbrBackground`. Never delete that class brush in `WM_DESTROY` (only delete per-instance `HFONT` / `WM_CTLCOLOR*` brushes), otherwise reopening a dialog renders with a freed brush.

3. **Dynamic `msctf.dll` Loading (MinGW Compatibility)**
   - `x86_64-pc-windows-gnu` lacks `libmsctf.a`. NEVER add `#[link(name = "msctf")]`.
   - Keep `LoadLibraryW("msctf.dll")` + `GetProcAddress("TF_CreateInputProcessorProfiles")` cached in `OnceLock` in `src/ime.rs`.
   - Keep `CoInitializeEx(COINIT_APARTMENTTHREADED)` on the main UI thread in `src/main.rs` — required for TSF COM calls.

4. **Unified `WH_KEYBOARD_LL` (Never Re-introduce `RegisterHotKey`)**
   - Windows reserves `VK_F12` for debuggers and blocks `RegisterHotKey` on it; arbitrary user-recorded keys also conflict frequently.
   - All takeover hotkeys run in a single table-driven `WH_KEYBOARD_LL` (`src/takeover.rs`) installed ONLY while `HOOK_ACTIVE == true`.
   - Use lock-free 256-bit `SWALLOWED_VKS` (`[AtomicU64; 4]`) so `KEYUP`/`SYSKEYUP` are cleanly swallowed even if modifiers are released first.
   - Use `try_lock()` inside hook callbacks and `PostMessageW(WM_TRIGGER_RETURN_DP)` — NEVER block inside a low-level hook.

5. **Win11 TSF IME Save & Restore**
   - Legacy `WM_INPUTLANGCHANGEREQUEST` fails to restore Windows 11 TSF IMEs (Weasel/Rime, Microsoft Pinyin).
   - Keep `ITfInputProcessorProfileMgr::GetActiveProfile` before takeover, switch to `ENG` (`0x0409`) during takeover (plus 150ms `TIMER_IME_GUARD`), and restore immediately on exit + 200ms retry via `WM_RESTORE_IME_RETRY`.

6. **Fast DP Auto-Return Watcher**
   - Users often switch to TV and immediately click the DP tile on the TV launcher.
   - Keep `start_dp_return_watcher()` settle at `500ms`, polling `450ms` (before seeing non-ext) / `650ms` (after seeing non-ext), triggering `WM_DP_RETURNED` when `saw_non_ext || consecutive_ext >= 2`.

7. **`MiTVDaemon.jar` Lifecycle & Exit**
   - `ActivityThread.systemMain()` spawns non-daemon binder threads inside `app_process`. `MiTVDaemon.java` MUST call `System.exit(0)` after `Q`/EOF, otherwise the process hangs on TV!
   - `DeviceController` keeps `MiTVDaemon` warm ONLY while flyout is open or takeover is running (`set_keep_warm(true)`), and auto-shuts it down after 25s idle (`DAEMON_IDLE_SECS = 25`), plus explicit `stop_daemon_now()` on app exit.
   - Always fall back gracefully to `cmd media_session volume --stream 3 --set` (rootless) for volume and `MtkDirectTool.jar` for backlight if `MiTVDaemon` fails (e.g., non-rooted device).

8. **Agent Hidden-Desktop Constraint (Windows)**
   - Automated shell commands run on an isolated Windows desktop where `Shell_TrayWnd = 0`.
   - NEVER leave `MiKVM.exe` running from an automated shell command — it would hold `Local\MiKVM_SingleInstance` invisibly and prevent the user from launching their real tray icon!

9. **Repository Security & Hygiene**
   - NEVER commit: `config.ini` (personal LAN IP), `scrcpy/`, `*.exe`, `target/`, `Cargo.lock`, `*.jks`, `*.keystore`.

---

## 5. Verified TV Hardware & Protocol Reference (Redmi G Pro 27U)

| Feature | Verified Command / API | Root Required? |
| :--- | :--- | :---: |
| **Switch to TV System** | `cmd input keyevent 3` (`KEYCODE_HOME`) | No |
| **Switch to DP / HDMI 1 / HDMI 2** | `am start -a com.xiaomi.mitv.tvplayer.EXTSRC_PLAY -n com.xiaomi.mitv.tvplayer/.ExternalSourceActivity --ei input <29\|23\|24> -f 0x10000000` | No |
| **Query Active Source** | `settings get global mitv.tvplayer.hdmi.last.source` + `dumpsys window \| grep -E 'mCurrentFocus\|mFocusedApp'` (`ExternalSourceActivity` => external source, else TV system) | No |
| **XiaoAi Far-Field Voice (Onboard Mic)** | `am start-service -n com.xiaomi.voicecontrol/.VoiceControlService -a com.xiaomi.mitv.voice.ONBOARD_MIC_START` (stop: `ONBOARD_MIC_STOP`) | **No** |
| **Volume Read / Write (Rootless)** | `cmd media_session volume --stream 3 --get` / `--set <0..100>` (~68ms) | **No** |
| **Volume & Backlight (6–9ms Daemon)** | `su 1000 -c 'CLASSPATH=/data/local/tmp/MiTVDaemon.jar /system/bin/app_process /data/local/tmp MiTVDaemon'` (stdin: `B <v>`, `V <v>`, `G`, `Q`) | Yes (`uid 1000`) |
| **Backlight Direct Reflection Signature** | `com.mediatek.twoworlds.tv.TVNative`: `getConfigValue_native(int, String)` & `setConfigValue_native(int, String, int, int)` with `(0, "g_disp__disp_back_light", val, 3)` | Yes (`uid 1000`) |
| **Backlight System Sync (on release)** | `am broadcast -a com.xiaomi.mitv.action.PIC_MODE_CHANGED --ei picmode 7` + `settings put global picture_backlight <v>` + `settings put global xiaomi_picture_backlight <v>` | No (broadcast/settings) |
| **TV Remote Key Notes** | `KEYCODE_MENU = 82` (≡ button); `KEYCODE_APP_SWITCH = 187` is stripped by HyperOS TV firmware (intentionally removed from `KeyAction`). Physical BLE remote (`Vendor_2717_Product_32b8.kl`) uses BLE GATT audio stream for its own mic, whereas `ONBOARD_MIC_START` uses the monitor's far-field mic. | No |
| **TV Chinese IME Compatibility** | **Rootless**: Trime (`com.osfans.trime/.ime.core.TrimeInputMethodService`) works out of the box after initial deploy (optional `appops set com.osfans.trime MANAGE_EXTERNAL_STORAGE allow` for `/sdcard/rime`). **Rooted**: Fcitx5 Android requires `extras/Fcitx5TvFix` scoped only to `org.fcitx.fcitx5.android`. | Trime: No / Fcitx5 fix: Yes |
