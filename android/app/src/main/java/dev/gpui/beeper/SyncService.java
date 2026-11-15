package dev.gpui.beeper;

import android.app.Notification;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.IBinder;
import androidx.core.app.ServiceCompat;

/**
 * Keeps the process alive, and so connected, while the app is in the background. It does no work
 * itself: the connection lives in the native code that the activity started. Not sticky, because
 * without the activity there is nothing to keep connected.
 *
 * <p>Its notice is posted once per run: Android 14 and later let the user swipe it away, and it
 * must not come back while the service goes on. {@link Notify#connect} starts the service only
 * when it is not running.
 */
public final class SyncService extends Service {
    /** From onCreate to onDestroy, on the main thread; read by {@link Notify#connect}. */
    static volatile boolean running;

    @Override
    public void onCreate() {
        super.onCreate();
        running = true;
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        Notify.channels(this);
        Notification notification = new Notification.Builder(this, Notify.CONNECTION)
                .setSmallIcon(android.R.drawable.stat_notify_sync_noanim)
                .setContentTitle("Murmur")
                .setContentText(getString(R.string.connected_for_new))
                .setOngoing(true)
                .build();
        // specialUse: a chat connection fits no other type, and unlike dataSync it has no daily limit.
        ServiceCompat.startForeground(this, 1, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE);
        return START_NOT_STICKY;
    }

    /**
     * Android 15 and later end time-limited foreground services after 6 hours a day in the
     * background, and crash the app if the service does not stop within seconds. It starts again
     * when the app is next in front, which resets the limit.
     */
    @Override
    public void onTimeout(int startId, int fgsType) {
        stopSelf();
    }

    @Override
    public void onDestroy() {
        running = false;
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
