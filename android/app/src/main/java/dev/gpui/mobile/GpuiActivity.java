package dev.gpui.mobile;

import android.app.NativeActivity;
import android.content.Intent;
import android.content.pm.ActivityInfo;
import android.content.pm.PackageManager;
import android.content.res.Configuration;
import android.media.AudioManager;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.window.BackEvent;
import android.window.OnBackAnimationCallback;
import android.window.OnBackInvokedDispatcher;
import android.util.Log;
import android.view.KeyEvent;
import android.view.WindowInsets;

import androidx.core.splashscreen.SplashScreen;

/**
 * Custom Activity extending NativeActivity that integrates with the
 * AndroidX SplashScreen API.
 *
 * On API 31+ the system splash screen is displayed automatically via theme
 * attributes. On API 26-30 the AndroidX compat library emulates the same
 * behavior using the theme's windowBackground drawable.
 *
 * The splash screen is held visible until the Rust native library signals
 * that initialization is complete by setting NATIVE_INITIALIZED to true
 * (see src/android/jni.rs). This prevents the user from seeing an empty
 * or partially-rendered surface during startup.
 *
 * Also handles:
 * - Deep link intents (onNewIntent)
 * - Volume key routing to the MUSIC audio stream
 * - Media button events via MediaSessionCompat
 */
public class GpuiActivity extends GpuiInputActivity {

    /** Whether the native .so has been loaded via System.loadLibrary. */
    private static volatile boolean sNativeLibLoaded = false;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        // Install the splash screen BEFORE calling super.onCreate().
        // This is required by the AndroidX SplashScreen API.
        SplashScreen splash = SplashScreen.installSplashScreen(this);

        // NativeActivity loads the .so via dlopen (loadNativeCode), which does
        // NOT register JNI symbols with the classloader. We must call
        // System.loadLibrary() ourselves so that JNI can resolve our native
        // methods. Reading the library name from the manifest meta-data ensures
        // we stay in sync with the nativeLibraryName placeholder.
        if (!sNativeLibLoaded) {
            try {
                ActivityInfo ai = getPackageManager().getActivityInfo(
                        getComponentName(), PackageManager.GET_META_DATA);
                String libName = ai.metaData.getString("android.app.lib_name");
                if (libName != null) {
                    System.loadLibrary(libName);
                    sNativeLibLoaded = true;
                }
            } catch (PackageManager.NameNotFoundException e) {
                // Shouldn't happen — we're querying our own activity.
            } catch (UnsatisfiedLinkError e) {
                // Library may already be loaded by NativeActivity; that's fine.
                sNativeLibLoaded = true;
            }
        }

        // Keep the splash screen visible until the native side signals readiness.
        splash.setKeepOnScreenCondition(() -> !isNativeReady());

        // Route volume keys to the MUSIC stream so they control media volume
        // rather than the ringer/notification volume.
        setVolumeControlStream(AudioManager.STREAM_MUSIC);

        super.onCreate(savedInstanceState);
        dev.gpui.beeper.AppLock.onCreate(this);
        dev.gpui.beeper.Lang.configured(this, getResources().getConfiguration());
        // Runs after every layout pass, which a change of insets brings; NativeActivity's own
        // listener reports only a content rect, and gpui-mobile does not repaint for it.
        getWindow().getDecorView().getViewTreeObserver().addOnGlobalLayoutListener(this::publishInsets);
        registerBack();
    }

    /**
     * Android 14's predictive back: the back gesture's progress reaches src/lib.rs as it happens, so
     * the open chat follows the finger as on iOS and slides back if the gesture is let go. Before
     * Android 14 back stays a key, which gpui-mobile turns into Escape.
     */
    private void registerBack() {
        if (Build.VERSION.SDK_INT < 34) return;
        getOnBackInvokedDispatcher().registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT, new OnBackAnimationCallback() {
            @Override
            public void onBackStarted(BackEvent e) {
                back(BACK_STARTED, e);
            }

            @Override
            public void onBackProgressed(BackEvent e) {
                back(BACK_MOVED, e);
            }

            @Override
            public void onBackInvoked() {
                back(BACK_INVOKED, null);
            }

            @Override
            public void onBackCancelled() {
                back(BACK_CANCELED, null);
            }
        });
    }

    private static final int BACK_STARTED = 0, BACK_MOVED = 1, BACK_INVOKED = 2, BACK_CANCELED = 3;

    private void back(int phase, BackEvent e) {
        try {
            if (e == null) {
                nativeBack(phase, 0f, 0f, false);
            } else {
                nativeBack(phase, e.getProgress(), e.getTouchX(), e.getSwipeEdge() == BackEvent.EDGE_LEFT);
            }
        } catch (UnsatisfiedLinkError err) {
            Log.w("GpuiActivity", "nativeBack not available yet");
        }
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus) publishInsets();
    }

    private int publishedBottom = -1;

    /** Tells src/lib.rs the bottom inset, the keyboard or the navigation bar, whichever is taller. */
    private void publishInsets() {
        WindowInsets insets = getWindow().getDecorView().getRootWindowInsets();
        if (insets == null) return;
        int bottom = Build.VERSION.SDK_INT >= Build.VERSION_CODES.R
                ? insets.getInsets(WindowInsets.Type.navigationBars() | WindowInsets.Type.ime()).bottom
                // Before Android 11, with adjustResize, the keyboard is part of the system window insets.
                : insets.getSystemWindowInsetBottom();
        if (bottom == publishedBottom) return;
        publishedBottom = bottom;
        nativeInsets(bottom);
    }

    /** The manifest keeps the activity through a change of language, so the app redraws in place. */
    @Override
    public void onConfigurationChanged(Configuration newConfig) {
        super.onConfigurationChanged(newConfig);
        dev.gpui.beeper.Lang.configured(this, newConfig);
    }

    @Override
    protected void onPause() {
        dev.gpui.beeper.AppLock.onPause(this);
        super.onPause();
    }

    @Override
    protected void onResume() {
        super.onResume();
        dev.gpui.beeper.AppLock.onResume(this);
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        dev.gpui.beeper.AppLock.onResult(this, requestCode, resultCode);
    }

    /**
     * Check if the native library is fully initialized.
     * Returns false if the .so hasn't been loaded yet or if
     * NATIVE_INITIALIZED hasn't been set to true.
     */
    private boolean isNativeReady() {
        if (!sNativeLibLoaded) {
            return false;
        }
        try {
            return nativeIsInitialized();
        } catch (UnsatisfiedLinkError e) {
            return false;
        }
    }

    /**
     * Intercept key events to handle volume and media buttons.
     *
     * NativeActivity normally forwards ALL key events to the native side,
     * which means volume keys would be consumed by the Rust event loop
     * without actually adjusting the system volume. We intercept them here
     * and let the system handle them instead.
     */
    @Override
    public boolean dispatchKeyEvent(KeyEvent event) {
        int keyCode = event.getKeyCode();
        switch (keyCode) {
            case KeyEvent.KEYCODE_VOLUME_UP:
            case KeyEvent.KEYCODE_VOLUME_DOWN:
            case KeyEvent.KEYCODE_VOLUME_MUTE:
                // Let the system handle volume keys (adjusts STREAM_MUSIC).
                // Don't pass to NativeActivity's native input handler.
                return super.dispatchKeyEvent(event);

            case KeyEvent.KEYCODE_MEDIA_PLAY:
            case KeyEvent.KEYCODE_MEDIA_PAUSE:
            case KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE:
            case KeyEvent.KEYCODE_MEDIA_NEXT:
            case KeyEvent.KEYCODE_MEDIA_PREVIOUS:
            case KeyEvent.KEYCODE_MEDIA_STOP:
            case KeyEvent.KEYCODE_HEADSETHOOK:
                // Route media buttons through the MediaSession.
                // MediaButtonReceiver will dispatch to our session callback.
                return super.dispatchKeyEvent(event);

            default:
                return super.dispatchKeyEvent(event);
        }
    }

    @Override
    protected void onDestroy() {
        // Release media session when activity is destroyed.
        GpuiMediaSession.release();
        super.onDestroy();
    }

    /**
     * Handle new intents delivered to this singleTask activity.
     *
     * When the app is already running and a deeplink is opened
     * (e.g. `adb shell am start -d gpui://video_player`), this method
     * receives the new intent. We update the activity's intent and
     * notify the Rust side via JNI.
     */
    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);

        Uri data = intent.getData();
        if (data != null) {
            String url = data.toString();
            Log.i("GpuiActivity", "onNewIntent deeplink");
            try {
                nativeOnDeepLink(url);
            } catch (UnsatisfiedLinkError e) {
                Log.w("GpuiActivity", "nativeOnDeepLink not available yet");
            }
        }
    }

    /**
     * JNI bridge to check if the Rust NATIVE_INITIALIZED flag is set.
     */
    private static native boolean nativeIsInitialized();

    /**
     * JNI bridge to notify Rust of an incoming deeplink URL.
     */
    private static native void nativeOnDeepLink(String url);

    private static native void nativeInsets(int bottom);

    private static native void nativeBack(int phase, float progress, float touchX, boolean fromLeft);
}
