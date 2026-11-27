# AGENTS.md — MiKVM Maintenance & Engineering Guide

> This file is the canonical context handoff for AI coding agents and maintainers working on **MiKVM** across sessions. Read this before modifying Win32 hooks, direct `scrcpy-server` protocol, `MiTVEdge` / `MiTVDaemon`, GDI+ UI, or release workflows.

---

## 1. Project Identity & Design Invariants

- **Project**: `MiKVM` (`https://github.com/Reeeabc/MiKVM`, MIT License, owner `Reeeabc`)
- **Target Device**: Redmi Display G Pro 27U (`MiTV-MFFU1`, codename `beekeeper`, MT9655 SoC, HyperOS TV / Android 14). Also compatible with other Xiaomi/Redmi HyperOS / MIUI TV displays via configurable `DpInput / Hdmi1Input / Hdmi2Input`.
- **Tech Stack**: Pure **Rust** + native **Win32 / GDI+ / DWM / Raw Input / QueryDisplayConfig** with a single crate dependency (`windows-sys = "0.59"`). Direct TCP client to `scrcpy-server` (no `scrcpy.exe`, SDL3, or FFmpeg process!).
- **Non-Negotiable Quality Targets**:
  - **Zero external runtime dependencies** (only built-in Windows system DLLs — never link MinGW runtime DLLs, VC++ redist, `.NET`, or WebView2).
  - **Ultra-small binary**: ~500 KB stripped LTO release `.exe` (`opt-level = "z"`, `lto = true`, `codegen-units = 1`, `panic = "abort"`, `strip = true`).
  - **Ultra-low memory**: ~3–6 MB steady-state Private Working Set, **0% idle CPU**, **0 extra idle child processes** on both PC and TV.
  - **Clean C: drive policy (local machine)**: Never install toolchains, SDKs, or build caches on `C:`.

---

## 2. Local Environment & Build Commands

### Local Toolchain Paths (Windows)
- Rust: `RUSTUP_HOME=D:\develop\rustup`, `CARGO_HOME=D:\develop\cargo` (`stable-x86_64-pc-windows-gnu`)
- MinGW GCC / `windres` / `objdump`: `D:\develop\mingw64\bin`
- JDK (`javac` / `java`): `D:\develop\Java\jdk\bin`
- Repository root: `D:\develop\projects\mitv\MiKVM` (remote `origin` uses SSH: `git@github.com:Reeeabc/MiKVM.git`)
- Legacy C# v1.0 reference is preserved permanently on git tag `v1.0-csharp`.

### Build & Test `MiKVM.exe` Locally (PowerShell)
```powershell
$env:RUSTUP_HOME = "D:\develop\rustup"
$env:CARGO_HOME = "D:\develop\cargo"
$env:Path = "D:\develop\cargo\bin;D:\develop\mingw64\bin;$env:Path"
Set-Location "D:\develop\projects\mitv\MiKVM"
cargo test
cargo build --release
Copy-Item "target\release\MiKVM.exe" ".\MiKVM.exe" -Force
# Verify zero non-system DLL dependencies:
objdump -p ".\MiKVM.exe" | Select-String "DLL Name"
```
Expected DLL list ONLY (Windows system DLLs): `advapi32`, `api-ms-win-core-synch-l1-2-0`, `api-ms-win-shcore-scaling-l1-1-1`, `dwmapi`, `gdi32`, `gdiplus`, `KERNEL32`, `msvcrt`, `ntdll`, `ole32`, `shell32`, `user32`, `ws2_32`.

### Rebuild `assets/MiTVDaemon.jar` or `assets/MiTVEdge.jar`
Compile with Java 8 bytecode (`D:\develop\Java\jdk\bin\javac.exe --release 8 -d target\dex_tmp\out assets\<Name>.java`; for `MiTVEdge.java`, pass `-sourcepath target\dex_tmp\stubs` with compile-only `android.content.{Context,ContextWrapper,ContentResolver,IContentProvider,AttributionSource}` stubs), then `Set-Location target\dex_tmp\out` and run `java -cp ..\dalvik-dx.jar com.android.dx.command.Main --dex --no-strict --output=...\assets\<Name>.jar <Name>*.class` (excluding `android/` stub classes; download `dalvik-dx-9.0.0_r3.jar` temporarily into `target/dex_tmp/` on `D:` and delete `target/dex_tmp/` immediately afterward — never cache on `C:`). Note that `ensure_jars_deployed()` compares file size (`wc -c`) so changed jar sizes auto-push to the TV on connect.

### Release Process (GitHub Actions)
- Pushing any `v*` tag (e.g., `git tag -a v2.1.0 -m "..." && git push origin v2.1.0`) triggers `.github/workflows/release.yml` on `windows-latest` (MSVC).
- Automatically produces and attaches to GitHub Releases:
  - `MiKVM-windows-x64-full.zip` (bundles official `adb` + `scrcpy-server` under `scrcpy/`)
  - `MiKVM-windows-x64-lite.zip` (~180 KB binary + `assets/*.jar` + docs)
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
│   ├── MiTVEdge.java / .jar       # 8.5 KB self-contained rootless 3px TV edge hover sensor (Phase 2)
│   ├── MtkDirectTool.jar          # Fallback MTK hardware backlight tool (MIT, Mimonitor_Toolbox)
│   └── VolDirectTool.jar          # Fallback AudioManager volume tool (MIT, redmi-g-pro-27u-monitorswitch)
├── docs/
│   ├── ARCHITECTURE.md            # Detailed human-facing architecture & macOS porting guide
│   └── screenshots/               # README showcase screenshots (flyout.jpg, settings.jpg, keymap.jpg)
├── extras/
│   └── Fcitx5TvFix/               # Optional Xposed/Vector module source fixing Fcitx5 on HyperOS TV
├── .github/workflows/release.yml  # CI build + automatic dual-zip GitHub Release publisher
└── src/
    ├── main.rs                    # Single-instance mutex, tray host, WM_INPUT/DISPLAY/POWER/SETTING hub
    ├── diag.rs                    # Opt-in rotating diagnostic logger (DebugLog=True / --debug-log -> mikvm.log)
    ├── config.rs                  # config.ini loader/saver (UTF-8 BOM safe), theme query, PATH discovery, autostart
    ├── ddc.rs                     # Rootless DDC/CI VCP 0x10 brightness via dynamic dxva2.dll (MinGW & MSVC safe)
    ├── lan_scan.rs                # Concurrent /24 LAN port-5555 ADB scanner via dynamic iphlpapi.dll + prop ranking
    ├── adb.rs                     # ADB runner, status query, force-reconnect, DDC fallback, MiTVDaemon worker
    ├── scrcpy_proto.rs            # Direct scrcpy-server 3.0+ TCP client, UHID mouse, primed burst, keys, clipboard
    ├── display.rs                 # QueryDisplayConfig topology, Xiaomi EDID 0xA961, lid/sleep/resume power events
    ├── clipboard.rs               # Win32 CF_UNICODETEXT sync with password-manager privacy format filtering
    ├── takeover/                  # Modular 4-state multi-screen KVM subsystem
    │   ├── mod.rs                 # TakeoverSession state machine, topology reconcile, sleep/Wi-Fi auto-recovery
    │   ├── state.rs               # Shared lock-free atomics, TakeoverMode, HELD_MODS, SWALLOWED_VKS bitset
    │   ├── hooks.rs               # WH_KEYBOARD_LL, WH_MOUSE_LL, WM_INPUT Raw Input, SendInput flush, StickyKeysGuard
    │   ├── edge.rs                # PC elastic edge trigger window, TvEdgeSensor (MiTVEdge), 3.0x primed alignment
    │   └── dp_watcher.rs          # Persistent-shell adb DP return watcher
    ├── keymap.rs                  # KeyChord, KeyAction (12 actions incl. ToggleFocus), parser/formatter, presets 1/2
    ├── keymap_ui.rs               # Native Win32 click-to-record shortcut editor (multi-monitor aware)
    ├── flyout.rs                  # Double-buffered GDI+ Win11 rounded flyout (Light/Dark theme auto-sync)
    ├── gdi.rs                     # GDI+ lifecycle guard, ThemePalette, custom vector icons, DWM dark titlebar
    └── settings_ui.rs             # Native Win32 ADB, multi-screen edge resistance (0..300), and clipboard dialog
```

---

## 4. Critical Engineering Invariants & Pitfalls (DO NOT BREAK)

1. **Zero `RefCell` Re-entrancy Across Win32 Calls (`panic = "abort"` crashes immediately!)**
   - `FLYOUT_STATE`, `SETTINGS_STATE`, `KEYMAP_STATE`, and `APP_HOST` are `thread_local! { RefCell<Option<...>> }`.
   - **Rule**: NEVER call any Win32 API that synchronously dispatches window messages (`ShowWindow`, `SetWindowPos`, `SetForegroundWindow`, `DestroyWindow`, `ReleaseCapture`, `SetCapture`, `SetWindowTextW`, `Shell_NotifyIconW`, `emit_action`) while holding `.borrow()` or `.borrow_mut()`. Extract needed fields/handles, drop the borrow, THEN invoke Win32.

2. **Win32 Window Class Brush Lifetime**
   - Register each window class (`MiKVMSettingsWnd`, `MiKVMKeymapWnd`, `MiKVMEdgeTriggerWnd`, etc.) once via `std::sync::Once` with a permanent class `hbrBackground`. Never delete that class brush in `WM_DESTROY` (only delete per-instance `HFONT` / `WM_CTLCOLOR*` brushes), otherwise reopening a dialog renders with a freed brush.

3. **Direct `scrcpy-server` Control Engine & Modifier Isolation (`HELD_MODS` + `SendInput` Flush)**
   - Swallowing physical `Ctrl`/`Shift`/`Alt` keys in `WH_KEYBOARD_LL` prevents Windows from updating `GetAsyncKeyState`. Therefore `TvFocused`/`TvLocked` MUST track modifiers exclusively via lock-free `HELD_MODS` (`tv_held_mod_mask()` / `android_metastate_from_held()`), NEVER reading `GetAsyncKeyState` during TV capture!
   - Conversely, when switching PC → TV via a combo hotkey (e.g., `Ctrl+Alt+S` / `Ctrl+Shift+S`), call `release_pc_modifiers_and_swallow(true)` (`SendInput(KEYEVENTF_KEYUP)` + `0xFF` Alt/Win menu mask) *before* flipping `MODE_ATOMIC`, otherwise Windows gets stuck with `Ctrl`/`Shift` held down when the user releases the keys while in `TvFocused`!
   - Suppress Windows 5x-Shift StickyKeys popup via `StickyKeysGuard` (`SPI_SETSTICKYKEYS` without registry write) strictly while capturing TV, and restore on standby/exit.
   - Because there is no focus-stealing `scrcpy.exe` window, Windows IMEs are never disturbed (`src/ime.rs` is retired).

4. **Unified `WH_KEYBOARD_LL` (Never Re-introduce `RegisterHotKey`)**
   - Windows reserves `VK_F12` for debuggers and blocks `RegisterHotKey` on it.
   - `TvStandby` keeps `WH_KEYBOARD_LL` active *only* for `KeyAction::is_standby_global()` (`ReturnDp` and `ToggleFocus`), passing all other keys straight to Windows; `Idle` uninstalls hooks completely.
   - Use lock-free 256-bit `SWALLOWED_VKS` (`[AtomicU64; 4]`) and `try_lock()` inside hook callbacks; NEVER block inside a low-level hook.

5. **Single-Frame Primed Burst PC → TV `y`-Alignment (`3.000x` VelocityControl)**
   - Measured live on Redmi G Pro 27U (Android 14): `XScale = YScale = 1.000`, `PointerVelocityControlParameters(scale=1.0, lowThreshold=500, highThreshold=3000, acceleration=3.0)`, `STOP_TIME = 200ms`.
   - Packet 1 after `>200ms` idle always runs at `1.0x`, while back-to-back packets run at exact linear `3.0x`. Always use `send_primed_mouse_burst(pin_dx, 0, 0, counts)` (Packet 1 pins to edge with `0` vertical delta & primes `VelocityControl`, Packets 2..N move vertically at exact `3.0x` -> `±1px` error across 1080p in 1 VSync frame!).
   - Wi-Fi ADB hover confirmation latency can spike to `62–125ms` — keep `wait_edge_seq` timeout at `160ms` (never shrink below `140ms`).

6. **`MiTVEdge.jar` Self-Contained Rootless Overlay & Anti-Drop Hover Protocol**
   - Under `uid 2000` (`adb shell`), `MiTVEdge.jar` runs **100% standalone** via `appops set com.android.shell SYSTEM_ALERT_WINDOW allow` + `CLASSPATH=/data/local/tmp/MiTVEdge.jar /system/bin/app_process /data/local/tmp MiTVEdge <side>` with zero dependency on `scrcpy-server.jar` internal classes (it ships its own `SafeContentResolver` with a dynamic `IContentProvider` proxy, `ShellContext`, and `ActivityThread` `mBoundApplication` / `mConfigurationController` bootstrap).
   - NEVER throttle away large hover jumps in `MiTVEdge.java`: always emit immediately on `ACTION_HOVER_ENTER` or `|r10k - lastEmittedR10k| >= 60`, otherwise multi-packet bursts lose their final landed coordinate!
   - Cache `last_side` in both Rust `TvEdgeSensor` and Java `updateSideOnMain` so PC ↔ TV switches never trigger redundant `wmUpdateView` relayouts.

7. **Multi-Monitor Ghost-Screen Handling, Overlap Seam Mapping & `TvEdgeSide` Persistence (`QueryDisplayConfig`)**
   - Redmi G Pro 27U keeps DP HPD continuously active even when switched to Android TV. Use `QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS)` + Xiaomi EDID `0xA961` / `"Mi Monitor"` matching to separate `visible_pc_monitors` from `tv_ghost_monitor`, deduplicating Clone/Duplicate mode by `source_key`.
   - `DisplayTopology::seam_span` computes the actual shared overlap interval between the TV ghost screen and all adjacent PC monitors on that seam (handling staggered dual-screen, top/bottom, and 3-screen layouts); both `compute_pc_entry_ratio_10k` and `DisplayTopology::point_on_pc_edge` map strictly against `seam_span`.
   - `persist_detected_edge_side()` auto-saves Extend-mode `TvEdgeSide` to `config.ini` whenever it changes (at startup, on `WM_DISPLAYCHANGE`, and BEFORE `AutoInternalOnTv` collapses to single-screen).
   - Always position flyout / Settings / Keymap dialogs via `visible_work_area_for_cursor(...)` so UI never pops onto the invisible 27U primary ghost screen.

8. **Persistent-Shell Fast DP Auto-Return Watcher, IP Auto-Rediscovery & `MiTVDaemon.jar` Lifecycle**
   - `start_dp_return_watcher()` reuses a single persistent `adb shell` stream (falling back to one-shot `dev.shell` only if the pipe dies) to avoid spawning thousands of `adb.exe` processes during long multi-screen sessions.
   - On `PBT_APMSUSPEND`, active TV hooks and sockets are torn down immediately so wake never freezes input; on `PBT_APMRESUMEAUTOMATIC`/`PBT_APMRESUMESUSPEND` (3s debounced) or unexpected `WM_CONTROL_EXITED` (Wi-Fi drop), bounded backoff recovery runs `DeviceController::reconnect_force()` (`adb disconnect` + `connect`) and, if all direct retries fail (e.g. router DHCP reassigned IP), falls back once to `lan_scan::rediscover_xiaomi_tv()` (strictly matching verified Xiaomi/Redmi devices), auto-updating `config.ini` and silently restoring into safe `TvStandby` (never stealing PC focus) or cleanly exiting via `WM_DP_RETURNED` if the TV already switched back to DP. Manual source clicks and explicit red-dot reconnect also invoke LAN rediscovery on failure.
   - `ActivityThread.systemMain()` spawns non-daemon binder threads inside `app_process`. Both `MiTVDaemon.java` and `MiTVEdge.java` MUST call `System.exit(0)` after `Q`/EOF!
   - `DeviceController` keeps `MiTVDaemon` warm ONLY while flyout is open or a TV session is active, and auto-shuts it down after 25s idle (`DAEMON_IDLE_SECS = 25`), plus explicit `stop_daemon_now()` on exit.
   - Keep `dxva2.dll` (`src/ddc.rs`, rootless DDC/CI VCP `0x10` fallback) and `iphlpapi.dll` (`src/lan_scan.rs`, local IPv4 subnet enumeration) dynamically loaded via `LoadLibraryW` + `GetProcAddress` so MinGW GNU and MSVC builds stay 100% warning-free with zero extra static link libraries.

9. **Agent Hidden-Desktop Constraint (Windows)**
   - Automated shell commands run on an isolated Windows desktop where `Shell_TrayWnd = 0`.
   - NEVER leave `MiKVM.exe` running from an automated shell command — it would hold `Local\MiKVM_SingleInstance` invisibly and prevent the user from launching their real tray icon!

10. **Repository Security & Hygiene**
    - NEVER commit: `config.ini` (personal LAN IP), `scrcpy/`, `*.exe`, `target/`, `Cargo.lock`, `*.jks`, `*.keystore`.

---

## 5. Verified TV Hardware & Protocol Reference (Redmi G Pro 27U)

| Feature | Verified Command / API | Root Required? |
| :--- | :--- | :---: |
| **Switch to TV System** | `cmd input keyevent 3` (`KEYCODE_HOME`) | No |
| **Switch to DP / HDMI 1 / HDMI 2** | `am start -a com.xiaomi.mitv.tvplayer.EXTSRC_PLAY -n com.xiaomi.mitv.tvplayer/.ExternalSourceActivity --ei input <29\|23\|24> -f 0x10000000` | No |
| **Query Active Source** | `settings get global mitv.tvplayer.hdmi.last.source` + `dumpsys window \| grep -E 'mCurrentFocus\|mFocusedApp'` (`ExternalSourceActivity` => external source, else TV system) | No |
| **Direct Control Channel (`scrcpy-server 3.0+`)** | `CLASSPATH=/data/local/tmp/scrcpy-server.jar app_process / com.genymobile.scrcpy.Server <auto-ver> scid=<hex> log_level=warn video=false audio=false control=true tunnel_forward=true send_device_meta=false send_dummy_byte=true cleanup=false clipboard_autosync=true` (`TYPE_INJECT_KEYCODE=0`, `TYPE_SET_CLIPBOARD=9`, `TYPE_UHID_CREATE=12`, `TYPE_UHID_INPUT=13`, `TYPE_UHID_DESTROY=14`; version auto-detected from sibling `scrcpy.exe --version` or on-device probe) | **No** |
| **TV Edge Hover Sensor (`MiTVEdge.jar`)** | `appops set com.android.shell SYSTEM_ALERT_WINDOW allow` + `CLASSPATH=/data/local/tmp/MiTVEdge.jar /system/bin/app_process /data/local/tmp MiTVEdge RIGHT` (`1920x1080` logical overlay, 100% self-contained, `READY`/`H`/`L`/`S`/`Q`) | **No** |
| **XiaoAi Far-Field Voice (Onboard Mic)** | `am start-service -n com.xiaomi.voicecontrol/.VoiceControlService -a com.xiaomi.mitv.voice.ONBOARD_MIC_START` (stop: `ONBOARD_MIC_STOP`) | **No** |
| **Volume Read / Write (Rootless)** | `cmd media_session volume --stream 3 --get` / `--set <0..100>` (~68ms) | **No** |
| **Volume & Backlight (6–9ms Daemon)** | `su 1000 -c 'CLASSPATH=/data/local/tmp/MiTVDaemon.jar /system/bin/app_process /data/local/tmp MiTVDaemon'` (stdin: `B <v>`, `V <v>`, `G`, `Q`) | Yes (`uid 1000`) |
| **Backlight Direct Reflection Signature** | `com.mediatek.twoworlds.tv.TVNative`: `getConfigValue_native(int, String)` & `setConfigValue_native(int, String, int, int)` with `(0, "g_disp__disp_back_light", val, 3)` | Yes (`uid 1000`) |
| **Backlight System Sync (on release)** | `am broadcast -a com.xiaomi.mitv.action.PIC_MODE_CHANGED --ei picmode 7` + `settings put global picture_backlight <v>` + `settings put global xiaomi_picture_backlight <v>` | No (broadcast/settings) |
| **TV Remote Key Notes** | `KEYCODE_MENU = 82` (≡ button); `KEYCODE_APP_SWITCH = 187` is stripped by HyperOS TV firmware. Physical BLE remote (`Vendor_2717_Product_32b8.kl`) uses BLE GATT audio stream for its own mic, whereas `ONBOARD_MIC_START` uses the monitor's far-field mic. | No |
| **TV Chinese IME Compatibility** | **Rootless**: Trime (`com.osfans.trime/.ime.core.TrimeInputMethodService`) works out of the box after initial deploy (optional `appops set com.osfans.trime MANAGE_EXTERNAL_STORAGE allow` for `/sdcard/rime`). **Rooted**: Fcitx5 Android requires `extras/Fcitx5TvFix` scoped only to `org.fcitx.fcitx5.android`. | Trime: No / Fcitx5 fix: Yes |
