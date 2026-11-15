package dev.gpui.beeper;

import android.app.Activity;
import android.app.PendingIntent;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageInfo;
import android.content.pm.PackageInstaller;
import android.content.pm.PackageManager;
import android.net.Uri;
import android.os.Build;
import android.provider.Settings;
import android.util.Log;
import java.io.File;
import java.io.FileInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;

/**
 * Installs an update of this app that the native side downloaded and checked, through a
 * PackageInstaller session. Android itself refuses an APK signed with another key or with a
 * lower version code. The system's answer arrives in {@link #onReceive}.
 */
public final class Update extends BroadcastReceiver {
    private static final String TAG = "Unisend";

    /** Set by the native library when it is loaded; runs in the app's process. */
    private static native void nativeFailed(int status, String message);

    /** The installed version code, which a release must exceed. */
    public static long versionCode(Activity activity) {
        try {
            PackageInfo info = activity.getPackageManager().getPackageInfo(activity.getPackageName(), 0);
            return Build.VERSION.SDK_INT >= 28 ? info.getLongVersionCode() : info.versionCode;
        } catch (PackageManager.NameNotFoundException e) {
            return 0;
        }
    }

    /** Whether the user let this app install apps ("Install unknown apps"). */
    public static boolean canInstall(Activity activity) {
        return activity.getPackageManager().canRequestPackageInstalls();
    }

    /** Opens the "Install unknown apps" switch for this app alone. */
    public static void allowInstall(Activity activity) {
        Uri app = Uri.parse("package:" + activity.getPackageName());
        activity.startActivity(new Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, app));
    }

    /** Copies the APK at {@code path} into a new install session and commits it; null when committed, else why not. */
    public static String install(Activity activity, String path) {
        Context context = activity.getApplicationContext();
        PackageInstaller installer = context.getPackageManager().getPackageInstaller();
        PackageInstaller.SessionParams params = new PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL);
        params.setAppPackageName(context.getPackageName());
        // Honoured only once this app is the installer of record; before that the system asks.
        if (Build.VERSION.SDK_INT >= 31) {
            params.setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_NOT_REQUIRED);
        }
        File apk = new File(path);
        params.setSize(apk.length());
        int id = -1;
        boolean committed = false;
        try {
            id = installer.createSession(params);
            try (PackageInstaller.Session session = installer.openSession(id)) {
                try (InputStream in = new FileInputStream(apk); OutputStream out = session.openWrite("unisend.apk", 0, apk.length())) {
                    byte[] buffer = new byte[64 * 1024];
                    int n;
                    while ((n = in.read(buffer)) > 0) {
                        out.write(buffer, 0, n);
                    }
                    session.fsync(out);
                }
                // Mutable: the installer adds the status to it. Explicit, so only this receiver gets it.
                int flags = PendingIntent.FLAG_UPDATE_CURRENT | (Build.VERSION.SDK_INT >= 31 ? PendingIntent.FLAG_MUTABLE : 0);
                PendingIntent answer = PendingIntent.getBroadcast(context, id, new Intent(context, Update.class), flags);
                session.commit(answer.getIntentSender());
                committed = true;
            }
            return null;
        } catch (IOException | RuntimeException e) {
            Log.w(TAG, "update: " + e);
            if (id != -1 && !committed) {
                installer.abandonSession(id);
            }
            return e.getClass().getSimpleName() + ": " + e.getMessage();
        }
    }

    @Override
    public void onReceive(Context context, Intent intent) {
        int status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE);
        if (status == PackageInstaller.STATUS_PENDING_USER_ACTION) {
            Intent confirm = intent.getParcelableExtra(Intent.EXTRA_INTENT);
            if (confirm != null) {
                confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
                context.startActivity(confirm);
            }
            return;
        }
        if (status == PackageInstaller.STATUS_SUCCESS) {
            return;
        }
        String message = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE);
        Log.w(TAG, "update not installed: " + status + " " + message);
        try {
            nativeFailed(status, message == null ? "" : message);
        } catch (UnsatisfiedLinkError e) {
            // The app's process was gone and this one was started for the receiver alone.
        }
    }
}
