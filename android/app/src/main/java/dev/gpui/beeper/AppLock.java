package dev.gpui.beeper;

import android.app.Activity;
import android.app.KeyguardManager;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.hardware.biometrics.BiometricManager;
import android.hardware.biometrics.BiometricPrompt;
import android.os.Build;
import android.os.CancellationSignal;
import android.os.SystemClock;
import android.view.WindowManager;

/**
 * Screen lock and screen security. The Activity's window belongs to the native surface, so no
 * Java view can cover it: this class only decides when the app is locked and asks for the
 * credential, and Rust draws the lock screen from {@link #isLocked()}. The settings are mirrored
 * in preferences so a cold start is locked before Rust has read anything.
 * Lifecycle hooks and everything else run on the UI thread; Rust calls the static entry points.
 */
public final class AppLock {
    private static final String PREFS = "applock";
    private static final int REQUEST = 0x4c4b;

    private static volatile boolean locked;
    private static boolean booted;
    private static boolean lockOn;
    private static boolean secure;
    private static long timeoutMs;
    private static long pausedAt = -1;
    private static boolean authenticating;
    /** The credential screen of API 26 to 28 pauses the Activity; coming back is not "leaving". */
    private static boolean skipResume;
    private static CancellationSignal cancel;

    private AppLock() {}

    private static void load(Activity activity) {
        SharedPreferences p = activity.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
        lockOn = p.getBoolean("lock", false);
        secure = p.getBoolean("secure", false);
        timeoutMs = p.getLong("timeout_ms", 0);
    }

    private static void applyFlag(Activity activity, boolean on) {
        if (on) {
            activity.getWindow().setFlags(WindowManager.LayoutParams.FLAG_SECURE, WindowManager.LayoutParams.FLAG_SECURE);
        } else {
            activity.getWindow().clearFlags(WindowManager.LayoutParams.FLAG_SECURE);
        }
    }

    public static void onCreate(Activity activity) {
        load(activity);
        if (!booted) {
            booted = true;
            locked = lockOn;
        }
        applyFlag(activity, secure || locked);
    }

    /** FLAG_SECURE from here on blanks the recents thumbnail, which is taken after this. */
    public static void onPause(Activity activity) {
        pausedAt = SystemClock.elapsedRealtime();
        if (lockOn) {
            applyFlag(activity, true);
        }
        if (cancel != null) {
            cancel.cancel();
        }
    }

    public static void onResume(Activity activity) {
        load(activity);
        if (skipResume) {
            // Back from the credential screen, whatever it answered: no new prompt until the user asks.
            skipResume = false;
            pausedAt = -1;
            return;
        }
        if (!lockOn) {
            locked = false;
        } else if (pausedAt >= 0 && SystemClock.elapsedRealtime() - pausedAt >= timeoutMs) {
            locked = true;
        }
        pausedAt = -1;
        applyFlag(activity, secure || locked);
        if (locked) {
            authenticate(activity);
        }
    }

    public static void onResult(Activity activity, int requestCode, int resultCode) {
        if (requestCode != REQUEST) {
            return;
        }
        authenticating = false;
        if (resultCode == Activity.RESULT_OK) {
            unlocked(activity);
        }
    }

    public static boolean isLocked() {
        return locked;
    }

    /** Whether the device has a PIN, pattern, password or an enrolled biometric to unlock with. */
    public static boolean available(Activity activity) {
        if (Build.VERSION.SDK_INT >= 30) {
            BiometricManager m = activity.getSystemService(BiometricManager.class);
            return m != null
                    && m.canAuthenticate(BiometricManager.Authenticators.BIOMETRIC_WEAK
                            | BiometricManager.Authenticators.DEVICE_CREDENTIAL)
                            == BiometricManager.BIOMETRIC_SUCCESS;
        }
        KeyguardManager km = activity.getSystemService(KeyguardManager.class);
        return km != null && km.isDeviceSecure();
    }

    /** Stores the settings; called when Rust starts and whenever the user changes them. */
    public static void configure(Activity activity, boolean lock, int timeoutSeconds, boolean screenSecurity) {
        activity.runOnUiThread(() -> {
            activity.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
                    .putBoolean("lock", lock)
                    .putBoolean("secure", screenSecurity)
                    .putLong("timeout_ms", timeoutSeconds * 1000L)
                    .apply();
            load(activity);
            if (!lockOn) {
                locked = false;
            }
            applyFlag(activity, secure || locked);
        });
    }

    /** Shows the system prompt; the lock screen's button calls this after a cancelled prompt. */
    public static void authenticate(Activity activity) {
        activity.runOnUiThread(() -> prompt(activity));
    }

    private static void prompt(Activity activity) {
        if (authenticating || !locked) {
            return;
        }
        if (!available(activity)) {
            // The device lock was removed: there is nothing to check, so do not lock the user out.
            unlocked(activity);
            return;
        }
        authenticating = true;
        if (Build.VERSION.SDK_INT >= 29) {
            BiometricPrompt.Builder b = new BiometricPrompt.Builder(activity).setTitle("Unlock Murmur");
            if (Build.VERSION.SDK_INT >= 30) {
                b.setAllowedAuthenticators(BiometricManager.Authenticators.BIOMETRIC_WEAK
                        | BiometricManager.Authenticators.DEVICE_CREDENTIAL);
            } else {
                b.setDeviceCredentialAllowed(true);
            }
            CancellationSignal mine = new CancellationSignal();
            cancel = mine;
            b.build().authenticate(mine, activity.getMainExecutor(), new BiometricPrompt.AuthenticationCallback() {
                @Override
                public void onAuthenticationSucceeded(BiometricPrompt.AuthenticationResult result) {
                    if (cancel == mine) {
                        authenticating = false;
                        cancel = null;
                    }
                    unlocked(activity);
                }

                @Override
                public void onAuthenticationError(int code, CharSequence message) {
                    if (cancel == mine) {
                        authenticating = false;
                        cancel = null;
                    }
                }
            });
        } else {
            KeyguardManager km = activity.getSystemService(KeyguardManager.class);
            Intent intent = km.createConfirmDeviceCredentialIntent("Unlock Murmur", null);
            if (intent == null) {
                authenticating = false;
                unlocked(activity);
                return;
            }
            skipResume = true;
            activity.startActivityForResult(intent, REQUEST);
        }
    }

    private static void unlocked(Activity activity) {
        locked = false;
        applyFlag(activity, secure);
    }
}
