package dev.gpui.beeper;

import android.Manifest;
import android.app.Activity;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.os.Build;
import androidx.core.content.ContextCompat;

/** Message notifications and the background connection. All public methods are called from Rust. */
public final class Notify {
    static final String MESSAGES = "messages";
    static final String CONNECTION = "connection";

    private Notify() {}

    static void channels(Context context) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        manager.createNotificationChannel(
                new NotificationChannel(MESSAGES, "Messages", NotificationManager.IMPORTANCE_HIGH));
        NotificationChannel connection =
                new NotificationChannel(CONNECTION, "Background connection", NotificationManager.IMPORTANCE_MIN);
        connection.setShowBadge(false);
        manager.createNotificationChannel(connection);
    }

    /**
     * Asks for the notification permission where Android has one (13 and later) and starts the
     * service that keeps the app connected while it is in the background.
     */
    public static void start(Activity activity) {
        channels(activity);
        if (Build.VERSION.SDK_INT >= 33
                && activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                        != PackageManager.PERMISSION_GRANTED) {
            activity.requestPermissions(new String[] {Manifest.permission.POST_NOTIFICATIONS}, 1);
        }
        ContextCompat.startForegroundService(activity, new Intent(activity, SyncService.class));
    }

    public static void stop(Activity activity) {
        activity.stopService(new Intent(activity, SyncService.class));
        activity.getSystemService(NotificationManager.class).cancelAll();
    }

    /** One notification per chat, keyed by its ID: a newer message replaces the older one. */
    public static void message(Activity activity, String chat, String title, String text) {
        Intent open = new Intent(activity, activity.getClass())
                .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP | Intent.FLAG_ACTIVITY_CLEAR_TOP);
        PendingIntent tap = PendingIntent.getActivity(
                activity, 0, open, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        Notification notification = new Notification.Builder(activity, MESSAGES)
                .setSmallIcon(android.R.drawable.stat_notify_chat)
                .setContentTitle(title)
                .setContentText(text)
                .setStyle(new Notification.BigTextStyle().bigText(text))
                .setCategory(Notification.CATEGORY_MESSAGE)
                .setAutoCancel(true)
                .setContentIntent(tap)
                .build();
        activity.getSystemService(NotificationManager.class).notify(chat, 0, notification);
    }

    public static void clear(Activity activity, String chat) {
        activity.getSystemService(NotificationManager.class).cancel(chat, 0);
    }
}
