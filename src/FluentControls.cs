using System;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Text;
using System.IO;
using System.Windows.Forms;

namespace MiTVSwitch
{
    enum TileIconKind { Tv, Monitor, Hdmi }

    static class FluentGraphics
    {
        public static readonly Color SurfaceBg = Color.FromArgb(245, 246, 249);
        public static readonly Color FooterBg = Color.FromArgb(236, 238, 243);
        public static readonly Color CardBg = Color.FromArgb(255, 255, 255);
        public static readonly Color CardHoverBg = Color.FromArgb(249, 250, 252);
        public static readonly Color CardBorder = Color.FromArgb(224, 227, 232);
        public static readonly Color AccentBlue = Color.FromArgb(0, 103, 192);
        public static readonly Color AccentBlueHover = Color.FromArgb(0, 90, 168);
        public static readonly Color TextPrimary = Color.FromArgb(26, 30, 36);
        public static readonly Color TextSecondary = Color.FromArgb(95, 102, 114);
        public static readonly Color DividerColor = Color.FromArgb(228, 231, 236);
        public static readonly Color StatusGreen = Color.FromArgb(16, 124, 16);
        public static readonly Color StatusAmber = Color.FromArgb(202, 130, 0);
        public static readonly Color StatusRed = Color.FromArgb(196, 43, 28);

        public static GraphicsPath CreateRoundedRect(RectangleF r, float radius)
        {
            var path = new GraphicsPath();
            float d = radius * 2f;
            if (d > r.Width) d = r.Width;
            if (d > r.Height) d = r.Height;
            path.AddArc(r.X, r.Y, d, d, 180, 90);
            path.AddArc(r.Right - d, r.Y, d, d, 270, 90);
            path.AddArc(r.Right - d, r.Bottom - d, d, d, 0, 90);
            path.AddArc(r.X, r.Bottom - d, d, d, 90, 90);
            path.CloseFigure();
            return path;
        }

        public static void DrawTileIcon(Graphics g, TileIconKind kind, RectangleF bounds, Color color, float scale)
        {
            g.SmoothingMode = SmoothingMode.AntiAlias;
            float cx = bounds.X + bounds.Width / 2f;
            float cy = bounds.Y + bounds.Height / 2f;
            float penW = 1.65f * scale;

            using (var pen = new Pen(color, penW))
            {
                pen.LineJoin = LineJoin.Round;
                pen.StartCap = LineCap.Round;
                pen.EndCap = LineCap.Round;

                if (kind == TileIconKind.Tv)
                {
                    // Sleek TV screen with tiny home roof inside
                    float w = 22f * scale;
                    float h = 15f * scale;
                    var scr = new RectangleF(cx - w / 2f, cy - h / 2f - 1.5f * scale, w, h);
                    using (var rp = CreateRoundedRect(scr, 2.2f * scale))
                        g.DrawPath(pen, rp);
                    // TV feet
                    g.DrawLine(pen, cx - 5.5f * scale, scr.Bottom + 3.2f * scale, cx - 3.5f * scale, scr.Bottom);
                    g.DrawLine(pen, cx + 5.5f * scale, scr.Bottom + 3.2f * scale, cx + 3.5f * scale, scr.Bottom);
                    // Play triangle inside TV
                    float py = scr.Y + scr.Height / 2f;
                    var tri = new PointF[]
                    {
                        new PointF(cx - 2.3f * scale, py - 3.1f * scale),
                        new PointF(cx - 2.3f * scale, py + 3.1f * scale),
                        new PointF(cx + 3.2f * scale, py)
                    };
                    using (var br = new SolidBrush(color))
                        g.FillPolygon(br, tri);
                }
                else if (kind == TileIconKind.Monitor)
                {
                    float w = 22f * scale;
                    float h = 14.5f * scale;
                    var scr = new RectangleF(cx - w / 2f, cy - h / 2f - 2f * scale, w, h);
                    using (var rp = CreateRoundedRect(scr, 2.0f * scale))
                        g.DrawPath(pen, rp);
                    g.DrawLine(pen, cx, scr.Bottom, cx, scr.Bottom + 3.8f * scale);
                    g.DrawLine(pen, cx - 5.5f * scale, scr.Bottom + 3.8f * scale, cx + 5.5f * scale, scr.Bottom + 3.8f * scale);
                }
                else
                {
                    // HDMI monitor with subtle port dot
                    float w = 22f * scale;
                    float h = 14.5f * scale;
                    var scr = new RectangleF(cx - w / 2f, cy - h / 2f - 2f * scale, w, h);
                    using (var rp = CreateRoundedRect(scr, 2.0f * scale))
                        g.DrawPath(pen, rp);
                    g.DrawLine(pen, cx - 4f * scale, scr.Bottom + 3.8f * scale, cx + 4f * scale, scr.Bottom + 3.8f * scale);
                    g.DrawLine(pen, cx, scr.Bottom, cx, scr.Bottom + 3.8f * scale);
                }
            }
        }

        public static void DrawSunIcon(Graphics g, RectangleF bounds, Color color, float scale)
        {
            g.SmoothingMode = SmoothingMode.AntiAlias;
            float cx = bounds.X + bounds.Width / 2f;
            float cy = bounds.Y + bounds.Height / 2f;
            float r = 3.8f * scale;
            using (var pen = new Pen(color, 1.45f * scale))
            {
                pen.StartCap = LineCap.Round;
                pen.EndCap = LineCap.Round;
                g.DrawEllipse(pen, cx - r, cy - r, r * 2f, r * 2f);
                for (int i = 0; i < 8; i++)
                {
                    double a = i * Math.PI / 4.0;
                    float r1 = 6.2f * scale;
                    float r2 = 8.2f * scale;
                    g.DrawLine(pen,
                        cx + (float)Math.Cos(a) * r1, cy + (float)Math.Sin(a) * r1,
                        cx + (float)Math.Cos(a) * r2, cy + (float)Math.Sin(a) * r2);
                }
            }
        }

        public static void DrawSpeakerIcon(Graphics g, RectangleF bounds, Color color, float scale)
        {
            g.SmoothingMode = SmoothingMode.AntiAlias;
            float cx = bounds.X + bounds.Width / 2f;
            float cy = bounds.Y + bounds.Height / 2f;
            using (var pen = new Pen(color, 1.45f * scale))
            {
                pen.LineJoin = LineJoin.Round;
                pen.StartCap = LineCap.Round;
                pen.EndCap = LineCap.Round;
                var pts = new PointF[]
                {
                    new PointF(cx - 7.5f * scale, cy - 2.6f * scale),
                    new PointF(cx - 4.2f * scale, cy - 2.6f * scale),
                    new PointF(cx - 0.5f * scale, cy - 6.0f * scale),
                    new PointF(cx - 0.5f * scale, cy + 6.0f * scale),
                    new PointF(cx - 4.2f * scale, cy + 2.6f * scale),
                    new PointF(cx - 7.5f * scale, cy + 2.6f * scale)
                };
                g.DrawPolygon(pen, pts);
                // Sound waves
                g.DrawArc(pen, cx - 1.5f * scale, cy - 3.6f * scale, 6.2f * scale, 7.2f * scale, -50, 100);
                g.DrawArc(pen, cx - 1.5f * scale, cy - 6.2f * scale, 10.2f * scale, 12.4f * scale, -50, 100);
            }
        }

        public static Icon CreateTrayIcon(bool takeoverActive)
        {
            using (var bmp = new Bitmap(32, 32))
            using (var g = Graphics.FromImage(bmp))
            {
                g.SmoothingMode = SmoothingMode.AntiAlias;
                g.Clear(Color.Transparent);

                Color stroke = takeoverActive ? Color.FromArgb(0, 130, 95) : Color.FromArgb(30, 36, 44);
                Color fill = takeoverActive ? Color.FromArgb(0, 168, 120) : Color.FromArgb(245, 248, 252);

                using (var bg = new SolidBrush(fill))
                using (var pen = new Pen(stroke, 2.5f))
                {
                    pen.LineJoin = LineJoin.Round;
                    pen.StartCap = LineCap.Round;
                    pen.EndCap = LineCap.Round;
                    var scr = new RectangleF(4, 5, 24, 16);
                    using (var rp = CreateRoundedRect(scr, 2.5f))
                    {
                        g.FillPath(bg, rp);
                        g.DrawPath(pen, rp);
                    }
                    g.DrawLine(pen, 16, 21, 16, 27);
                    g.DrawLine(pen, 10, 27, 22, 27);
                }

                using (var pngMs = new MemoryStream())
                {
                    bmp.Save(pngMs, System.Drawing.Imaging.ImageFormat.Png);
                    byte[] pngBytes = pngMs.ToArray();
                    using (var icoMs = new MemoryStream())
                    using (var bw = new BinaryWriter(icoMs))
                    {
                        bw.Write((short)0); bw.Write((short)1); bw.Write((short)1);
                        bw.Write((byte)32); bw.Write((byte)32); bw.Write((byte)0); bw.Write((byte)0);
                        bw.Write((short)1); bw.Write((short)32);
                        bw.Write(pngBytes.Length); bw.Write(22);
                        bw.Write(pngBytes); bw.Flush();
                        icoMs.Position = 0;
                        return new Icon(icoMs);
                    }
                }
            }
        }
    }

    sealed class FluentSourceTile : Control
    {
        private bool _active;
        private bool _hovered;
        private bool _pressed;

        public TileIconKind IconKind { get; set; }
        public string Caption { get; set; }
        public float DpiScale { get; set; }

        public bool Active
        {
            get { return _active; }
            set { if (_active != value) { _active = value; Invalidate(); } }
        }

        public FluentSourceTile()
        {
            DpiScale = 1f;
            SetStyle(ControlStyles.AllPaintingInWmPaint | ControlStyles.UserPaint | ControlStyles.OptimizedDoubleBuffer | ControlStyles.ResizeRedraw, true);
            Cursor = Cursors.Hand;
        }

        protected override void OnMouseEnter(EventArgs e) { _hovered = true; Invalidate(); base.OnMouseEnter(e); }
        protected override void OnMouseLeave(EventArgs e) { _hovered = false; _pressed = false; Invalidate(); base.OnMouseLeave(e); }
        protected override void OnMouseDown(MouseEventArgs e) { if (e.Button == MouseButtons.Left) { _pressed = true; Invalidate(); } base.OnMouseDown(e); }
        protected override void OnMouseUp(MouseEventArgs e) { _pressed = false; Invalidate(); base.OnMouseUp(e); }

        protected override void OnPaint(PaintEventArgs e)
        {
            var g = e.Graphics;
            g.SmoothingMode = SmoothingMode.AntiAlias;
            g.TextRenderingHint = TextRenderingHint.ClearTypeGridFit;
            g.Clear(Parent != null ? Parent.BackColor : FluentGraphics.SurfaceBg);

            float s = DpiScale;
            float labelH = 22f * s;
            float gap = 5f * s;
            var cardRect = new RectangleF(1f, 1f, Width - 2f, Height - labelH - gap - 1f);

            Color fill = _active
                ? (_hovered ? FluentGraphics.AccentBlueHover : FluentGraphics.AccentBlue)
                : (_pressed ? Color.FromArgb(236, 239, 244) : (_hovered ? FluentGraphics.CardHoverBg : FluentGraphics.CardBg));
            Color border = _active ? fill : FluentGraphics.CardBorder;
            Color iconColor = _active ? Color.White : FluentGraphics.TextPrimary;

            using (var path = FluentGraphics.CreateRoundedRect(cardRect, 7f * s))
            using (var br = new SolidBrush(fill))
            using (var pen = new Pen(border, 1f))
            {
                g.FillPath(br, path);
                g.DrawPath(pen, path);
            }

            FluentGraphics.DrawTileIcon(g, IconKind, cardRect, iconColor, s);

            var labelRect = new RectangleF(0, cardRect.Bottom + gap, Width, labelH);
            using (var sf = new StringFormat { Alignment = StringAlignment.Center, LineAlignment = StringAlignment.Center })
            using (var tb = new SolidBrush(_active ? FluentGraphics.AccentBlue : FluentGraphics.TextPrimary))
            using (var f = new Font("Microsoft YaHei UI", 9.0f, _active ? FontStyle.Bold : FontStyle.Regular))
            {
                g.DrawString(Caption ?? "", f, tb, labelRect, sf);
            }
        }
    }

    sealed class FluentSlider : Control
    {
        private int _value = 50;
        private bool _dragging;
        private bool _hovered;

        public int Minimum { get; set; }
        public int Maximum { get; set; }
        public float DpiScale { get; set; }

        public event EventHandler ValueChanged;
        public event EventHandler ValueCommitted;

        public int Value
        {
            get { return _value; }
            set
            {
                int clamped = Math.Max(Minimum, Math.Min(Maximum, value));
                if (_value != clamped)
                {
                    _value = clamped;
                    Invalidate();
                }
            }
        }

        public bool IsDragging { get { return _dragging; } }

        public FluentSlider()
        {
            Minimum = 0;
            Maximum = 100;
            DpiScale = 1f;
            SetStyle(ControlStyles.AllPaintingInWmPaint | ControlStyles.UserPaint | ControlStyles.OptimizedDoubleBuffer | ControlStyles.ResizeRedraw, true);
            Cursor = Cursors.Hand;
        }

        private void UpdateFromX(int x)
        {
            float pad = 11f * DpiScale;
            float trackW = Math.Max(1f, Width - pad * 2f);
            float ratio = (x - pad) / trackW;
            if (ratio < 0f) ratio = 0f;
            if (ratio > 1f) ratio = 1f;
            int nv = Minimum + (int)Math.Round(ratio * (Maximum - Minimum));
            if (nv != _value)
            {
                _value = nv;
                Invalidate();
                var h = ValueChanged;
                if (h != null) h(this, EventArgs.Empty);
            }
        }

        protected override void OnMouseEnter(EventArgs e) { _hovered = true; Invalidate(); base.OnMouseEnter(e); }
        protected override void OnMouseLeave(EventArgs e) { _hovered = false; Invalidate(); base.OnMouseLeave(e); }

        protected override void OnMouseDown(MouseEventArgs e)
        {
            if (e.Button == MouseButtons.Left)
            {
                _dragging = true;
                Capture = true;
                UpdateFromX(e.X);
            }
            base.OnMouseDown(e);
        }

        protected override void OnMouseMove(MouseEventArgs e)
        {
            if (_dragging) UpdateFromX(e.X);
            base.OnMouseMove(e);
        }

        protected override void OnMouseUp(MouseEventArgs e)
        {
            if (_dragging)
            {
                _dragging = false;
                Capture = false;
                Invalidate();
                var h = ValueCommitted;
                if (h != null) h(this, EventArgs.Empty);
            }
            base.OnMouseUp(e);
        }

        protected override void OnPaint(PaintEventArgs e)
        {
            var g = e.Graphics;
            g.SmoothingMode = SmoothingMode.AntiAlias;
            g.Clear(Parent != null ? Parent.BackColor : FluentGraphics.SurfaceBg);

            float s = DpiScale;
            float pad = 11f * s;
            float trackH = 4f * s;
            float cy = Height / 2f;
            float trackW = Math.Max(1f, Width - pad * 2f);
            float ratio = (Maximum > Minimum) ? (float)(_value - Minimum) / (Maximum - Minimum) : 0f;
            float thumbX = pad + trackW * ratio;

            // Unfilled track
            var fullTrack = new RectangleF(pad, cy - trackH / 2f, trackW, trackH);
            using (var p = FluentGraphics.CreateRoundedRect(fullTrack, trackH / 2f))
            using (var br = new SolidBrush(Color.FromArgb(196, 200, 207)))
                g.FillPath(br, p);

            // Filled track
            if (thumbX > pad + 1f)
            {
                var fillTrack = new RectangleF(pad, cy - trackH / 2f, thumbX - pad, trackH);
                using (var p = FluentGraphics.CreateRoundedRect(fillTrack, trackH / 2f))
                using (var br = new SolidBrush(FluentGraphics.AccentBlue))
                    g.FillPath(br, p);
            }

            // Thumb outer white circle with subtle border
            float outerR = 9f * s;
            var outerRect = new RectangleF(thumbX - outerR, cy - outerR, outerR * 2f, outerR * 2f);
            using (var br = new SolidBrush(Color.White))
            using (var pen = new Pen(Color.FromArgb(204, 208, 214), 1.1f))
            {
                g.FillEllipse(br, outerRect);
                g.DrawEllipse(pen, outerRect);
            }

            // Thumb inner accent dot (grows slightly on hover/drag, just like WinUI 3!)
            float innerR = (_dragging ? 4.2f : (_hovered ? 5.6f : 4.8f)) * s;
            var innerRect = new RectangleF(thumbX - innerR, cy - innerR, innerR * 2f, innerR * 2f);
            using (var br = new SolidBrush(FluentGraphics.AccentBlue))
                g.FillEllipse(br, innerRect);
        }
    }

    sealed class FluentButton : Control
    {
        private bool _hovered;
        private bool _pressed;
        public bool Primary { get; set; }
        public float DpiScale { get; set; }

        public FluentButton()
        {
            DpiScale = 1f;
            SetStyle(ControlStyles.AllPaintingInWmPaint | ControlStyles.UserPaint | ControlStyles.OptimizedDoubleBuffer | ControlStyles.ResizeRedraw, true);
            Cursor = Cursors.Hand;
        }

        protected override void OnMouseEnter(EventArgs e) { _hovered = true; Invalidate(); base.OnMouseEnter(e); }
        protected override void OnMouseLeave(EventArgs e) { _hovered = false; _pressed = false; Invalidate(); base.OnMouseLeave(e); }
        protected override void OnMouseDown(MouseEventArgs e) { if (e.Button == MouseButtons.Left) { _pressed = true; Invalidate(); } base.OnMouseDown(e); }
        protected override void OnMouseUp(MouseEventArgs e) { _pressed = false; Invalidate(); base.OnMouseUp(e); }

        protected override void OnPaint(PaintEventArgs e)
        {
            var g = e.Graphics;
            g.SmoothingMode = SmoothingMode.AntiAlias;
            g.TextRenderingHint = TextRenderingHint.ClearTypeGridFit;
            g.Clear(Parent != null ? Parent.BackColor : FluentGraphics.FooterBg);

            float s = DpiScale;
            var rect = new RectangleF(0.5f, 0.5f, Width - 1.5f, Height - 1.5f);

            Color fill = Primary
                ? (_hovered ? FluentGraphics.AccentBlueHover : FluentGraphics.AccentBlue)
                : (_pressed ? Color.FromArgb(234, 237, 242) : (_hovered ? Color.FromArgb(248, 249, 251) : Color.White));
            Color border = Primary ? fill : Color.FromArgb(215, 219, 226);
            Color fg = Primary ? Color.White : FluentGraphics.TextPrimary;

            using (var path = FluentGraphics.CreateRoundedRect(rect, 5f * s))
            using (var br = new SolidBrush(fill))
            using (var pen = new Pen(border, 1f))
            {
                g.FillPath(br, path);
                g.DrawPath(pen, path);
            }

            using (var sf = new StringFormat { Alignment = StringAlignment.Center, LineAlignment = StringAlignment.Center })
            using (var tb = new SolidBrush(fg))
            using (var f = new Font("Microsoft YaHei UI", 9.0f))
            {
                g.DrawString(Text ?? "", f, tb, rect, sf);
            }
        }
    }
}
