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
import android.util.Log;
import android.view.WindowManager;

/**
 * App lock and screen security. The Activity's window belongs to the native surface, so no
 * Java view can cover it: this class only decides when the app is locked and, for the phone lock,
 * asks for the credential; Rust draws the lock screen from {@link #isLocked()} and checks the app
 * PIN itself, then calls {@link #unlock}. The settings are mirrored in preferences so a cold start
 * is locked before Rust has read anything.
 * Lifecycle hooks and everything else run on the UI thread; Rust calls the static entry points.
 */
public final class AppLock {
    private static final String TAG = "AppLock";
    private static final String PREFS = "applock";
    private static final int REQUEST = 0x4c4b;
    private static final int REQUEST_CONFIRM = 0x4c4c;
    /** The same numbers as `LockMode` in src/applock.rs. */
    private static final int OFF = 0;
    private static final int PHONE = 1;
    /** What {@link #takeConfirm()} answers. */
    private static final int NONE = 0;
    private static final int WAITING = 1;
    private static final int PASSED = 2;
    private static final int FAILED = 3;

    private static volatile boolean locked;
    private static boolean booted;
    private static int mode;
    private static boolean secure;
    private static long timeoutMs;
    private static long pausedAt = -1;
    private static boolean authenticating;
    /** The credential screen of API 26 to 28 pauses the Activity; coming back is not "leaving". */
    private static boolean skipResume;
    private static CancellationSignal cancel;
    /** A check of the phone lock asked for by the settings, outside the lock screen. */
    private static int confirmed = NONE;

    private AppLock() {}

    private static void load(Activity activity) {
        SharedPreferences p = activity.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
        mode = p.getInt("mode", OFF);
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
            locked = mode != OFF;
        }
        applyFlag(activity, secure || locked);
    }

    /** FLAG_SECURE from here on blanks the recents thumbnail, which is taken after this. */
    public static void onPause(Activity activity) {
        pausedAt = SystemClock.elapsedRealtime();
        if (mode != OFF) {
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
        if (mode == OFF) {
            locked = false;
        } else if (pausedAt >= 0 && SystemClock.elapsedRealtime() - pausedAt >= timeoutMs) {
            locked = true;
        }
        pausedAt = -1;
        applyFlag(activity, secure || locked);
        if (locked && mode == PHONE) {
            ask(activity, false);
        }
    }

    public static void onResult(Activity activity, int requestCode, int resultCode) {
        if (requestCode == REQUEST) {
            authenticating = false;
            if (resultCode == Activity.RESULT_OK) {
                unlocked(activity);
            }
        } else if (requestCode == REQUEST_CONFIRM) {
            authenticating = false;
            setConfirmed(resultCode == Activity.RESULT_OK ? PASSED : FAILED);
        }
    }

    public static boolean isLocked() {
        return locked;
    }

    /** Whether the device has a PIN, pattern, password or an enrolled biometric to unlock with. */
    public static boolean available(Activity activity) {
        if (Build.VERSION.SDK_INT >= 30) {
            BiometricManager m = activity.getSystemService(BiometricManager.class);
            try {
                return m != null
                        && m.canAuthenticate(BiometricManager.Authenticators.BIOMETRIC_WEAK
                                | BiometricManager.Authenticators.DEVICE_CREDENTIAL)
                                == BiometricManager.BIOMETRIC_SUCCESS;
            } catch (SecurityException e) {
                Log.w(TAG, "canAuthenticate", e);
                return false;
            }
        }
        KeyguardManager km = activity.getSystemService(KeyguardManager.class);
        return km != null && km.isDeviceSecure();
    }

    /** Stores the settings; called when Rust starts and whenever the user changes them. */
    public static void configure(Activity activity, int lockMode, int timeoutSeconds, boolean screenSecurity) {
        activity.runOnUiThread(() -> {
            activity.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
                    .remove("lock")
                    .putInt("mode", lockMode)
                    .putBoolean("secure", screenSecurity)
                    .putLong("timeout_ms", timeoutSeconds * 1000L)
                    .apply();
            load(activity);
            if (mode == OFF) {
                locked = false;
            }
            applyFlag(activity, secure || locked);
        });
    }

    /** Shows the system prompt; the lock screen's button calls this after a canceled prompt. */
    public static void authenticate(Activity activity) {
        activity.runOnUiThread(() -> ask(activity, false));
    }

    /** Rust has checked the app PIN. */
    public static void unlock(Activity activity) {
        activity.runOnUiThread(() -> unlocked(activity));
    }

    /** Asks for the phone lock without locking anything; the answer comes from {@link #takeConfirm()}. */
    public static void confirm(Activity activity) {
        setConfirmed(WAITING);
        activity.runOnUiThread(() -> ask(activity, true));
    }

    /** 0 nothing asked, 1 waiting, 2 passed, 3 failed or canceled; an answer is given once. */
    public static synchronized int takeConfirm() {
        int answer = confirmed;
        if (answer == PASSED || answer == FAILED) {
            confirmed = NONE;
        }
        return answer;
    }

    private static synchronized void setConfirmed(int answer) {
        confirmed = answer;
    }

    private static void ask(Activity activity, boolean confirm) {
        if (authenticating) {
            if (confirm) {
                setConfirmed(FAILED);
            }
            return;
        }
        if (!confirm && (!locked || mode != PHONE)) {
            return;
        }
        if (!available(activity)) {
            if (confirm) {
                setConfirmed(FAILED);
            } else {
                // The device lock was removed: there is nothing to check, so do not lock the user out.
                unlocked(activity);
            }
            return;
        }
        authenticating = true;
        String title = activity.getString(confirm ? R.string.confirm_lock : R.string.unlock_app);
        if (Build.VERSION.SDK_INT >= 29) {
            BiometricPrompt.Builder b = new BiometricPrompt.Builder(activity).setTitle(title);
            if (Build.VERSION.SDK_INT >= 30) {
                b.setAllowedAuthenticators(BiometricManager.Authenticators.BIOMETRIC_WEAK
                        | BiometricManager.Authenticators.DEVICE_CREDENTIAL);
            } else {
                b.setDeviceCredentialAllowed(true);
            }
            CancellationSignal mine = new CancellationSignal();
            cancel = mine;
            try {
                b.build().authenticate(mine, activity.getMainExecutor(), new BiometricPrompt.AuthenticationCallback() {
                    @Override
                    public void onAuthenticationSucceeded(BiometricPrompt.AuthenticationResult result) {
                        if (cancel == mine) {
                            authenticating = false;
                            cancel = null;
                        }
                        if (confirm) {
                            setConfirmed(PASSED);
                        } else {
                            unlocked(activity);
                        }
                    }

                    @Override
                    public void onAuthenticationError(int code, CharSequence message) {
                        if (cancel == mine) {
                            authenticating = false;
                            cancel = null;
                        }
                        if (confirm) {
                            setConfirmed(FAILED);
                        }
                    }
                });
            } catch (RuntimeException e) {
                Log.w(TAG, "BiometricPrompt", e);
                authenticating = false;
                cancel = null;
                if (confirm) {
                    setConfirmed(FAILED);
                }
            }
        } else {
            KeyguardManager km = activity.getSystemService(KeyguardManager.class);
            Intent intent = km.createConfirmDeviceCredentialIntent(title, null);
            if (intent == null) {
                authenticating = false;
                if (confirm) {
                    setConfirmed(FAILED);
                } else {
                    unlocked(activity);
                }
                return;
            }
            skipResume = true;
            activity.startActivityForResult(intent, confirm ? REQUEST_CONFIRM : REQUEST);
        }
    }

    private static void unlocked(Activity activity) {
        locked = false;
        applyFlag(activity, secure);
    }
}
