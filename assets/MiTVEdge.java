import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.lang.reflect.Constructor;
import java.lang.reflect.InvocationHandler;
import java.lang.reflect.Method;
import java.lang.reflect.Proxy;

/**
 * Ultra-lightweight 3px transparent TV edge hover sensor for MiKVM Phase 2.
 * Runs via `app_process` (no APK install required; supports both rootless `com.android.shell`
 * with `appops SYSTEM_ALERT_WINDOW allow` and rooted `su 1000`).
 *
 * Protocol:
 *   stdout:
 *     READY <w> <h>     - Overlay attached & ready
 *     H <0..10000>      - Mouse hovering on edge strip at normalized position ratio (x10000)
 *     L                 - Mouse left edge strip
 *     ERR <reason>      - Startup failed
 *   stdin:
 *     S LEFT|RIGHT|TOP|BOTTOM|HIDE  - Move or hide edge strip dynamically
 *     Q                             - Remove view cleanly & System.exit(0)
 */
public class MiTVEdge {
    private static final int TYPE_APPLICATION_OVERLAY = 2038;
    private static final int FLAG_NOT_FOCUSABLE = 0x00000008;
    private static final int FLAG_NOT_TOUCHABLE = 0x00000010;
    private static final int FLAG_NOT_TOUCH_MODAL = 0x00000020;
    private static final int FLAG_LAYOUT_IN_SCREEN = 0x00000100;
    private static final int FLAG_LAYOUT_NO_LIMITS = 0x00000200;
    private static final int PIXEL_FORMAT_TRANSLUCENT = -3;

    private static final int GRAVITY_TOP = 0x30;
    private static final int GRAVITY_BOTTOM = 0x50;
    private static final int GRAVITY_LEFT = 0x03;
    private static final int GRAVITY_RIGHT = 0x05;

    private static final int ACTION_HOVER_MOVE = 7;
    private static final int ACTION_HOVER_ENTER = 9;
    private static final int ACTION_HOVER_EXIT = 10;

    private static final int VIEW_VISIBLE = 0;
    private static final int VIEW_GONE = 8;
    private static final int STRIP_PX = 3;

    private static volatile String currentSide = "RIGHT";
    private static volatile int screenW = 1920;
    private static volatile int screenH = 1080;
    private static volatile long lastEmitMs = 0L;
    private static volatile int lastEmittedR10k = -1;

    private static Object windowMgr;
    private static Object edgeView;
    private static Object layoutParams;
    private static Method wmUpdateView;
    private static Method wmRemoveView;
    private static Method viewSetVisibility;
    private static Method evGetActionMasked;
    private static Method evGetRawX;
    private static Method evGetRawY;

    public static void main(String[] args) {
        if (args != null && args.length > 0 && args[0] != null && !args[0].trim().isEmpty()) {
            currentSide = args[0].trim().toUpperCase();
        }

        try {
            Class<?> looperCls = Class.forName("android.os.Looper");
            looperCls.getMethod("prepareMainLooper").invoke(null);
            Object mainLooper = looperCls.getMethod("getMainLooper").invoke(null);

            Class<?> atCls = Class.forName("android.app.ActivityThread");
            Object at = atCls.getMethod("systemMain").invoke(null);
            Object sysCtx = atCls.getMethod("getSystemContext").invoke(at);

            int uid = -1;
            try {
                Class<?> procCls = Class.forName("android.os.Process");
                Object r = procCls.getMethod("myUid").invoke(null);
                if (r instanceof Integer) uid = (Integer) r;
            } catch (Throwable ignored) {
            }

            // Initialize Workarounds (from scrcpy-server.jar on classpath) when available
            try {
                Class<?> wkCls = Class.forName("com.genymobile.scrcpy.Workarounds");
                wkCls.getMethod("apply").invoke(null);
            } catch (Throwable ignored) {
            }

            Object ctx = sysCtx;
            String pkgName = "android";
            if (uid == 2000) {
                pkgName = "com.android.shell";
                patchShellContext(sysCtx, uid, pkgName);
            }

            // Replace ContextImpl.mContentResolver with FakeContext's safe ContentResolver
            // so ViewRootImpl.<init> -> Settings.System.getInt() never fails in app_process!
            Object fakeCtx = null;
            try {
                Class<?> fcCls = Class.forName("com.genymobile.scrcpy.FakeContext");
                fakeCtx = fcCls.getMethod("get").invoke(null);
                if (fakeCtx != null) {
                    Object safeCr = fakeCtx.getClass().getMethod("getContentResolver").invoke(fakeCtx);
                    if (safeCr != null) {
                        patchField(sysCtx, "mContentResolver", safeCr);
                    }
                    if (uid == 2000) {
                        ctx = fakeCtx;
                    }
                }
            } catch (Throwable ignored) {
            }

            windowMgr = ctx.getClass().getMethod("getSystemService", String.class).invoke(ctx, "window");
            if (windowMgr == null) {
                fail("NO_WM");
                return;
            }

            queryScreenSize(windowMgr);

            Class<?> viewCls = Class.forName("android.view.View");
            Class<?> ctxCls = Class.forName("android.content.Context");
            Constructor<?> viewCtor = viewCls.getConstructor(ctxCls);
            edgeView = viewCtor.newInstance(ctx);
            viewSetVisibility = viewCls.getMethod("setVisibility", int.class);
            try {
                viewCls.getMethod("setBackgroundColor", int.class).invoke(edgeView, 0x00000000);
            } catch (Throwable ignored) {
            }

            Class<?> evCls = Class.forName("android.view.MotionEvent");
            evGetActionMasked = evCls.getMethod("getActionMasked");
            evGetRawX = evCls.getMethod("getRawX");
            evGetRawY = evCls.getMethod("getRawY");

            Class<?> hoverIfc = Class.forName("android.view.View$OnHoverListener");
            Object hoverProxy = Proxy.newProxyInstance(
                    hoverIfc.getClassLoader(),
                    new Class<?>[]{hoverIfc},
                    new InvocationHandler() {
                        @Override
                        public Object invoke(Object proxy, Method method, Object[] mArgs) {
                            String mName = method.getName();
                            if ("onHover".equals(mName) && mArgs != null && mArgs.length >= 2) {
                                handleHoverEvent(mArgs[1]);
                                return Boolean.TRUE;
                            }
                            if ("toString".equals(mName)) return "MiTVEdgeHover";
                            if ("hashCode".equals(mName)) return 0;
                            if ("equals".equals(mName)) return Boolean.FALSE;
                            return Boolean.FALSE;
                        }
                    }
            );
            viewCls.getMethod("setOnHoverListener", hoverIfc).invoke(edgeView, hoverProxy);

            Class<?> lpCls = Class.forName("android.view.WindowManager$LayoutParams");
            Class<?> vgLpCls = Class.forName("android.view.ViewGroup$LayoutParams");
            layoutParams = lpCls.getConstructor().newInstance();
            lpCls.getField("type").setInt(layoutParams, TYPE_APPLICATION_OVERLAY);
            lpCls.getField("flags").setInt(
                    layoutParams,
                    FLAG_NOT_FOCUSABLE | FLAG_NOT_TOUCH_MODAL | FLAG_LAYOUT_IN_SCREEN | FLAG_LAYOUT_NO_LIMITS
            );
            lpCls.getField("format").setInt(layoutParams, PIXEL_FORMAT_TRANSLUCENT);
            lpCls.getField("packageName").set(layoutParams, pkgName);
            try {
                lpCls.getMethod("setTitle", CharSequence.class).invoke(layoutParams, "MiTVEdge");
            } catch (Throwable ignored) {
            }

            applySideToParams(currentSide);
            if ("HIDE".equals(currentSide)) {
                viewSetVisibility.invoke(edgeView, VIEW_GONE);
            }

            Method wmAddView = windowMgr.getClass().getMethod("addView", viewCls, vgLpCls);
            wmUpdateView = windowMgr.getClass().getMethod("updateViewLayout", viewCls, vgLpCls);
            try {
                wmRemoveView = windowMgr.getClass().getMethod("removeViewImmediate", viewCls);
            } catch (Throwable ignored) {
                wmRemoveView = windowMgr.getClass().getMethod("removeView", viewCls);
            }
            wmAddView.invoke(windowMgr, edgeView, layoutParams);

            Class<?> handlerCls = Class.forName("android.os.Handler");
            final Object mainHandler = handlerCls.getConstructor(looperCls).newInstance(mainLooper);
            final Method handlerPost = handlerCls.getMethod("post", Runnable.class);

            synchronized (System.out) {
                System.out.println("READY " + screenW + " " + screenH);
                System.out.flush();
            }

            Thread stdinThread = new Thread(new Runnable() {
                @Override
                public void run() {
                    try (BufferedReader br = new BufferedReader(new InputStreamReader(System.in))) {
                        String line;
                        while ((line = br.readLine()) != null) {
                            final String cmd = line.trim();
                            if (cmd.isEmpty()) continue;
                            if ("Q".equals(cmd)) {
                                break;
                            }
                            if (cmd.startsWith("S ")) {
                                final String side = cmd.substring(2).trim().toUpperCase();
                                try {
                                    handlerPost.invoke(mainHandler, new Runnable() {
                                        @Override
                                        public void run() {
                                            updateSideOnMain(side);
                                        }
                                    });
                                } catch (Throwable ignored) {
                                }
                            }
                        }
                    } catch (Throwable ignored) {
                    }
                    try {
                        handlerPost.invoke(mainHandler, new Runnable() {
                            @Override
                            public void run() {
                                cleanupAndExit();
                            }
                        });
                        Thread.sleep(250);
                    } catch (Throwable ignored) {
                    }
                    System.exit(0);
                }
            }, "MiTVEdge-Stdin");
            stdinThread.setDaemon(true);
            stdinThread.start();

            looperCls.getMethod("loop").invoke(null);
        } catch (Throwable t) {
            Throwable cause = t.getCause() != null ? t.getCause() : t;
            fail(cause.getClass().getSimpleName() + ": " + cause.getMessage());
        }
        System.exit(0);
    }

    private static void patchField(Object obj, String fieldName, Object value) {
        Class<?> c = obj.getClass();
        while (c != null) {
            try {
                java.lang.reflect.Field f = c.getDeclaredField(fieldName);
                f.setAccessible(true);
                f.set(obj, value);
                return;
            } catch (Throwable ignored) {
            }
            c = c.getSuperclass();
        }
    }

    private static void patchShellContext(Object ctx, int uid, String pkgName) {
        Class<?> c = ctx.getClass();
        while (c != null) {
            for (String fieldName : new String[]{"mBasePackageName", "mOpPackageName"}) {
                try {
                    java.lang.reflect.Field f = c.getDeclaredField(fieldName);
                    f.setAccessible(true);
                    f.set(ctx, pkgName);
                } catch (Throwable ignored) {
                }
            }
            try {
                java.lang.reflect.Field af = c.getDeclaredField("mAttributionSource");
                af.setAccessible(true);
                Class<?> builderCls = Class.forName("android.content.AttributionSource$Builder");
                Object builder = builderCls.getConstructor(int.class).newInstance(uid);
                builderCls.getMethod("setPackageName", String.class).invoke(builder, pkgName);
                Object attrSrc = builderCls.getMethod("build").invoke(builder);
                af.set(ctx, attrSrc);
            } catch (Throwable ignored) {
            }
            c = c.getSuperclass();
        }
    }

    private static void queryScreenSize(Object wm) {
        try {
            // Try API 30+ WindowMetrics first, fallback to Display.getRealSize
            try {
                Object metrics = wm.getClass().getMethod("getMaximumWindowMetrics").invoke(wm);
                Object bounds = metrics.getClass().getMethod("getBounds").invoke(metrics);
                int w = (Integer) bounds.getClass().getMethod("width").invoke(bounds);
                int h = (Integer) bounds.getClass().getMethod("height").invoke(bounds);
                if (w > 0 && h > 0) {
                    screenW = w;
                    screenH = h;
                    return;
                }
            } catch (Throwable ignored) {
            }
            Object disp = wm.getClass().getMethod("getDefaultDisplay").invoke(wm);
            Class<?> ptCls = Class.forName("android.graphics.Point");
            Object pt = ptCls.getConstructor().newInstance();
            disp.getClass().getMethod("getRealSize", ptCls).invoke(disp, pt);
            int w = ptCls.getField("x").getInt(pt);
            int h = ptCls.getField("y").getInt(pt);
            if (w > 0 && h > 0) {
                screenW = w;
                screenH = h;
            }
        } catch (Throwable ignored) {
        }
    }

    private static void applySideToParams(String side) throws Exception {
        Class<?> lpCls = layoutParams.getClass();
        int w;
        int h;
        int gravity;
        if ("LEFT".equals(side)) {
            w = STRIP_PX;
            h = -1; // MATCH_PARENT
            gravity = GRAVITY_LEFT | GRAVITY_TOP;
        } else if ("TOP".equals(side)) {
            w = -1;
            h = STRIP_PX;
            gravity = GRAVITY_TOP | GRAVITY_LEFT;
        } else if ("BOTTOM".equals(side)) {
            w = -1;
            h = STRIP_PX;
            gravity = GRAVITY_BOTTOM | GRAVITY_LEFT;
        } else {
            // Default RIGHT
            w = STRIP_PX;
            h = -1;
            gravity = GRAVITY_RIGHT | GRAVITY_TOP;
        }
        lpCls.getField("width").setInt(layoutParams, w);
        lpCls.getField("height").setInt(layoutParams, h);
        lpCls.getField("gravity").setInt(layoutParams, gravity);
        lpCls.getField("x").setInt(layoutParams, 0);
        lpCls.getField("y").setInt(layoutParams, 0);
    }

    private static void updateSideOnMain(String side) {
        try {
            if (side.equals(currentSide)) {
                return;
            }
            Class<?> lpCls = layoutParams.getClass();
            int baseFlags = FLAG_NOT_FOCUSABLE | FLAG_NOT_TOUCH_MODAL | FLAG_LAYOUT_IN_SCREEN | FLAG_LAYOUT_NO_LIMITS;
            if ("HIDE".equals(side)) {
                currentSide = "HIDE";
                lastEmittedR10k = -1;
                lpCls.getField("flags").setInt(layoutParams, baseFlags | FLAG_NOT_TOUCHABLE);
                viewSetVisibility.invoke(edgeView, VIEW_GONE);
                wmUpdateView.invoke(windowMgr, edgeView, layoutParams);
            } else {
                currentSide = side;
                lastEmittedR10k = -1;
                queryScreenSize(windowMgr);
                applySideToParams(side);
                lpCls.getField("flags").setInt(layoutParams, baseFlags);
                viewSetVisibility.invoke(edgeView, VIEW_VISIBLE);
                wmUpdateView.invoke(windowMgr, edgeView, layoutParams);
            }
        } catch (Throwable ignored) {
        }
    }

    private static void handleHoverEvent(Object ev) {
        try {
            int action = (Integer) evGetActionMasked.invoke(ev);
            if (action == ACTION_HOVER_EXIT) {
                lastEmittedR10k = -1;
                synchronized (System.out) {
                    System.out.println("L");
                    System.out.flush();
                }
                return;
            }
            if (action == ACTION_HOVER_ENTER || action == ACTION_HOVER_MOVE) {
                String side = currentSide;
                float ratio;
                if ("TOP".equals(side) || "BOTTOM".equals(side)) {
                    float rx = (Float) evGetRawX.invoke(ev);
                    ratio = screenW > 0 ? rx / (float) screenW : 0.5f;
                } else {
                    float ry = (Float) evGetRawY.invoke(ev);
                    ratio = screenH > 0 ? ry / (float) screenH : 0.5f;
                }
                int r10k = Math.max(0, Math.min(10000, Math.round(ratio * 10000.0f)));
                long now = System.currentTimeMillis();
                // Always emit immediately on HOVER_ENTER or any meaningful position shift (>= 0.6%),
                // so multi-packet bursts NEVER lose their final landed coordinate to throttling!
                boolean jumped = (lastEmittedR10k < 0) || Math.abs(r10k - lastEmittedR10k) >= 60;
                if (action == ACTION_HOVER_MOVE && !jumped && (now - lastEmitMs) < 16L) {
                    return;
                }
                lastEmitMs = now;
                lastEmittedR10k = r10k;
                synchronized (System.out) {
                    System.out.println("H " + r10k);
                    System.out.flush();
                }
            }
        } catch (Throwable ignored) {
        }
    }

    private static void cleanupAndExit() {
        try {
            if (windowMgr != null && edgeView != null && wmRemoveView != null) {
                wmRemoveView.invoke(windowMgr, edgeView);
            }
        } catch (Throwable ignored) {
        }
        System.exit(0);
    }

    private static void fail(String reason) {
        synchronized (System.out) {
            System.out.println("ERR " + reason);
            System.out.flush();
        }
        System.exit(1);
    }
}
