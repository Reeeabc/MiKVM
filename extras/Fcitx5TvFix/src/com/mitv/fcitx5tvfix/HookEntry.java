package com.mitv.fcitx5tvfix;

import android.inputmethodservice.InputMethodService;
import android.text.InputType;
import android.util.Log;
import android.view.KeyEvent;
import android.view.ViewGroup;
import android.view.Window;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputMethodSubtype;
import io.github.libxposed.api.XposedModule;
import java.lang.reflect.Method;

/**
 * Fix Fcitx5 Android focus / ANR / D-pad key interception on Xiaomi TV (HyperOS).
 * Scope: org.fcitx.fcitx5.android ONLY (does not touch system_server).
 */
public final class HookEntry extends XposedModule {

    private static final String TAG = "Fcitx5TvFix";
    private static final String TARGET_PKG = "org.fcitx.fcitx5.android";
    private static final String IMS_CLASS = "org.fcitx.fcitx5.android.input.FcitxInputMethodService";

    private static volatile boolean hooked;

    @Override
    public void onModuleLoaded(ModuleLoadedParam param) {
        log(Log.INFO, TAG, "Loaded in process: " + param.getProcessName());
    }

    @Override
    public void onPackageReady(PackageReadyParam param) {
        if (!TARGET_PKG.equals(param.getPackageName()) || hooked) {
            return;
        }
        try {
            ClassLoader cl = param.getClassLoader();
            Class<?> imsCls = cl.loadClass(IMS_CLASS);

            // 1. onEvaluateInputViewShown(): never force IME shown when no text field is focused
            Method mEval = imsCls.getDeclaredMethod("onEvaluateInputViewShown");
            hook(mEval).intercept(chain -> {
                InputMethodService svc = (InputMethodService) chain.getThisObject();
                if (isNoTextInput(svc)) {
                    return Boolean.FALSE;
                }
                return chain.proceed();
            });

            // 2. onStartInput(EditorInfo, boolean): hide IME immediately when entering non-text screens
            Method mStartInput = imsCls.getDeclaredMethod("onStartInput", EditorInfo.class, boolean.class);
            hook(mStartInput).intercept(chain -> {
                Object res = chain.proceed();
                InputMethodService svc = (InputMethodService) chain.getThisObject();
                EditorInfo ei = (EditorInfo) chain.getArg(0);
                if (isNoTextInput(ei)) {
                    try {
                        svc.requestHideSelf(0);
                    } catch (Throwable ignored) {
                    }
                }
                return res;
            });

            // 3. onKeyDown / onKeyUp: never intercept D-pad / Enter / Back / TV keys outside text inputs
            Method mKeyDown = imsCls.getDeclaredMethod("onKeyDown", int.class, KeyEvent.class);
            hook(mKeyDown).intercept(chain -> {
                InputMethodService svc = (InputMethodService) chain.getThisObject();
                int keyCode = (Integer) chain.getArg(0);
                if (shouldBypassKey(svc, keyCode)) {
                    return Boolean.FALSE;
                }
                return chain.proceed();
            });

            Method mKeyUp = imsCls.getDeclaredMethod("onKeyUp", int.class, KeyEvent.class);
            hook(mKeyUp).intercept(chain -> {
                InputMethodService svc = (InputMethodService) chain.getThisObject();
                int keyCode = (Integer) chain.getArg(0);
                if (shouldBypassKey(svc, keyCode)) {
                    return Boolean.FALSE;
                }
                return chain.proceed();
            });

            // 4. onComputeInsets: make touchable region empty when no text field is focused
            Method mInsets = imsCls.getDeclaredMethod("onComputeInsets", InputMethodService.Insets.class);
            hook(mInsets).intercept(chain -> {
                Object res = chain.proceed();
                InputMethodService svc = (InputMethodService) chain.getThisObject();
                if (isNoTextInput(svc)) {
                    InputMethodService.Insets out = (InputMethodService.Insets) chain.getArg(0);
                    if (out != null) {
                        out.touchableInsets = InputMethodService.Insets.TOUCHABLE_INSETS_REGION;
                        out.touchableRegion.setEmpty();
                    }
                }
                return res;
            });

            // 5. onConfigureWindow: avoid fullscreen window when not in text input
            Method mConfigWin = imsCls.getDeclaredMethod(
                    "onConfigureWindow", Window.class, boolean.class, boolean.class);
            hook(mConfigWin).intercept(chain -> {
                InputMethodService svc = (InputMethodService) chain.getThisObject();
                if (isNoTextInput(svc)) {
                    Window win = (Window) chain.getArg(0);
                    if (win != null) {
                        win.setLayout(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
                    }
                    return null;
                }
                return chain.proceed();
            });

            // 6. Guard Android 14 switchInputMethod(id, subtype) pings when not in a text input
            try {
                Method mSwitchIme = InputMethodService.class.getMethod(
                        "switchInputMethod", String.class, InputMethodSubtype.class);
                hook(mSwitchIme).intercept(chain -> {
                    InputMethodService svc = (InputMethodService) chain.getThisObject();
                    if (isNoTextInput(svc)) {
                        return null;
                    }
                    return chain.proceed();
                });
            } catch (Throwable ignored) {
            }

            hooked = true;
            log(Log.INFO, TAG, "All Fcitx5 TV fix hooks installed successfully");
        } catch (Throwable t) {
            log(Log.ERROR, TAG, "Failed to install Fcitx5 TV fix hooks", t);
        }
    }

    private static boolean isNoTextInput(InputMethodService svc) {
        if (svc == null) return true;
        return isNoTextInput(svc.getCurrentInputEditorInfo());
    }

    private static boolean isNoTextInput(EditorInfo ei) {
        if (ei == null) return true;
        return (ei.inputType & InputType.TYPE_MASK_CLASS) == InputType.TYPE_NULL;
    }

    private static boolean shouldBypassKey(InputMethodService svc, int keyCode) {
        if (isSystemOrTvKey(keyCode)) {
            return true;
        }
        if (isNoTextInput(svc)) {
            return true;
        }
        if (keyCode == KeyEvent.KEYCODE_BACK && !svc.isInputViewShown()) {
            return true;
        }
        return false;
    }

    private static boolean isSystemOrTvKey(int keyCode) {
        switch (keyCode) {
            case KeyEvent.KEYCODE_HOME:
            case KeyEvent.KEYCODE_APP_SWITCH:
            case KeyEvent.KEYCODE_MENU:
            case KeyEvent.KEYCODE_SETTINGS:
            case KeyEvent.KEYCODE_POWER:
            case KeyEvent.KEYCODE_SLEEP:
            case KeyEvent.KEYCODE_WAKEUP:
            case KeyEvent.KEYCODE_VOLUME_UP:
            case KeyEvent.KEYCODE_VOLUME_DOWN:
            case KeyEvent.KEYCODE_VOLUME_MUTE:
            case KeyEvent.KEYCODE_TV_INPUT:
            case KeyEvent.KEYCODE_TV_POWER:
            case KeyEvent.KEYCODE_VOICE_ASSIST:
            case KeyEvent.KEYCODE_ASSIST:
            case KeyEvent.KEYCODE_UNKNOWN:
                return true;
            default:
                return false;
        }
    }
}
