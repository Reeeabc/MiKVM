using System;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.RegularExpressions;
using System.Threading.Tasks;
using System.Windows.Forms;
using Microsoft.Win32;

namespace MiTVSwitch
{
    static class NativeMethods
    {
        [DllImport("user32.dll")]
        public static extern bool SetForegroundWindow(IntPtr hWnd);

        [DllImport("user32.dll")]
        public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

        [DllImport("user32.dll")]
        public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);

        [DllImport("user32.dll")]
        public static extern bool GetClipCursor(out RECT lpRect);

        [DllImport("user32.dll")]
        public static extern bool SetCursorPos(int X, int Y);

        [DllImport("user32.dll")]
        public static extern void mouse_event(uint dwFlags, int dx, int dy, uint dwData, UIntPtr dwExtraInfo);

        [DllImport("user32.dll")]
        public static extern bool RegisterHotKey(IntPtr hWnd, int id, uint fsModifiers, uint vk);

        [DllImport("user32.dll")]
        public static extern bool UnregisterHotKey(IntPtr hWnd, int id);

        [DllImport("user32.dll")]
        public static extern bool SetProcessDPIAware();

        [DllImport("imm32.dll")]
        public static extern IntPtr ImmGetDefaultIMEWnd(IntPtr hWnd);

        [DllImport("user32.dll", CharSet = CharSet.Auto)]
        public static extern IntPtr SendMessage(IntPtr hWnd, uint Msg, IntPtr wParam, IntPtr lParam);

        [DllImport("user32.dll", CharSet = CharSet.Auto)]
        public static extern bool PostMessage(IntPtr hWnd, uint Msg, IntPtr wParam, IntPtr lParam);

        [DllImport("user32.dll")]
        public static extern int GetKeyboardLayoutList(int nBuff, [Out] IntPtr[] lpList);

        [DllImport("user32.dll")]
        public static extern IntPtr GetForegroundWindow();

        [DllImport("user32.dll")]
        public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId);

        [DllImport("user32.dll")]
        public static extern IntPtr GetKeyboardLayout(uint idThread);

        [DllImport("user32.dll")]
        public static extern IntPtr ActivateKeyboardLayout(IntPtr hkl, uint flags);

        public const uint KLF_SETFORPROCESS = 0x00000100;
        public const uint TF_PROFILETYPE_KEYBOARDLAYOUT = 0x0002;
        public const uint TF_IPPMF_FORSESSION = 0x20000000;
        public const uint TF_IPPMF_DONTCARECURRENTINPUTLANGUAGE = 0x00000004;

        [StructLayout(LayoutKind.Sequential)]
        public struct TF_INPUTPROCESSORPROFILE
        {
            public uint dwProfileType;
            public ushort langid;
            public Guid clsid;
            public Guid guidProfile;
            public Guid catid;
            public IntPtr hklSubstitute;
            public uint dwCaps;
            public IntPtr hkl;
            public uint dwFlags;
        }

        [ComImport, Guid("71C6E74C-0F28-11D8-A82A-00065B84435C"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        public interface ITfInputProcessorProfileMgr
        {
            [PreserveSig] int ActivateProfile(uint dwProfileType, ushort langid, ref Guid clsid, ref Guid guidProfile, IntPtr hkl, uint dwFlags);
            [PreserveSig] int DeactivateProfile(uint dwProfileType, ushort langid, ref Guid clsid, ref Guid guidProfile, IntPtr hkl, uint dwFlags);
            [PreserveSig] int GetProfile(uint dwProfileType, ushort langid, ref Guid clsid, ref Guid guidProfile, IntPtr hkl, out TF_INPUTPROCESSORPROFILE pProfile);
            [PreserveSig] int EnumProfiles(ushort langid, out IntPtr ppEnum);
            [PreserveSig] int ReleaseInputProcessor(ref Guid rclsid, uint dwFlags);
            [PreserveSig] int RegisterProfile(ref Guid rclsid, ushort langid, ref Guid guidProfile, [MarshalAs(UnmanagedType.LPWStr)] string pchDesc, uint cchDesc, [MarshalAs(UnmanagedType.LPWStr)] string pchIconFile, uint cchFile, uint uIconIndex, IntPtr hklsubstitute, uint dwPreferredLayout, bool bEnabledByDefault, uint dwFlags);
            [PreserveSig] int UnregisterProfile(ref Guid rclsid, ushort langid, ref Guid guidProfile, uint dwFlags);
            [PreserveSig] int GetActiveProfile(ref Guid catid, out TF_INPUTPROCESSORPROFILE pProfile);
        }

        [DllImport("msctf.dll")]
        public static extern int TF_CreateInputProcessorProfiles(out IntPtr ppipr);

        public static readonly Guid GUID_TFCAT_TIP_KEYBOARD = new Guid("34745C63-B2F0-4784-8B67-5E12C8701A31");

        public static bool TryGetActiveTsfProfile(out TF_INPUTPROCESSORPROFILE profile)
        {
            profile = new TF_INPUTPROCESSORPROFILE();
            IntPtr p = IntPtr.Zero;
            try
            {
                if (TF_CreateInputProcessorProfiles(out p) != 0 || p == IntPtr.Zero) return false;
                var mgr = (ITfInputProcessorProfileMgr)Marshal.GetObjectForIUnknown(p);
                try
                {
                    Guid cat = GUID_TFCAT_TIP_KEYBOARD;
                    return mgr.GetActiveProfile(ref cat, out profile) == 0;
                }
                finally { Marshal.ReleaseComObject(mgr); }
            }
            catch { return false; }
            finally { if (p != IntPtr.Zero) Marshal.Release(p); }
        }

        public static bool TryActivateTsfProfile(TF_INPUTPROCESSORPROFILE profile)
        {
            IntPtr p = IntPtr.Zero;
            try
            {
                if (TF_CreateInputProcessorProfiles(out p) != 0 || p == IntPtr.Zero) return false;
                var mgr = (ITfInputProcessorProfileMgr)Marshal.GetObjectForIUnknown(p);
                try
                {
                    Guid clsid = profile.clsid;
                    Guid guid = profile.guidProfile;
                    return mgr.ActivateProfile(
                        profile.dwProfileType,
                        profile.langid,
                        ref clsid,
                        ref guid,
                        profile.hkl,
                        TF_IPPMF_FORSESSION | TF_IPPMF_DONTCARECURRENTINPUTLANGUAGE) == 0;
                }
                finally { Marshal.ReleaseComObject(mgr); }
            }
            catch { return false; }
            finally { if (p != IntPtr.Zero) Marshal.Release(p); }
        }

        [DllImport("dwmapi.dll")]
        public static extern int DwmSetWindowAttribute(IntPtr hwnd, int attr, ref int attrValue, int attrSize);

        [DllImport("user32.dll", EntryPoint = "GetWindowLongPtr")]
        public static extern IntPtr GetWindowLongPtr(IntPtr hWnd, int nIndex);

        [DllImport("user32.dll", EntryPoint = "SetWindowLongPtr")]
        public static extern IntPtr SetWindowLongPtr(IntPtr hWnd, int nIndex, IntPtr dwNewLong);

        [DllImport("user32.dll")]
        public static extern bool SetLayeredWindowAttributes(IntPtr hwnd, uint crKey, byte bAlpha, uint dwFlags);

        public const int GWL_EXSTYLE = -20;
        public const long WS_EX_TOOLWINDOW = 0x00000080L;
        public const long WS_EX_APPWINDOW = 0x00040000L;
        public const long WS_EX_LAYERED = 0x00080000L;
        public const uint LWA_ALPHA = 0x00000002;

        public delegate IntPtr LowLevelMouseProc(int nCode, IntPtr wParam, IntPtr lParam);
        public delegate IntPtr LowLevelKeyboardProc(int nCode, IntPtr wParam, IntPtr lParam);

        [DllImport("user32.dll", CharSet = CharSet.Auto, SetLastError = true)]
        public static extern IntPtr SetWindowsHookEx(int idHook, LowLevelMouseProc lpfn, IntPtr hMod, uint dwThreadId);

        [DllImport("user32.dll", CharSet = CharSet.Auto, SetLastError = true)]
        public static extern IntPtr SetWindowsHookEx(int idHook, LowLevelKeyboardProc lpfn, IntPtr hMod, uint dwThreadId);

        [DllImport("user32.dll", CharSet = CharSet.Auto, SetLastError = true)]
        public static extern bool UnhookWindowsHookEx(IntPtr hhk);

        [DllImport("user32.dll", CharSet = CharSet.Auto, SetLastError = true)]
        public static extern IntPtr CallNextHookEx(IntPtr hhk, int nCode, IntPtr wParam, IntPtr lParam);

        [DllImport("kernel32.dll", CharSet = CharSet.Auto, SetLastError = true)]
        public static extern IntPtr GetModuleHandle(string lpModuleName);

        public const int SW_RESTORE = 9;
        public const uint MOUSEEVENTF_LEFTDOWN = 0x0002;
        public const uint MOUSEEVENTF_LEFTUP = 0x0004;
        public const uint MOD_NONE = 0x0000;
        public const uint MOD_ALT = 0x0001;
        public const uint MOD_CONTROL = 0x0002;
        public const uint MOD_NOREPEAT = 0x4000;
        public const uint VK_ESCAPE = 0x1B;
        public const uint VK_PRIOR = 0x21;
        public const uint VK_NEXT = 0x22;
        public const uint VK_HOME = 0x24;
        public const uint VK_D = 0x44;
        public const uint VK_M = 0x4D;
        public const uint VK_P = 0x50;
        public const uint VK_F12 = 0x7B;
        public const int WM_HOTKEY = 0x0312;
        public const int WH_KEYBOARD_LL = 13;
        public const int WH_MOUSE_LL = 14;
        public const int WM_KEYDOWN = 0x0100;
        public const int WM_KEYUP = 0x0101;
        public const int WM_SYSKEYDOWN = 0x0104;
        public const int WM_SYSKEYUP = 0x0105;
        public const int WM_MOUSEWHEEL = 0x020A;
        public const uint WM_INPUTLANGCHANGEREQUEST = 0x0050;
        public const uint WM_IME_CONTROL = 0x0283;
        public const int IMC_GETOPENSTATUS = 0x0005;
        public const int IMC_SETOPENSTATUS = 0x0006;

        public const int DWMWA_WINDOW_CORNER_PREFERENCE = 33;
        public const int DWMWCP_ROUND = 2;

        [StructLayout(LayoutKind.Sequential)]
        public struct RECT { public int Left, Top, Right, Bottom; }

        [StructLayout(LayoutKind.Sequential)]
        public struct POINT { public int x, y; }

        [StructLayout(LayoutKind.Sequential)]
        public struct MSLLHOOKSTRUCT
        {
            public POINT pt;
            public uint mouseData;
            public uint flags;
            public uint time;
            public IntPtr dwExtraInfo;
        }
    }

    sealed class HotkeyMessageWindow : NativeWindow, IDisposable
    {
        public event Action<int> HotkeyPressed;
        public HotkeyMessageWindow() { CreateHandle(new CreateParams()); }

        protected override void WndProc(ref Message m)
        {
            if (m.Msg == NativeMethods.WM_HOTKEY)
            {
                var h = HotkeyPressed;
                if (h != null) h(m.WParam.ToInt32());
            }
            base.WndProc(ref m);
        }

        public void Dispose() { DestroyHandle(); }
    }

    enum SignalSource { Unknown, TvSystem, Dp, Hdmi1, Hdmi2 }

    sealed class DeviceStatus
    {
        public bool Connected;
        public SignalSource ActiveSource = SignalSource.Unknown;
        public int? Backlight;
        public int? Volume;
    }

    sealed class AppConfig
    {
        private const string RunRegPath = @"Software\Microsoft\Windows\CurrentVersion\Run";
        private const string RunRegName = "MiTVSwitch";
        private readonly string _path;

        public string TvAddress = "192.168.1.200:5555";
        public bool AutoTakeoverOnTv = true;
        public bool WheelAsDpad = true;
        public int DpInput = 29;
        public int Hdmi1Input = 23;
        public int Hdmi2Input = 24;

        public AppConfig(string path) { _path = path; Load(); }

        public void Load()
        {
            if (!File.Exists(_path)) { Save(); return; }
            try
            {
                foreach (var raw in File.ReadAllLines(_path, Encoding.UTF8))
                {
                    var line = raw.Trim();
                    if (line.Length == 0 || line.StartsWith("#")) continue;
                    int i = line.IndexOf('=');
                    if (i <= 0) continue;
                    string k = line.Substring(0, i).Trim();
                    string v = line.Substring(i + 1).Trim();
                    if (k.Equals("TvAddress", StringComparison.OrdinalIgnoreCase) && v.Length > 0) TvAddress = v;
                    else if (k.Equals("AutoTakeoverOnTv", StringComparison.OrdinalIgnoreCase)) bool.TryParse(v, out AutoTakeoverOnTv);
                    else if (k.Equals("WheelAsDpad", StringComparison.OrdinalIgnoreCase)) bool.TryParse(v, out WheelAsDpad);
                    else if (k.Equals("DpInput", StringComparison.OrdinalIgnoreCase)) int.TryParse(v, out DpInput);
                    else if (k.Equals("Hdmi1Input", StringComparison.OrdinalIgnoreCase)) int.TryParse(v, out Hdmi1Input);
                    else if (k.Equals("Hdmi2Input", StringComparison.OrdinalIgnoreCase)) int.TryParse(v, out Hdmi2Input);
                }
            }
            catch { }
        }

        public void Save()
        {
            try
            {
                var sb = new StringBuilder();
                sb.AppendLine("# G Pro 27U 融合切换工具配置 (键盘固定 SDK 模式)");
                sb.AppendLine("TvAddress=" + TvAddress);
                sb.AppendLine("AutoTakeoverOnTv=" + AutoTakeoverOnTv);
                sb.AppendLine("WheelAsDpad=" + WheelAsDpad);
                sb.AppendLine("DpInput=" + DpInput);
                sb.AppendLine("Hdmi1Input=" + Hdmi1Input);
                sb.AppendLine("Hdmi2Input=" + Hdmi2Input);
                File.WriteAllText(_path, sb.ToString(), Encoding.UTF8);
            }
            catch { }
        }

        public static bool IsAutoStartEnabled()
        {
            try
            {
                using (var key = Registry.CurrentUser.OpenSubKey(RunRegPath, false))
                {
                    var val = key != null ? key.GetValue(RunRegName) as string : null;
                    return !string.IsNullOrEmpty(val);
                }
            }
            catch { return false; }
        }

        public static void SetAutoStart(bool enable)
        {
            try
            {
                using (var key = Registry.CurrentUser.OpenSubKey(RunRegPath, true))
                {
                    if (key == null) return;
                    if (enable)
                    {
                        string exe = Process.GetCurrentProcess().MainModule.FileName;
                        key.SetValue(RunRegName, "\"" + exe + "\"");
                    }
                    else
                    {
                        key.DeleteValue(RunRegName, false);
                    }
                }
            }
            catch { }
        }
    }

    sealed class CmdResult
    {
        public int ExitCode;
        public string StdOut = "";
        public string StdErr = "";
    }

    sealed class DeviceController : IDisposable
    {
        private readonly string _adbPath;
        private readonly string _assetsDir;
        private readonly AppConfig _cfg;
        private readonly object _shellLock = new object();
        private Process _shellProc;
        private StreamWriter _shellIn;
        private bool _jarsVerified;

        public DeviceController(string adbPath, string assetsDir, AppConfig cfg)
        {
            _adbPath = adbPath;
            _assetsDir = assetsDir;
            _cfg = cfg;
        }

        public string AdbPath { get { return _adbPath; } }
        public void ResetJarCache() { _jarsVerified = false; }

        public async Task<bool> EnsureConnectedAsync(int timeoutMs = 3500)
        {
            if (!File.Exists(_adbPath)) return false;
            var res = await RunAdbAsync("connect " + _cfg.TvAddress, timeoutMs);
            string combined = (res.StdOut + " " + res.StdErr).ToLowerInvariant();
            if (combined.Contains("connected to") || combined.Contains("already connected"))
            {
                var st = await RunAdbAsync("-s " + _cfg.TvAddress + " get-state", 2500);
                return st.StdOut.Trim() == "device";
            }
            return false;
        }

        public async Task EnsureJarsDeployedAsync()
        {
            if (_jarsVerified) return;
            var chk = await ShellAsync("test -f /data/local/tmp/MtkDirectTool.jar && test -f /data/local/tmp/VolDirectTool.jar && echo OK", 2500);
            if (chk.StdOut.Trim() == "OK")
            {
                _jarsVerified = true;
                return;
            }
            string mtk = Path.Combine(_assetsDir, "MtkDirectTool.jar");
            string vol = Path.Combine(_assetsDir, "VolDirectTool.jar");
            if (File.Exists(mtk)) await RunAdbAsync("-s " + _cfg.TvAddress + " push \"" + mtk + "\" /data/local/tmp/MtkDirectTool.jar", 6000);
            if (File.Exists(vol)) await RunAdbAsync("-s " + _cfg.TvAddress + " push \"" + vol + "\" /data/local/tmp/VolDirectTool.jar", 6000);
            await ShellAsync("chmod 644 /data/local/tmp/MtkDirectTool.jar /data/local/tmp/VolDirectTool.jar 2>/dev/null", 2500);
            _jarsVerified = true;
        }

        public async Task<DeviceStatus> QueryStatusAsync()
        {
            var status = new DeviceStatus();
            if (!await EnsureConnectedAsync()) return status;
            status.Connected = true;

            await EnsureJarsDeployedAsync();

            string cmd =
                "echo SRC=$(settings get global mitv.tvplayer.hdmi.last.source); " +
                "echo FOC=$(dumpsys window | grep -E 'mCurrentFocus|mFocusedApp' | head -n 2 | tr '\\n' ' '); " +
                "echo BL=$(su 1000 -c 'CLASSPATH=/data/local/tmp/MtkDirectTool.jar /system/bin/app_process /data/local/tmp MtkDirectTool get g_disp__disp_back_light' 2>/dev/null | grep -oE '[0-9]+$' | tail -n 1); " +
                "echo VOL=$(cmd media_session volume --stream 3 --get 2>/dev/null | grep -oE 'volume is [0-9]+' | grep -oE '[0-9]+')";

            var res = await ShellAsync(cmd, 4500);
            string text = res.StdOut ?? "";

            bool isExt = false;
            int lastSrc = -1;
            foreach (var rawLine in text.Split('\n'))
            {
                string line = rawLine.Trim();
                if (line.StartsWith("SRC="))
                {
                    int.TryParse(line.Substring(4).Trim(), out lastSrc);
                }
                else if (line.StartsWith("FOC="))
                {
                    isExt = line.IndexOf("ExternalSourceActivity", StringComparison.OrdinalIgnoreCase) >= 0;
                }
                else if (line.StartsWith("BL="))
                {
                    int bl;
                    if (int.TryParse(line.Substring(3).Trim(), out bl) && bl >= 0 && bl <= 100)
                        status.Backlight = bl;
                }
                else if (line.StartsWith("VOL="))
                {
                    int vol;
                    if (int.TryParse(line.Substring(4).Trim(), out vol) && vol >= 0 && vol <= 100)
                        status.Volume = vol;
                }
            }

            if (!isExt)
            {
                status.ActiveSource = SignalSource.TvSystem;
            }
            else if (lastSrc == _cfg.DpInput) status.ActiveSource = SignalSource.Dp;
            else if (lastSrc == _cfg.Hdmi1Input) status.ActiveSource = SignalSource.Hdmi1;
            else if (lastSrc == _cfg.Hdmi2Input) status.ActiveSource = SignalSource.Hdmi2;
            else status.ActiveSource = SignalSource.Dp;

            return status;
        }

        public async Task<bool> SwitchToTvDesktopAsync()
        {
            if (!await EnsureConnectedAsync()) return false;
            await ShellAsync("cmd input keyevent 3", 3500);
            return true;
        }

        public async Task<bool> SwitchToExternalAsync(int inputId)
        {
            if (!await EnsureConnectedAsync()) return false;
            string cmd = string.Format(
                "am start -a com.xiaomi.mitv.tvplayer.EXTSRC_PLAY -n com.xiaomi.mitv.tvplayer/.ExternalSourceActivity --ei input {0} -f 0x10000000",
                inputId);
            await ShellAsync(cmd, 3500);
            return true;
        }

        public async Task<bool> SetBacklightAsync(int value)
        {
            value = Math.Max(0, Math.Min(100, value));
            if (!await EnsureConnectedAsync()) return false;
            await EnsureJarsDeployedAsync();
            string cmd = string.Format(
                "su 1000 -c 'CLASSPATH=/data/local/tmp/MtkDirectTool.jar /system/bin/app_process /data/local/tmp MtkDirectTool set g_disp__disp_back_light {0} 3' >/dev/null 2>&1; " +
                "am broadcast -a com.xiaomi.mitv.action.PIC_MODE_CHANGED --ei picmode 7 >/dev/null 2>&1; " +
                "settings put global picture_backlight {0}; settings put global xiaomi_picture_backlight {0}",
                value);
            var r = await ShellAsync(cmd, 4000);
            return r.ExitCode == 0;
        }

        public async Task<bool> SetVolumeAsync(int value)
        {
            value = Math.Max(0, Math.Min(100, value));
            if (!await EnsureConnectedAsync()) return false;
            await EnsureJarsDeployedAsync();
            string cmd = string.Format(
                "su 1000 -c 'CLASSPATH=/data/local/tmp/VolDirectTool.jar /system/bin/app_process /data/local/tmp VolDirectTool set 3 {0}' >/dev/null 2>&1",
                value);
            var r = await ShellAsync(cmd, 3500);
            return r.ExitCode == 0;
        }

        public void StartPersistentShell()
        {
            lock (_shellLock)
            {
                if (_shellProc != null && !_shellProc.HasExited && _shellIn != null) return;
                StopPersistentShellLocked();
                try
                {
                    var psi = new ProcessStartInfo
                    {
                        FileName = _adbPath,
                        Arguments = "-s " + _cfg.TvAddress + " shell",
                        UseShellExecute = false,
                        CreateNoWindow = true,
                        RedirectStandardInput = true,
                        RedirectStandardOutput = true,
                        RedirectStandardError = true
                    };
                    var p = Process.Start(psi);
                    p.BeginOutputReadLine();
                    p.BeginErrorReadLine();
                    _shellProc = p;
                    _shellIn = p.StandardInput;
                }
                catch { }
            }
        }

        public void StopPersistentShell()
        {
            lock (_shellLock) { StopPersistentShellLocked(); }
        }

        private void StopPersistentShellLocked()
        {
            try { if (_shellIn != null) _shellIn.Dispose(); } catch { }
            _shellIn = null;
            try
            {
                if (_shellProc != null)
                {
                    if (!_shellProc.HasExited) _shellProc.Kill();
                    _shellProc.Dispose();
                }
            }
            catch { }
            _shellProc = null;
        }

        public void SendTvKeyFast(int keyCode)
        {
            Task.Run(() =>
            {
                lock (_shellLock)
                {
                    try
                    {
                        if (_shellProc == null || _shellProc.HasExited || _shellIn == null)
                            StartPersistentShell();
                        if (_shellIn != null)
                        {
                            _shellIn.WriteLine("cmd input keyevent " + keyCode);
                            _shellIn.Flush();
                            return;
                        }
                    }
                    catch { StopPersistentShellLocked(); }
                }
                ShellAsync("cmd input keyevent " + keyCode, 2000);
            });
        }

        public Task<CmdResult> ShellAsync(string shellCmd, int timeoutMs = 4000)
        {
            string escaped = shellCmd.Replace("\"", "\\\"");
            return RunAdbAsync("-s " + _cfg.TvAddress + " shell \"" + escaped + "\"", timeoutMs);
        }

        public Task<CmdResult> RunAdbAsync(string args, int timeoutMs)
        {
            return Task.Run(() =>
            {
                try
                {
                    var psi = new ProcessStartInfo
                    {
                        FileName = _adbPath,
                        Arguments = args,
                        UseShellExecute = false,
                        CreateNoWindow = true,
                        RedirectStandardOutput = true,
                        RedirectStandardError = true,
                        StandardOutputEncoding = Encoding.UTF8,
                        StandardErrorEncoding = Encoding.UTF8
                    };
                    using (var p = Process.Start(psi))
                    {
                        var outTask = p.StandardOutput.ReadToEndAsync();
                        var errTask = p.StandardError.ReadToEndAsync();
                        if (!p.WaitForExit(timeoutMs))
                        {
                            try { p.Kill(); } catch { }
                            return new CmdResult { ExitCode = -1, StdErr = "Timeout" };
                        }
                        return new CmdResult { ExitCode = p.ExitCode, StdOut = outTask.Result, StdErr = errTask.Result };
                    }
                }
                catch (Exception ex)
                {
                    return new CmdResult { ExitCode = -1, StdErr = ex.Message };
                }
            });
        }

        public void Dispose() { StopPersistentShell(); }
    }
}
