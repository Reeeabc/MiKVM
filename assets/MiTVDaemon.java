import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.lang.reflect.Method;

/**
 * Lightweight on-demand stdin/stdout bridge for Redmi G Pro 27U (MT9655).
 * Keeps ART + TVNative + AudioManager warm while the flyout/takeover is active,
 * reducing per-step backlight & volume writes from ~400ms to ~5-15ms.
 *
 * Commands on stdin:
 *   B <0..100>   - Set hardware backlight immediately via MTK TVNative
 *   V <0..100>   - Set media volume (stream 3) immediately via AudioManager
 *   G            - Print current BL=<n> VOL=<n>
 *   Q            - Exit cleanly
 */
public class MiTVDaemon {
    private static final String TV_CLASSPATH =
            "/system/framework/mitvmiddlewareimpl.jar:/system_ext/priv-app/TvServices/TvServices.apk";
    private static final String NATIVE_LIB_PATH =
            "/vendor/lib64:/system/lib64:/system_ext/lib64";
    private static final String KEY_BACKLIGHT = "g_disp__disp_back_light";

    public static void main(String[] args) {
        Method getConfig = null;
        Method setConfig = null;
        Object audioMgr = null;
        Method setVol = null;
        Method getVol = null;

        try {
            Class<?> pcl = Class.forName("dalvik.system.PathClassLoader");
            ClassLoader cl = (ClassLoader) pcl
                    .getConstructor(String.class, String.class, ClassLoader.class)
                    .newInstance(TV_CLASSPATH, NATIVE_LIB_PATH, ClassLoader.getSystemClassLoader());
            Class<?> tvNative = cl.loadClass("com.mediatek.twoworlds.tv.TVNative");
            getConfig = tvNative.getDeclaredMethod("getConfigValue_native", int.class, String.class);
            getConfig.setAccessible(true);
            setConfig = tvNative.getDeclaredMethod("setConfigValue_native", int.class, String.class, int.class, int.class);
            setConfig.setAccessible(true);
        } catch (Throwable ignored) {
        }

        try {
            Class<?> looperCls = Class.forName("android.os.Looper");
            looperCls.getMethod("prepareMainLooper").invoke(null);
            Class<?> atCls = Class.forName("android.app.ActivityThread");
            Object at = atCls.getMethod("systemMain").invoke(null);
            Object ctx = atCls.getMethod("getSystemContext").invoke(at);
            audioMgr = ctx.getClass().getMethod("getSystemService", String.class).invoke(ctx, "audio");
            if (audioMgr != null) {
                setVol = audioMgr.getClass().getMethod("setStreamVolume", int.class, int.class, int.class);
                getVol = audioMgr.getClass().getMethod("getStreamVolume", int.class);
            }
        } catch (Throwable ignored) {
        }

        System.out.println("READY");
        System.out.flush();

        try (BufferedReader br = new BufferedReader(new InputStreamReader(System.in))) {
            String line;
            while ((line = br.readLine()) != null) {
                String trimmed = line.trim();
                if (trimmed.isEmpty()) {
                    continue;
                }
                if ("Q".equals(trimmed)) {
                    break;
                }
                if ("G".equals(trimmed)) {
                    int bl = -1;
                    int vol = -1;
                    try {
                        if (getConfig != null) {
                            Object r = getConfig.invoke(null, 0, KEY_BACKLIGHT);
                            if (r instanceof Integer) bl = (Integer) r;
                        }
                    } catch (Throwable ignored) {
                    }
                    try {
                        if (audioMgr != null && getVol != null) {
                            Object r = getVol.invoke(audioMgr, 3);
                            if (r instanceof Integer) vol = (Integer) r;
                        }
                    } catch (Throwable ignored) {
                    }
                    System.out.println("G " + bl + " " + vol);
                    System.out.flush();
                    continue;
                }
                if (trimmed.startsWith("B ")) {
                    try {
                        int v = Math.max(0, Math.min(100, Integer.parseInt(trimmed.substring(2).trim())));
                        if (setConfig != null) {
                            setConfig.invoke(null, 0, KEY_BACKLIGHT, v, 3);
                            System.out.println("OK B " + v);
                        } else {
                            System.out.println("ERR NO_MTK");
                        }
                    } catch (Throwable t) {
                        System.out.println("ERR " + t.getClass().getSimpleName());
                    }
                    System.out.flush();
                    continue;
                }
                if (trimmed.startsWith("V ")) {
                    try {
                        int v = Math.max(0, Math.min(100, Integer.parseInt(trimmed.substring(2).trim())));
                        if (audioMgr != null && setVol != null) {
                            setVol.invoke(audioMgr, 3, v, 1); // AudioManager.FLAG_SHOW_UI = 1
                            System.out.println("OK V " + v);
                        } else {
                            System.out.println("ERR NO_AUDIO");
                        }
                    } catch (Throwable t) {
                        System.out.println("ERR " + t.getClass().getSimpleName());
                    }
                    System.out.flush();
                }
            }
        } catch (Throwable ignored) {
        }
        System.exit(0);
    }
}
