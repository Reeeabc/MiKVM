using System;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using System.Windows.Forms;

namespace MiTVSwitch
{
    sealed class TrayAppContext : ApplicationContext
    {
        private const int HotkeyReturnDpId = 9001;
        private const int HotkeyBackId = 9002;
        private const int HotkeyHomeId = 9003;
        private const int HotkeyMenuId = 9004;
        private const int HotkeyVolUpId = 9005;
        private const int HotkeyVolDownId = 9006;
        private const int HotkeyMuteId = 9007;
        private const int HotkeyPowerId = 9008;

        private readonly string _scrcpyDir;
        private readonly string _scrcpyPath;
        private readonly AppConfig _cfg;
        private readonly DeviceController _dev;
        private readonly FlyoutForm _flyout;
        private readonly NotifyIcon _trayIcon;
        private readonly ContextMenuStrip _rightMenu;
        private readonly ToolStripMenuItem _itemToggleControl;
        private readonly Icon _idleIcon;
        private readonly Icon _activeIcon;
        private readonly HotkeyMessageWindow _hotkeyWindow;
        private readonly SynchronizationContext _uiCtx;
        private readonly NativeMethods.LowLevelMouseProc _mouseHookProc;
        private readonly NativeMethods.LowLevelKeyboardProc _keyHookProc;
        private readonly System.Windows.Forms.Timer _imeGuardTimer;
        private readonly IntPtr _englishHkl;

        private Process _scrcpyProc;
        private CancellationTokenSource _watchCts;
        private IntPtr _mouseHookHandle = IntPtr.Zero;
        private IntPtr _keyHookHandle = IntPtr.Zero;
        private bool _f12Down;
        private NativeMethods.TF_INPUTPROCESSORPROFILE _savedTsfProfile;
        private IntPtr _savedUserImeOpen = IntPtr.Zero;
        private bool _hasSavedUserIme;
        private int _lastWheelTick;
        private bool _hotkeysRegistered;
        private volatile bool _busy;

        public TrayAppContext()
        {
            _uiCtx = SynchronizationContext.Current ?? new WindowsFormsSynchronizationContext();
            string baseDir = AppDomain.CurrentDomain.BaseDirectory;
            _scrcpyDir = Path.Combine(baseDir, "scrcpy");
            _scrcpyPath = Path.Combine(_scrcpyDir, "scrcpy.exe");
            string adbPath = Path.Combine(_scrcpyDir, "adb.exe");
            string assetsDir = Path.Combine(baseDir, "assets");
            string configPath = Path.Combine(baseDir, "config.ini");

            _cfg = new AppConfig(configPath);
            _dev = new DeviceController(adbPath, assetsDir, _cfg);

            _idleIcon = FluentGraphics.CreateTrayIcon(false);
            _activeIcon = FluentGraphics.CreateTrayIcon(true);

            _hotkeyWindow = new HotkeyMessageWindow();
            _hotkeyWindow.HotkeyPressed += OnTvHotkeyPressed;
            _mouseHookProc = MouseHookCallback;
            _keyHookProc = KeyboardHookCallback;

            _englishHkl = FindEnglishKeyboardLayout();
            _imeGuardTimer = new System.Windows.Forms.Timer { Interval = 150 };
            _imeGuardTimer.Tick += (s, e) => EnforceScrcpyEnglishIme(false);

            // Win11 Fluent Flyout Panel
            _flyout = new FlyoutForm();
            _flyout.SourceClicked += OnFlyoutSourceClicked;
            _flyout.BacklightChanged += v => Task.Run(() => _dev.SetBacklightAsync(v));
            _flyout.VolumeChanged += v => Task.Run(() => _dev.SetVolumeAsync(v));
            _flyout.SettingsClicked += ShowSettingsDialog;
            _flyout.RefreshClicked += () => RefreshFlyoutStatusAsync(false);
            _flyout.ExitClicked += ExitApp;

            // Right-click fast tray menu
            _rightMenu = new ContextMenuStrip
            {
                ShowImageMargin = false,
                Font = new Font("Microsoft YaHei UI", 9.5f)
            };
            var miOpen = new ToolStripMenuItem("打开控制面板");
            miOpen.Font = new Font(_rightMenu.Font, FontStyle.Bold);
            miOpen.Click += (s, e) => ToggleFlyout();

            var miTv = new ToolStripMenuItem("切换到电视系统");
            miTv.Click += (s, e) => OnFlyoutSourceClicked(SignalSource.TvSystem);

            var miDp = new ToolStripMenuItem("切回电脑 DP");
            miDp.Click += (s, e) => OnFlyoutSourceClicked(SignalSource.Dp);

            _itemToggleControl = new ToolStripMenuItem("仅开启键鼠接管");
            _itemToggleControl.Click += (s, e) =>
            {
                if (IsScrcpyRunning()) StopControlSession();
                else RunBusyAsync(() => StartScrcpySessionAsync(false));
            };

            var miSettings = new ToolStripMenuItem("ADB 设置...");
            miSettings.Click += (s, e) => ShowSettingsDialog();

            var miExit = new ToolStripMenuItem("退出");
            miExit.Click += (s, e) => ExitApp();

            _rightMenu.Items.AddRange(new ToolStripItem[]
            {
                miOpen, new ToolStripSeparator(),
                miTv, miDp, _itemToggleControl, new ToolStripSeparator(),
                miSettings, miExit
            });

            _trayIcon = new NotifyIcon
            {
                Icon = _idleIcon,
                Text = "G Pro 27U 信号源 / 背光 / 键鼠控制",
                ContextMenuStrip = _rightMenu,
                Visible = true
            };

            _trayIcon.MouseUp += (s, e) =>
            {
                if (e.Button == MouseButtons.Left)
                {
                    ToggleFlyout();
                }
            };
        }

        private void ToggleFlyout()
        {
            if (_flyout.Visible)
            {
                _flyout.HideFlyout();
                return;
            }
            if ((DateTime.UtcNow - _flyout.LastHiddenUtc).TotalMilliseconds < 220)
            {
                return;
            }
            _flyout.ShowNearTray();
            RefreshFlyoutStatusAsync(true);
        }

        private void RefreshFlyoutStatusAsync(bool silentIfConnected)
        {
            _flyout.SetRefreshing(_cfg.TvAddress, silentIfConnected);
            Task.Run(async () =>
            {
                var st = await _dev.QueryStatusAsync();
                PostUi(() => _flyout.UpdateStatus(st, _cfg.TvAddress, IsScrcpyRunning()));
            });
        }

        private void OnFlyoutSourceClicked(SignalSource src)
        {
            if (src == SignalSource.TvSystem)
            {
                if (_cfg.AutoTakeoverOnTv) SaveCurrentUserIme();
                _flyout.HideFlyout();
                RunBusyAsync(async () =>
                {
                    if (!await _dev.EnsureConnectedAsync())
                    {
                        PostUi(() => ShowTip("连接电视失败", "无法连接到 " + _cfg.TvAddress, ToolTipIcon.Warning));
                        return;
                    }
                    if (_cfg.AutoTakeoverOnTv)
                    {
                        bool ok = await StartScrcpySessionAsync(false);
                        if (!ok) return;
                    }
                    await _dev.SwitchToTvDesktopAsync();
                    PostUi(() => _flyout.SetOptimisticSource(SignalSource.TvSystem, _cfg.TvAddress, IsScrcpyRunning()));
                    if (_cfg.AutoTakeoverOnTv && IsScrcpyRunning())
                    {
                        StartDpReturnWatcher();
                    }
                });
            }
            else
            {
                int inputId = src == SignalSource.Hdmi1 ? _cfg.Hdmi1Input
                            : src == SignalSource.Hdmi2 ? _cfg.Hdmi2Input
                            : _cfg.DpInput;
                RunBusyAsync(async () =>
                {
                    PostUi(StopControlSession);
                    if (!await _dev.EnsureConnectedAsync())
                    {
                        PostUi(() => ShowTip("连接电视失败", "无法连接到 " + _cfg.TvAddress, ToolTipIcon.Warning));
                        return;
                    }
                    await _dev.SwitchToExternalAsync(inputId);
                    PostUi(() => _flyout.SetOptimisticSource(src, _cfg.TvAddress, false));
                });
            }
        }

        private void ShowSettingsDialog()
        {
            using (var dlg = new SettingsDialog(_cfg, _dev))
            {
                if (dlg.ShowDialog() == DialogResult.OK)
                {
                    string addr = dlg.ResultAddress;
                    if (!string.IsNullOrEmpty(addr))
                    {
                        if (!addr.Contains(":")) addr += ":5555";
                        if (!addr.Equals(_cfg.TvAddress, StringComparison.OrdinalIgnoreCase))
                        {
                            _cfg.TvAddress = addr;
                            _dev.ResetJarCache();
                        }
                    }
                    _cfg.AutoTakeoverOnTv = dlg.ResultAutoTakeover;
                    _cfg.WheelAsDpad = dlg.ResultWheelAsDpad;
                    _cfg.Save();
                    AppConfig.SetAutoStart(dlg.ResultAutoStart);

                    if (IsScrcpyRunning())
                    {
                        if (_cfg.WheelAsDpad) InstallMouseWheelHook();
                        else UninstallMouseWheelHook();
                    }
                }
            }
        }

        private void RunBusyAsync(Func<Task> work)
        {
            if (_busy) return;
            _busy = true;
            Task.Run(async () =>
            {
                try { await work(); }
                catch (Exception ex) { PostUi(() => ShowTip("操作失败", ex.Message, ToolTipIcon.Error)); }
                finally { _busy = false; }
            });
        }

        private async Task<bool> StartScrcpySessionAsync(bool autoWatchReturnToDp)
        {
            if (!File.Exists(_scrcpyPath))
            {
                PostUi(() => ShowTip("缺少组件", "未找到 " + _scrcpyPath, ToolTipIcon.Error));
                return false;
            }
            if (IsScrcpyRunning())
            {
                PostUi(() => FocusAndCaptureScrcpy(_scrcpyProc));
                return true;
            }

            // Fixed SDK keyboard (--raw-key-events) + UHID relative mouse (borderless invisible capture window)
            var psi = new ProcessStartInfo
            {
                FileName = _scrcpyPath,
                WorkingDirectory = _scrcpyDir,
                Arguments = string.Format(
                    "--serial={0} --no-video --no-audio --keyboard=sdk --raw-key-events --mouse=uhid --mouse-bind=+hbh:b+++ --window-borderless --window-width=120 --window-height=120 --window-title=\"MiTV-Switch-Capture\"",
                    _cfg.TvAddress),
                UseShellExecute = false,
                CreateNoWindow = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true
            };

            var proc = new Process { StartInfo = psi, EnableRaisingEvents = true };
            var errBuf = new StringBuilder();
            proc.ErrorDataReceived += (s, e) => { if (e.Data != null) errBuf.AppendLine(e.Data); };
            proc.OutputDataReceived += (s, e) => { };
            proc.Exited += (s, e) => PostUi(OnScrcpyExited);

            if (!proc.Start())
            {
                PostUi(() => ShowTip("启动键鼠接管失败", "无法启动 scrcpy.exe", ToolTipIcon.Error));
                return false;
            }
            proc.BeginOutputReadLine();
            proc.BeginErrorReadLine();
            _scrcpyProc = proc;

            for (int i = 0; i < 130; i++)
            {
                if (proc.HasExited)
                {
                    string msg = errBuf.ToString().Trim();
                    if (msg.Length > 200) msg = msg.Substring(0, 200);
                    PostUi(() => ShowTip("键鼠接管启动失败", string.IsNullOrEmpty(msg) ? "scrcpy 异常退出" : msg, ToolTipIcon.Error));
                    return false;
                }
                proc.Refresh();
                if (proc.MainWindowHandle != IntPtr.Zero)
                {
                    MakeScrcpyWindowInvisible(proc.MainWindowHandle);
                    break;
                }
                await Task.Delay(30);
            }

            await Task.Delay(250);
            if (proc.HasExited) return false;

            PostUi(() =>
            {
                SaveCurrentUserIme();
                UpdateTakeoverUi(true);
                _dev.StartPersistentShell();
                RegisterTvHotkeys();
                if (_cfg.WheelAsDpad) InstallMouseWheelHook();
                FocusAndCaptureScrcpy(proc);
                EnforceScrcpyEnglishIme(true);
                _imeGuardTimer.Start();
            });

            if (autoWatchReturnToDp) StartDpReturnWatcher();
            return true;
        }

        private void MakeScrcpyWindowInvisible(IntPtr hwnd)
        {
            try
            {
                long ex = NativeMethods.GetWindowLongPtr(hwnd, NativeMethods.GWL_EXSTYLE).ToInt64();
                ex = (ex | NativeMethods.WS_EX_LAYERED | NativeMethods.WS_EX_TOOLWINDOW) & ~NativeMethods.WS_EX_APPWINDOW;
                NativeMethods.SetWindowLongPtr(hwnd, NativeMethods.GWL_EXSTYLE, new IntPtr(ex));
                // alpha=1 is completely invisible to the eye while still accepting focus and hit-testing
                NativeMethods.SetLayeredWindowAttributes(hwnd, 0, 1, NativeMethods.LWA_ALPHA);
            }
            catch { }
        }

        private void FocusAndCaptureScrcpy(Process proc)
        {
            try
            {
                if (proc == null || proc.HasExited) return;
                proc.Refresh();
                IntPtr hwnd = proc.MainWindowHandle;
                if (hwnd == IntPtr.Zero) return;

                MakeScrcpyWindowInvisible(hwnd);
                NativeMethods.ShowWindow(hwnd, NativeMethods.SW_RESTORE);
                NativeMethods.SetForegroundWindow(hwnd);

                NativeMethods.RECT clip;
                var screen = Screen.PrimaryScreen.Bounds;
                bool alreadyClipped = NativeMethods.GetClipCursor(out clip) &&
                    ((clip.Right - clip.Left) < (screen.Width - 10) || (clip.Bottom - clip.Top) < (screen.Height - 10));

                if (!alreadyClipped)
                {
                    NativeMethods.RECT rect;
                    if (NativeMethods.GetWindowRect(hwnd, out rect))
                    {
                        NativeMethods.SetCursorPos((rect.Left + rect.Right) / 2, (rect.Top + rect.Bottom) / 2);
                        Thread.Sleep(40);
                        NativeMethods.mouse_event(NativeMethods.MOUSEEVENTF_LEFTDOWN, 0, 0, 0, UIntPtr.Zero);
                        Thread.Sleep(20);
                        NativeMethods.mouse_event(NativeMethods.MOUSEEVENTF_LEFTUP, 0, 0, 0, UIntPtr.Zero);
                    }
                }
            }
            catch { }
        }

        private void StartDpReturnWatcher()
        {
            StopDpReturnWatcher();
            var cts = new CancellationTokenSource();
            _watchCts = cts;
            var token = cts.Token;

            Task.Run(async () =>
            {
                try
                {
                    // Short settle after KEYCODE_HOME finishes, then fast polling so instant DP clicks are never missed
                    await Task.Delay(500, token);
                    bool sawNonExternal = false;
                    int consecutiveExternal = 0;

                    while (!token.IsCancellationRequested && IsScrcpyRunning())
                    {
                        var res = await _dev.ShellAsync("dumpsys window | grep -E 'mCurrentFocus|mFocusedApp'", 2500);
                        string txt = res.StdOut ?? "";
                        bool isExt = txt.IndexOf("ExternalSourceActivity", StringComparison.OrdinalIgnoreCase) >= 0;

                        if (!isExt && txt.Length > 0)
                        {
                            sawNonExternal = true;
                            consecutiveExternal = 0;
                        }
                        else if (isExt)
                        {
                            consecutiveExternal++;
                            // Trigger immediately if we already saw TV desktop, or after 2 consecutive external polls (~1.2s)
                            if (sawNonExternal || consecutiveExternal >= 2)
                            {
                                PostUi(() =>
                                {
                                    StopControlSession();
                                    _flyout.SetOptimisticSource(SignalSource.Dp, _cfg.TvAddress, false);
                                    RefreshFlyoutStatusAsync(true);
                                });
                                break;
                            }
                        }

                        await Task.Delay(sawNonExternal ? 650 : 450, token);
                    }
                }
                catch { }
            }, token);
        }

        private void StopDpReturnWatcher()
        {
            var cts = _watchCts;
            _watchCts = null;
            if (cts != null) { try { cts.Cancel(); cts.Dispose(); } catch { } }
        }

        private void OnTvHotkeyPressed(int id)
        {
            if (!IsScrcpyRunning()) return;
            switch (id)
            {
                case HotkeyReturnDpId:
                    OnFlyoutSourceClicked(SignalSource.Dp);
                    break;
                case HotkeyBackId: _dev.SendTvKeyFast(4); break;
                case HotkeyHomeId: _dev.SendTvKeyFast(3); break;
                case HotkeyMenuId: _dev.SendTvKeyFast(82); break;
                case HotkeyVolUpId: _dev.SendTvKeyFast(24); break;
                case HotkeyVolDownId: _dev.SendTvKeyFast(25); break;
                case HotkeyMuteId: _dev.SendTvKeyFast(164); break;
                case HotkeyPowerId: _dev.SendTvKeyFast(26); break;
            }
        }

        private void RegisterTvHotkeys()
        {
            if (_hotkeysRegistered) return;
            IntPtr h = _hotkeyWindow.Handle;
            NativeMethods.RegisterHotKey(h, HotkeyReturnDpId, NativeMethods.MOD_CONTROL | NativeMethods.MOD_ALT | NativeMethods.MOD_NOREPEAT, NativeMethods.VK_D);
            NativeMethods.RegisterHotKey(h, HotkeyBackId, NativeMethods.MOD_NONE | NativeMethods.MOD_NOREPEAT, NativeMethods.VK_ESCAPE);
            NativeMethods.RegisterHotKey(h, HotkeyHomeId, NativeMethods.MOD_NONE | NativeMethods.MOD_NOREPEAT, NativeMethods.VK_HOME);
            // Note: VK_F12 is reserved by Windows RegisterHotKey, handled via WH_KEYBOARD_LL instead
            NativeMethods.RegisterHotKey(h, HotkeyVolUpId, NativeMethods.MOD_NONE, NativeMethods.VK_PRIOR);
            NativeMethods.RegisterHotKey(h, HotkeyVolDownId, NativeMethods.MOD_NONE, NativeMethods.VK_NEXT);
            NativeMethods.RegisterHotKey(h, HotkeyMuteId, NativeMethods.MOD_CONTROL | NativeMethods.MOD_ALT | NativeMethods.MOD_NOREPEAT, NativeMethods.VK_M);
            NativeMethods.RegisterHotKey(h, HotkeyPowerId, NativeMethods.MOD_CONTROL | NativeMethods.MOD_ALT | NativeMethods.MOD_NOREPEAT, NativeMethods.VK_P);
            InstallKeyboardHook();
            _hotkeysRegistered = true;
        }

        private void UnregisterTvHotkeys()
        {
            UninstallKeyboardHook();
            if (!_hotkeysRegistered) return;
            IntPtr h = _hotkeyWindow.Handle;
            NativeMethods.UnregisterHotKey(h, HotkeyReturnDpId);
            NativeMethods.UnregisterHotKey(h, HotkeyBackId);
            NativeMethods.UnregisterHotKey(h, HotkeyHomeId);
            NativeMethods.UnregisterHotKey(h, HotkeyVolUpId);
            NativeMethods.UnregisterHotKey(h, HotkeyVolDownId);
            NativeMethods.UnregisterHotKey(h, HotkeyMuteId);
            NativeMethods.UnregisterHotKey(h, HotkeyPowerId);
            _hotkeysRegistered = false;
        }

        private void InstallKeyboardHook()
        {
            if (_keyHookHandle != IntPtr.Zero) return;
            _f12Down = false;
            using (var curProc = Process.GetCurrentProcess())
            using (var curMod = curProc.MainModule)
            {
                _keyHookHandle = NativeMethods.SetWindowsHookEx(
                    NativeMethods.WH_KEYBOARD_LL,
                    _keyHookProc,
                    NativeMethods.GetModuleHandle(curMod.ModuleName),
                    0);
            }
        }

        private void UninstallKeyboardHook()
        {
            _f12Down = false;
            if (_keyHookHandle == IntPtr.Zero) return;
            NativeMethods.UnhookWindowsHookEx(_keyHookHandle);
            _keyHookHandle = IntPtr.Zero;
        }

        private IntPtr KeyboardHookCallback(int nCode, IntPtr wParam, IntPtr lParam)
        {
            if (nCode >= 0 && IsScrcpyRunning())
            {
                int vkCode = Marshal.ReadInt32(lParam);
                if (vkCode == (int)NativeMethods.VK_F12)
                {
                    int msg = wParam.ToInt32();
                    if (msg == NativeMethods.WM_KEYDOWN || msg == NativeMethods.WM_SYSKEYDOWN)
                    {
                        if (!_f12Down)
                        {
                            _f12Down = true;
                            _dev.SendTvKeyFast(82); // KEYCODE_MENU (Xiaomi remote 3-bars button)
                        }
                        return (IntPtr)1;
                    }
                    if (msg == NativeMethods.WM_KEYUP || msg == NativeMethods.WM_SYSKEYUP)
                    {
                        _f12Down = false;
                        return (IntPtr)1;
                    }
                }
            }
            return NativeMethods.CallNextHookEx(_keyHookHandle, nCode, wParam, lParam);
        }

        private void InstallMouseWheelHook()
        {
            if (_mouseHookHandle != IntPtr.Zero) return;
            using (var curProc = Process.GetCurrentProcess())
            using (var curMod = curProc.MainModule)
            {
                _mouseHookHandle = NativeMethods.SetWindowsHookEx(
                    NativeMethods.WH_MOUSE_LL,
                    _mouseHookProc,
                    NativeMethods.GetModuleHandle(curMod.ModuleName),
                    0);
            }
        }

        private void UninstallMouseWheelHook()
        {
            if (_mouseHookHandle == IntPtr.Zero) return;
            NativeMethods.UnhookWindowsHookEx(_mouseHookHandle);
            _mouseHookHandle = IntPtr.Zero;
        }

        private IntPtr MouseHookCallback(int nCode, IntPtr wParam, IntPtr lParam)
        {
            if (nCode >= 0 && wParam == (IntPtr)NativeMethods.WM_MOUSEWHEEL && IsScrcpyRunning() && _cfg.WheelAsDpad)
            {
                var info = (NativeMethods.MSLLHOOKSTRUCT)Marshal.PtrToStructure(lParam, typeof(NativeMethods.MSLLHOOKSTRUCT));
                short delta = (short)((info.mouseData >> 16) & 0xFFFF);
                if (delta != 0)
                {
                    int now = Environment.TickCount;
                    if (unchecked(now - _lastWheelTick) >= 50)
                    {
                        _lastWheelTick = now;
                        _dev.SendTvKeyFast(delta > 0 ? 19 : 20);
                    }
                    return (IntPtr)1;
                }
            }
            return NativeMethods.CallNextHookEx(_mouseHookHandle, nCode, wParam, lParam);
        }

        private static IntPtr FindEnglishKeyboardLayout()
        {
            try
            {
                int count = NativeMethods.GetKeyboardLayoutList(0, null);
                if (count <= 0) return IntPtr.Zero;
                var list = new IntPtr[count];
                NativeMethods.GetKeyboardLayoutList(count, list);
                foreach (var hkl in list)
                    if (((int)((long)hkl & 0xFFFF)) == 0x0409) return hkl;
            }
            catch { }
            return IntPtr.Zero;
        }

        private void SaveCurrentUserIme()
        {
            if (_hasSavedUserIme) return;
            try
            {
                NativeMethods.TF_INPUTPROCESSORPROFILE prof;
                if (NativeMethods.TryGetActiveTsfProfile(out prof))
                {
                    _savedTsfProfile = prof;
                    IntPtr fg = NativeMethods.GetForegroundWindow();
                    IntPtr imeWnd = fg != IntPtr.Zero ? NativeMethods.ImmGetDefaultIMEWnd(fg) : IntPtr.Zero;
                    _savedUserImeOpen = imeWnd != IntPtr.Zero
                        ? NativeMethods.SendMessage(imeWnd, NativeMethods.WM_IME_CONTROL, (IntPtr)NativeMethods.IMC_GETOPENSTATUS, IntPtr.Zero)
                        : IntPtr.Zero;
                    _hasSavedUserIme = true;
                }
            }
            catch { _hasSavedUserIme = false; }
        }

        private void RestoreSavedUserIme()
        {
            if (!_hasSavedUserIme) return;
            var prof = _savedTsfProfile;
            IntPtr imeOpen = _savedUserImeOpen;
            _hasSavedUserIme = false;

            Action apply = () =>
            {
                try
                {
                    NativeMethods.TryActivateTsfProfile(prof);
                    IntPtr fg = NativeMethods.GetForegroundWindow();
                    if (fg != IntPtr.Zero && imeOpen != IntPtr.Zero)
                    {
                        IntPtr imeWnd = NativeMethods.ImmGetDefaultIMEWnd(fg);
                        if (imeWnd != IntPtr.Zero)
                            NativeMethods.SendMessage(imeWnd, NativeMethods.WM_IME_CONTROL, (IntPtr)NativeMethods.IMC_SETOPENSTATUS, imeOpen);
                    }
                }
                catch { }
            };

            apply();
            Task.Delay(200).ContinueWith(_ => PostUi(apply));
        }

        private void EnforceScrcpyEnglishIme(bool switchLayout)
        {
            try
            {
                var p = _scrcpyProc;
                if (p == null || p.HasExited) return;
                IntPtr hwnd = p.MainWindowHandle;
                if (hwnd == IntPtr.Zero) return;

                if (switchLayout && _englishHkl != IntPtr.Zero)
                {
                    var engProf = new NativeMethods.TF_INPUTPROCESSORPROFILE
                    {
                        dwProfileType = NativeMethods.TF_PROFILETYPE_KEYBOARDLAYOUT,
                        langid = 0x0409,
                        clsid = Guid.Empty,
                        guidProfile = Guid.Empty,
                        hkl = _englishHkl
                    };
                    if (!NativeMethods.TryActivateTsfProfile(engProf))
                    {
                        NativeMethods.PostMessage(hwnd, NativeMethods.WM_INPUTLANGCHANGEREQUEST, IntPtr.Zero, _englishHkl);
                    }
                }

                IntPtr imeWnd = NativeMethods.ImmGetDefaultIMEWnd(hwnd);
                if (imeWnd != IntPtr.Zero)
                {
                    IntPtr st = NativeMethods.SendMessage(imeWnd, NativeMethods.WM_IME_CONTROL, (IntPtr)NativeMethods.IMC_GETOPENSTATUS, IntPtr.Zero);
                    if (st != IntPtr.Zero)
                        NativeMethods.SendMessage(imeWnd, NativeMethods.WM_IME_CONTROL, (IntPtr)NativeMethods.IMC_SETOPENSTATUS, IntPtr.Zero);
                }
            }
            catch { }
        }

        private bool IsScrcpyRunning()
        {
            var p = _scrcpyProc;
            if (p == null) return false;
            try { return !p.HasExited; } catch { return false; }
        }

        private void StopControlSession()
        {
            _imeGuardTimer.Stop();
            UninstallMouseWheelHook();
            StopDpReturnWatcher();
            UnregisterTvHotkeys();
            _dev.StopPersistentShell();

            var p = _scrcpyProc;
            _scrcpyProc = null;
            if (p != null)
            {
                try
                {
                    if (!p.HasExited)
                    {
                        p.CloseMainWindow();
                        if (!p.WaitForExit(600)) p.Kill();
                    }
                    p.Dispose();
                }
                catch { }
            }
            UpdateTakeoverUi(false);
            RestoreSavedUserIme();
        }

        private void OnScrcpyExited()
        {
            _imeGuardTimer.Stop();
            UninstallMouseWheelHook();
            StopDpReturnWatcher();
            UnregisterTvHotkeys();
            _dev.StopPersistentShell();
            _scrcpyProc = null;
            UpdateTakeoverUi(false);
            RestoreSavedUserIme();
        }

        private void UpdateTakeoverUi(bool active)
        {
            _trayIcon.Icon = active ? _activeIcon : _idleIcon;
            _trayIcon.Text = active
                ? "G Pro 27U 键鼠接管中 (Ctrl+Alt+D 切回电脑 DP)"
                : "G Pro 27U 信号源 / 背光 / 键鼠控制";
            _itemToggleControl.Text = active ? "停止键鼠接管" : "仅开启键鼠接管";
        }

        private void ShowTip(string title, string text, ToolTipIcon icon)
        {
            _trayIcon.BalloonTipTitle = title;
            _trayIcon.BalloonTipText = text;
            _trayIcon.BalloonTipIcon = icon;
            _trayIcon.ShowBalloonTip(3000);
        }

        private void PostUi(Action action) { _uiCtx.Post(_ => action(), null); }

        private void ExitApp()
        {
            StopControlSession();
            _imeGuardTimer.Dispose();
            _flyout.Dispose();
            _dev.Dispose();
            _trayIcon.Visible = false;
            _trayIcon.Dispose();
            _idleIcon.Dispose();
            _activeIcon.Dispose();
            _hotkeyWindow.Dispose();
            ExitThread();
        }
    }

    static class Program
    {
        [STAThread]
        static void Main()
        {
            bool createdNew;
            using (var mutex = new Mutex(true, "Local\\MiTVSwitch_GPro27U_SingleInstance", out createdNew))
            {
                if (!createdNew) return;
                NativeMethods.SetProcessDPIAware();
                Application.EnableVisualStyles();
                Application.SetCompatibleTextRenderingDefault(false);
                Application.Run(new TrayAppContext());
            }
        }
    }
}
