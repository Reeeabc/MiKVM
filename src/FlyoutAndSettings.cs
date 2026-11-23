using System;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Text;
using System.Windows.Forms;

namespace MiTVSwitch
{
    sealed class FlyoutForm : Form
    {
        private readonly float _s;
        private readonly FluentSourceTile _tileTv;
        private readonly FluentSourceTile _tileDp;
        private readonly FluentSourceTile _tileHdmi1;
        private readonly FluentSourceTile _tileHdmi2;
        private readonly FluentSlider _sliderBacklight;
        private readonly FluentSlider _sliderVolume;
        private readonly Label _lblBacklightVal;
        private readonly Label _lblVolumeVal;
        private readonly FluentButton _btnSettings;
        private readonly FluentButton _btnRefresh;
        private readonly FluentButton _btnExit;

        private readonly Timer _blDebounce;
        private readonly Timer _volDebounce;

        private bool _connected;
        private bool _refreshing;
        private bool _takeoverActive;
        private string _statusText = "正在连接...";
        private SignalSource _activeSource = SignalSource.Unknown;
        private bool _suppressDeactivate;
        private DateTime _lastHiddenUtc = DateTime.MinValue;

        public event Action<SignalSource> SourceClicked;
        public event Action<int> BacklightChanged;
        public event Action<int> VolumeChanged;
        public event Action SettingsClicked;
        public event Action RefreshClicked;
        public event Action ExitClicked;

        public DateTime LastHiddenUtc { get { return _lastHiddenUtc; } }

        public FlyoutForm()
        {
            FormBorderStyle = FormBorderStyle.None;
            StartPosition = FormStartPosition.Manual;
            ShowInTaskbar = false;
            TopMost = true;
            BackColor = FluentGraphics.SurfaceBg;
            DoubleBuffered = true;
            KeyPreview = true;

            using (var g = CreateGraphics())
                _s = g.DpiX / 96f;

            int W = S(376);
            int H = S(422);
            ClientSize = new Size(W, H);

            int pad = S(20);
            int innerW = W - pad * 2;
            int colGap = S(14);
            int tileW = (innerW - colGap) / 2;
            int tileH = S(78);

            // Signal source tiles (2x2)
            int gridTop = S(84);
            _tileTv = CreateTile(TileIconKind.Tv, "电视系统", pad, gridTop, tileW, tileH, SignalSource.TvSystem);
            _tileDp = CreateTile(TileIconKind.Monitor, "电脑 DP", pad + tileW + colGap, gridTop, tileW, tileH, SignalSource.Dp);
            _tileHdmi1 = CreateTile(TileIconKind.Hdmi, "HDMI 1", pad, gridTop + tileH + S(12), tileW, tileH, SignalSource.Hdmi1);
            _tileHdmi2 = CreateTile(TileIconKind.Hdmi, "HDMI 2", pad + tileW + colGap, gridTop + tileH + S(12), tileW, tileH, SignalSource.Hdmi2);

            Controls.AddRange(new Control[] { _tileTv, _tileDp, _tileHdmi1, _tileHdmi2 });

            // Backlight slider row
            int blSliderY = S(280);
            int iconW = S(26);
            int valW = S(34);
            int sliderX = pad + iconW + S(6);
            int sliderW = innerW - iconW - valW - S(12);

            _sliderBacklight = new FluentSlider
            {
                DpiScale = _s,
                Left = sliderX,
                Top = blSliderY,
                Width = sliderW,
                Height = S(26),
                Value = 50
            };
            _lblBacklightVal = new Label
            {
                Text = "--",
                Left = W - pad - valW,
                Top = blSliderY + S(2),
                Width = valW,
                Height = S(22),
                TextAlign = ContentAlignment.MiddleRight,
                ForeColor = FluentGraphics.TextSecondary,
                BackColor = FluentGraphics.SurfaceBg,
                Font = new Font("Microsoft YaHei UI", 9.5f)
            };

            // Volume slider row
            int volSliderY = S(334);
            _sliderVolume = new FluentSlider
            {
                DpiScale = _s,
                Left = sliderX,
                Top = volSliderY,
                Width = sliderW,
                Height = S(26),
                Value = 15
            };
            _lblVolumeVal = new Label
            {
                Text = "--",
                Left = W - pad - valW,
                Top = volSliderY + S(2),
                Width = valW,
                Height = S(22),
                TextAlign = ContentAlignment.MiddleRight,
                ForeColor = FluentGraphics.TextSecondary,
                BackColor = FluentGraphics.SurfaceBg,
                Font = new Font("Microsoft YaHei UI", 9.5f)
            };

            Controls.AddRange(new Control[] { _sliderBacklight, _lblBacklightVal, _sliderVolume, _lblVolumeVal });

            // Footer bar
            int footerH = S(52);
            int footerTop = H - footerH;
            var footer = new Panel
            {
                Left = 1,
                Top = footerTop,
                Width = W - 2,
                Height = footerH - 1,
                BackColor = FluentGraphics.FooterBg
            };
            footer.Paint += (s, e) =>
            {
                using (var pen = new Pen(FluentGraphics.DividerColor, 1f))
                    e.Graphics.DrawLine(pen, 0, 0, footer.Width, 0);
            };

            int btnY = S(10);
            int btnH = S(31);
            _btnSettings = new FluentButton { Text = "ADB 设置...", DpiScale = _s, Left = pad - 1, Top = btnY, Width = S(92), Height = btnH };
            _btnRefresh = new FluentButton { Text = "刷新", DpiScale = _s, Left = pad - 1 + S(100), Top = btnY, Width = S(62), Height = btnH };
            _btnExit = new FluentButton { Text = "退出", DpiScale = _s, Left = footer.Width - pad - S(62), Top = btnY, Width = S(62), Height = btnH };

            _btnSettings.Click += (s, e) => { HideFlyout(); var h = SettingsClicked; if (h != null) h(); };
            _btnRefresh.Click += (s, e) => { var h = RefreshClicked; if (h != null) h(); };
            _btnExit.Click += (s, e) => { HideFlyout(); var h = ExitClicked; if (h != null) h(); };

            footer.Controls.AddRange(new Control[] { _btnSettings, _btnRefresh, _btnExit });
            Controls.Add(footer);

            // Debounce timers for smooth dragging without ADB flood
            _blDebounce = new Timer { Interval = 160 };
            _blDebounce.Tick += (s, e) =>
            {
                _blDebounce.Stop();
                var h = BacklightChanged;
                if (h != null) h(_sliderBacklight.Value);
            };

            _volDebounce = new Timer { Interval = 140 };
            _volDebounce.Tick += (s, e) =>
            {
                _volDebounce.Stop();
                var h = VolumeChanged;
                if (h != null) h(_sliderVolume.Value);
            };

            _sliderBacklight.ValueChanged += (s, e) =>
            {
                _lblBacklightVal.Text = _sliderBacklight.Value.ToString();
                _blDebounce.Stop();
                _blDebounce.Start();
            };
            _sliderBacklight.ValueCommitted += (s, e) =>
            {
                _blDebounce.Stop();
                _lblBacklightVal.Text = _sliderBacklight.Value.ToString();
                var h = BacklightChanged;
                if (h != null) h(_sliderBacklight.Value);
            };

            _sliderVolume.ValueChanged += (s, e) =>
            {
                _lblVolumeVal.Text = _sliderVolume.Value.ToString();
                _volDebounce.Stop();
                _volDebounce.Start();
            };
            _sliderVolume.ValueCommitted += (s, e) =>
            {
                _volDebounce.Stop();
                _lblVolumeVal.Text = _sliderVolume.Value.ToString();
                var h = VolumeChanged;
                if (h != null) h(_sliderVolume.Value);
            };

            KeyDown += (s, e) => { if (e.KeyCode == Keys.Escape) HideFlyout(); };
            Deactivate += (s, e) => { if (!_suppressDeactivate) HideFlyout(); };
        }

        private FluentSourceTile CreateTile(TileIconKind kind, string caption, int x, int y, int w, int h, SignalSource src)
        {
            var t = new FluentSourceTile { IconKind = kind, Caption = caption, DpiScale = _s, Left = x, Top = y, Width = w, Height = h };
            t.Click += (s, e) =>
            {
                var handler = SourceClicked;
                if (handler != null) handler(src);
            };
            return t;
        }

        private int S(int px) { return (int)Math.Round(px * _s); }

        protected override void OnHandleCreated(EventArgs e)
        {
            base.OnHandleCreated(e);
            try
            {
                int pref = NativeMethods.DWMWCP_ROUND;
                NativeMethods.DwmSetWindowAttribute(Handle, NativeMethods.DWMWA_WINDOW_CORNER_PREFERENCE, ref pref, sizeof(int));
            }
            catch { }
        }

        protected override CreateParams CreateParams
        {
            get
            {
                var cp = base.CreateParams;
                cp.ClassStyle |= 0x00020000; // CS_DROPSHADOW fallback
                return cp;
            }
        }

        public void ShowNearTray()
        {
            var wa = Screen.PrimaryScreen.WorkingArea;
            int margin = S(12);
            Left = wa.Right - Width - margin;
            Top = wa.Bottom - Height - margin;
            _suppressDeactivate = true;
            Show();
            NativeMethods.SetForegroundWindow(Handle);
            Activate();
            _suppressDeactivate = false;
        }

        public void HideFlyout()
        {
            if (Visible)
            {
                Hide();
                _lastHiddenUtc = DateTime.UtcNow;
            }
        }

        public void SetRefreshing(string address, bool silentIfConnected = false)
        {
            if (silentIfConnected && _connected) return;
            _refreshing = true;
            _statusText = "正在刷新 " + address + "...";
            Invalidate();
        }

        public void UpdateStatus(DeviceStatus st, string address, bool takeoverActive)
        {
            _refreshing = false;
            _connected = st.Connected;
            _takeoverActive = takeoverActive;
            _activeSource = st.ActiveSource;

            if (!st.Connected)
            {
                _statusText = "未连接 " + address;
            }
            else
            {
                string srcName = GetSourceName(st.ActiveSource);
                _statusText = takeoverActive
                    ? "键鼠接管中 · 当前：" + srcName
                    : "已连接 " + address + " · 当前：" + srcName;
            }

            _tileTv.Active = st.Connected && st.ActiveSource == SignalSource.TvSystem;
            _tileDp.Active = st.Connected && st.ActiveSource == SignalSource.Dp;
            _tileHdmi1.Active = st.Connected && st.ActiveSource == SignalSource.Hdmi1;
            _tileHdmi2.Active = st.Connected && st.ActiveSource == SignalSource.Hdmi2;

            if (st.Backlight.HasValue && !_sliderBacklight.IsDragging)
            {
                _sliderBacklight.Value = st.Backlight.Value;
                _lblBacklightVal.Text = st.Backlight.Value.ToString();
            }
            if (st.Volume.HasValue && !_sliderVolume.IsDragging)
            {
                _sliderVolume.Value = st.Volume.Value;
                _lblVolumeVal.Text = st.Volume.Value.ToString();
            }
            Invalidate();
        }

        public void SetOptimisticSource(SignalSource src, string address, bool takeoverActive)
        {
            _connected = true;
            _takeoverActive = takeoverActive;
            _activeSource = src;
            string srcName = GetSourceName(src);
            _statusText = takeoverActive
                ? "键鼠接管中 · 当前：" + srcName
                : "已连接 " + address + " · 当前：" + srcName;

            _tileTv.Active = src == SignalSource.TvSystem;
            _tileDp.Active = src == SignalSource.Dp;
            _tileHdmi1.Active = src == SignalSource.Hdmi1;
            _tileHdmi2.Active = src == SignalSource.Hdmi2;
            Invalidate();
        }

        private static string GetSourceName(SignalSource s)
        {
            switch (s)
            {
                case SignalSource.TvSystem: return "电视系统";
                case SignalSource.Dp: return "电脑 DP";
                case SignalSource.Hdmi1: return "HDMI 1";
                case SignalSource.Hdmi2: return "HDMI 2";
                default: return "未知";
            }
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            var g = e.Graphics;
            g.SmoothingMode = SmoothingMode.AntiAlias;
            g.TextRenderingHint = TextRenderingHint.ClearTypeGridFit;

            int pad = S(20);

            // Outer subtle 1px border
            using (var pen = new Pen(FluentGraphics.CardBorder, 1f))
                g.DrawRectangle(pen, 0, 0, Width - 1, Height - 1);

            // Status dot & text
            Color dotColor = _refreshing
                ? FluentGraphics.StatusAmber
                : (_connected ? FluentGraphics.StatusGreen : FluentGraphics.StatusRed);
            float dotSize = 8.5f * _s;
            float statusCenterY = 26f * _s;
            using (var br = new SolidBrush(dotColor))
                g.FillEllipse(br, pad, statusCenterY - dotSize / 2f, dotSize, dotSize);

            using (var f = new Font("Microsoft YaHei UI", 9.0f))
            using (var br = new SolidBrush(FluentGraphics.TextPrimary))
            using (var sf = new StringFormat { LineAlignment = StringAlignment.Center, Trimming = StringTrimming.EllipsisCharacter, FormatFlags = StringFormatFlags.NoWrap })
            {
                var r = new RectangleF(pad + dotSize + 8f * _s, 10f * _s, Width - pad * 2 - dotSize - 8f * _s, 32f * _s);
                g.DrawString(_statusText, f, br, r, sf);
            }

            // Divider under status bar
            using (var pen = new Pen(FluentGraphics.DividerColor, 1f))
                g.DrawLine(pen, pad, S(48), Width - pad, S(48));

            // Section labels
            using (var f = new Font("Microsoft YaHei UI", 9.0f))
            using (var br = new SolidBrush(FluentGraphics.TextSecondary))
            {
                g.DrawString("信号源", f, br, pad, S(58));
                g.DrawString("背光", f, br, pad, S(258));
                g.DrawString("音量", f, br, pad, S(312));
            }

            // Sun & Speaker icons beside sliders
            FluentGraphics.DrawSunIcon(g, new RectangleF(pad, S(280), S(24), S(26)), FluentGraphics.TextSecondary, _s);
            FluentGraphics.DrawSpeakerIcon(g, new RectangleF(pad, S(334), S(24), S(26)), FluentGraphics.TextSecondary, _s);
        }

        protected override void Dispose(bool disposing)
        {
            if (disposing)
            {
                _blDebounce.Dispose();
                _volDebounce.Dispose();
            }
            base.Dispose(disposing);
        }
    }

    sealed class SettingsDialog : Form
    {
        private readonly float _s;
        private readonly TextBox _tbAddress;
        private readonly Label _lblTestResult;
        private readonly CheckBox _chkAutoTakeover;
        private readonly CheckBox _chkWheelDpad;
        private readonly CheckBox _chkAutoStart;

        public string ResultAddress { get { return _tbAddress.Text.Trim(); } }
        public bool ResultAutoTakeover { get { return _chkAutoTakeover.Checked; } }
        public bool ResultWheelAsDpad { get { return _chkWheelDpad.Checked; } }
        public bool ResultAutoStart { get { return _chkAutoStart.Checked; } }

        public SettingsDialog(AppConfig cfg, DeviceController dev)
        {
            Text = "ADB 与键鼠设置 — G Pro 27U";
            FormBorderStyle = FormBorderStyle.FixedDialog;
            MaximizeBox = false;
            MinimizeBox = false;
            StartPosition = FormStartPosition.CenterScreen;
            BackColor = FluentGraphics.SurfaceBg;
            Font = new Font("Microsoft YaHei UI", 9.5f);

            using (var g = CreateGraphics())
                _s = g.DpiX / 96f;

            int W = S(420);
            int H = S(365);
            ClientSize = new Size(W, H);
            int pad = S(20);

            var lblAddr = new Label { Text = "电视无线 ADB 地址 (IP:端口)：", Left = pad, Top = S(16), Width = W - pad * 2, Height = S(22), ForeColor = FluentGraphics.TextPrimary };
            _tbAddress = new TextBox { Text = cfg.TvAddress, Left = pad, Top = S(42), Width = W - pad * 2 - S(92), Height = S(28) };
            var btnTest = new FluentButton { Text = "测试连接", DpiScale = _s, Left = W - pad - S(84), Top = S(40), Width = S(84), Height = S(30) };
            _lblTestResult = new Label { Text = "键盘模式已固定为 SDK（兼容 TV 应用与小企鹅中文）", Left = pad, Top = S(75), Width = W - pad * 2, Height = S(20), ForeColor = FluentGraphics.TextSecondary, Font = new Font("Microsoft YaHei UI", 8.5f) };

            btnTest.Click += async (s, e) =>
            {
                string addr = _tbAddress.Text.Trim();
                if (!addr.Contains(":")) addr += ":5555";
                _lblTestResult.ForeColor = FluentGraphics.StatusAmber;
                _lblTestResult.Text = "正在测试连接 " + addr + "...";
                var r = await dev.RunAdbAsync("connect " + addr, 3500);
                string txt = (r.StdOut + " " + r.StdErr).ToLowerInvariant();
                if (txt.Contains("connected to") || txt.Contains("already connected"))
                {
                    _lblTestResult.ForeColor = FluentGraphics.StatusGreen;
                    _lblTestResult.Text = "连接成功：" + addr;
                }
                else
                {
                    _lblTestResult.ForeColor = FluentGraphics.StatusRed;
                    _lblTestResult.Text = "连接失败，请检查电视 IP、同一局域网及 ADB 开关";
                }
            };

            _chkAutoTakeover = new CheckBox
            {
                Text = "切换到「电视系统」时自动接管电脑键鼠",
                Checked = cfg.AutoTakeoverOnTv,
                Left = pad,
                Top = S(108),
                Width = W - pad * 2,
                Height = S(24)
            };
            _chkWheelDpad = new CheckBox
            {
                Text = "接管期间鼠标滚轮映射为上下方向键",
                Checked = cfg.WheelAsDpad,
                Left = pad,
                Top = S(138),
                Width = W - pad * 2,
                Height = S(24)
            };
            _chkAutoStart = new CheckBox
            {
                Text = "Windows 开机自动启动",
                Checked = AppConfig.IsAutoStartEnabled(),
                Left = pad,
                Top = S(168),
                Width = W - pad * 2,
                Height = S(24)
            };

            // Shortcut reference card
            var guideBox = new Label
            {
                Text =
                    "接管电视期间遥控快捷键（切回电脑后自动注销，零误触）：\n" +
                    "• 方向 / 确认：方向键 / Enter（滚轮 = 上下方向）\n" +
                    "• 返回 / 主页 / 菜单：Esc / Home / F12（侧键 = 返回/主页）\n" +
                    "• 音量 + / - / 静音：PgUp / PgDn / Ctrl+Alt+M\n" +
                    "• 待机息屏 / 切回电脑 DP：Ctrl+Alt+P / Ctrl+Alt+D",
                Left = pad,
                Top = S(204),
                Width = W - pad * 2,
                Height = S(100),
                BackColor = Color.White,
                BorderStyle = BorderStyle.FixedSingle,
                Padding = new Padding(S(8)),
                ForeColor = FluentGraphics.TextSecondary,
                Font = new Font("Microsoft YaHei UI", 8.5f)
            };

            var btnSave = new FluentButton { Text = "保存", Primary = true, DpiScale = _s, Left = W - pad - S(168), Top = H - S(46), Width = S(78), Height = S(32) };
            var btnCancel = new FluentButton { Text = "取消", DpiScale = _s, Left = W - pad - S(80), Top = H - S(46), Width = S(80), Height = S(32) };

            btnSave.Click += (s, e) => { DialogResult = DialogResult.OK; Close(); };
            btnCancel.Click += (s, e) => { DialogResult = DialogResult.Cancel; Close(); };

            Controls.AddRange(new Control[]
            {
                lblAddr, _tbAddress, btnTest, _lblTestResult,
                _chkAutoTakeover, _chkWheelDpad, _chkAutoStart,
                guideBox, btnSave, btnCancel
            });
        }

        private int S(int px) { return (int)Math.Round(px * _s); }
    }
}
