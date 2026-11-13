package dev.gpui.beeper;

import android.app.Notification;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.IBinder;

/**
 * Keeps the process alive, and so connected, while the app is in the background. It does no work
 * itself: the connection lives in the native code that the activity started. Not sticky, because
 * without the activity there is nothing to keep connected.
 */
public final class SyncService extends Service {
    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        Notify.channels(this);
        Notification notification = new Notification.Builder(this, Notify.CONNECTION)
                .setSmallIcon(android.R.drawable.stat_notify_sync_noanim)
                .setContentTitle("Beeper")
                .setContentText("Connected for new messages")
                .setOngoing(true)
                .build();
        startForeground(1, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC);
        return START_NOT_STICKY;
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
