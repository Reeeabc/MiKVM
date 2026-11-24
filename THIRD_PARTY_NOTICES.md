# Third-Party Notices

MiKVM includes or interoperates with the following open-source projects:

## 1. redmi-g-pro-27u-monitorswitch (MIT License)
- Repository: https://github.com/HanHan666666/redmi-g-pro-27u-monitorswitch
- License: MIT
- Used components / references:
  - `assets/VolDirectTool.jar` (direct Android AudioManager volume control via `app_process`)
  - Input source switching commands and tray flyout UI inspiration

## 2. Mimonitor_Toolbox (MIT License)
- Repository: https://github.com/YiHoooong/Mimonitor_Toolbox
- License: MIT
- Used components / references:
  - `assets/MtkDirectTool.jar` (MTK display backlight register read/write fallback)
  - Redmi G Pro 27U input source IDs and backlight PQ refresh broadcast

## 3. scrcpy (Apache License 2.0)
- Repository: https://github.com/Genymobile/scrcpy
- License: Apache-2.0
- Bundled in the `MiKVM-windows-x64-full.zip` release package (under `scrcpy/`, unmodified official Windows 64-bit build) for video-less UHID mouse and SDK raw-key-events keyboard forwarding over wireless ADB.

## 4. windows-sys (MIT / Apache-2.0)
- Repository: https://github.com/microsoft/windows-rs
- License: MIT OR Apache-2.0
- Rust Win32 API bindings used at compile time.
