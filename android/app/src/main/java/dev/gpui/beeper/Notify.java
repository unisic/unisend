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
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.os.Build;
import android.os.Bundle;
import android.provider.Settings;
import android.service.notification.StatusBarNotification;
import androidx.core.app.NotificationCompat;
import androidx.core.app.Person;
import androidx.core.app.RemoteInput;
import androidx.core.content.ContextCompat;
import androidx.core.graphics.drawable.IconCompat;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * Message notifications and the background connection. Public static methods are called from Rust
 * over JNI; {@link NotifyReceiver} calls the ones taking a Context.
 *
 * <p>A chat has one notification, a conversation: the newest messages, who wrote each, the chat's
 * picture, and a reply box and a mark-as-read button that work without opening the app. The
 * conversation is not kept anywhere else: a new message reads it back from the notification
 * Android is showing, so a dismissed chat starts afresh.
 */
public final class Notify {
    static final String MESSAGES = "messages";
    static final String GROUPS = "groups";
    static final String MENTIONS = "mentions";
    static final String CONNECTION = "connection";
    static final String EXTRA_CHAT = "chat";
    static final String ACTION_REPLY = "dev.gpui.beeper.REPLY";
    static final String ACTION_READ = "dev.gpui.beeper.READ";
    static final String KEY_REPLY = "reply";
    private static final String GROUP = "dev.gpui.beeper.MESSAGES";
    private static final String SUMMARY = "summary";
    /** Messages a conversation notification keeps. */
    private static final int KEPT = 8;
    /** Chat pictures kept for replies that rebuild a notification, 96 px squares. */
    private static final int ICONS = 16;

    private static final Person ME = new Person.Builder().setName("You").setKey("me").build();
    private static final Map<String, Bitmap> icons = new LinkedHashMap<String, Bitmap>(ICONS, 0.75f, true) {
        @Override
        protected boolean removeEldestEntry(Map.Entry<String, Bitmap> eldest) {
            return size() > ICONS;
        }
    };

    private Notify() {}

    /** Set by the native library when it is loaded; these run in the app's process. */
    private static native boolean nativeReply(String chat, String text);

    private static native void nativeRead(String chat);

    static void channels(Context context) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        manager.createNotificationChannel(channel(MESSAGES, "Direct messages", NotificationManager.IMPORTANCE_HIGH));
        manager.createNotificationChannel(channel(GROUPS, "Group chats", NotificationManager.IMPORTANCE_DEFAULT));
        manager.createNotificationChannel(channel(MENTIONS, "Mentions", NotificationManager.IMPORTANCE_HIGH));
        NotificationChannel connection = channel(CONNECTION, "Background connection", NotificationManager.IMPORTANCE_MIN);
        connection.setShowBadge(false);
        manager.createNotificationChannel(connection);
    }

    private static NotificationChannel channel(String id, String name, int importance) {
        NotificationChannel channel = new NotificationChannel(id, name, importance);
        if (importance >= NotificationManager.IMPORTANCE_DEFAULT) {
            channel.setShowBadge(true);
        }
        return channel;
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

    /** The system's page for this app's notification channels: sounds, vibration, importance. */
    public static void settings(Activity activity) {
        activity.startActivity(new Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
                .putExtra(Settings.EXTRA_APP_PACKAGE, activity.getPackageName()));
    }

    /**
     * The chat a tapped notification asked for, once: null when the app was not opened from one.
     * The activity's intent is the one the notification carried (GpuiActivity keeps it on
     * onNewIntent).
     */
    public static String takeChat(Activity activity) {
        Intent intent = activity.getIntent();
        if (intent == null) {
            return null;
        }
        String chat = intent.getStringExtra(EXTRA_CHAT);
        intent.removeExtra(EXTRA_CHAT);
        return chat;
    }

    /**
     * Adds a message to the chat's conversation notification. {@code alert} is false for all but
     * the last of messages that arrive together, so they make one sound. {@code icon} is the path
     * of the chat's picture, or empty.
     */
    public static void message(Context any, String chat, String name, boolean group, String sender,
            String text, long time, boolean mention, String icon, boolean alert) {
        Context context = any.getApplicationContext();
        channels(context);
        Bitmap picture = icon.isEmpty() ? cached(chat) : load(chat, icon);
        // In a one-to-one chat the picture is the sender's; in a group it is the chat's.
        Person person = new Person.Builder().setName(sender).setKey(sender).build();
        if (!group && picture != null) {
            person = new Person.Builder().setName(sender).setKey(sender).setIcon(IconCompat.createWithBitmap(picture)).build();
        }
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        NotificationCompat.MessagingStyle old = conversation(manager, chat);
        NotificationCompat.MessagingStyle style = extend(old, group ? name : null, group,
                new NotificationCompat.MessagingStyle.Message(text, time, person));
        // Once a chat has been mentioned in, its notification stays on the mentions channel until
        // it is read, so the sound of a later message is the mention's.
        boolean mentioned = mention || (active(manager, chat) != null
                && MENTIONS.equals(active(manager, chat).getNotification().getChannelId()));
        String channel = mentioned ? MENTIONS : group ? GROUPS : MESSAGES;
        post(context, manager, chat, name, channel, picture, style, text, time, alert);
    }

    /** Takes the chat's notification away, because it was read. */
    public static void clear(Activity activity, String chat) {
        Context context = activity.getApplicationContext();
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        manager.cancel(chat, 0);
        summarize(context, manager, chat, false);
    }

    /** A reply typed into a notification. */
    static void reply(Context context, String chat, CharSequence text) {
        boolean sent = false;
        try {
            sent = nativeReply(chat, text.toString());
        } catch (UnsatisfiedLinkError e) {
            // The app's process was gone and this one was started for the receiver alone.
        }
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        NotificationCompat.MessagingStyle old = conversation(manager, chat);
        StatusBarNotification shown = active(manager, chat);
        if (old == null || shown == null) {
            return;
        }
        // Updating the notification is what stops its reply box from spinning.
        long now = System.currentTimeMillis();
        NotificationCompat.MessagingStyle.Message line = sent
                ? new NotificationCompat.MessagingStyle.Message(text, now, old.getUser())
                : new NotificationCompat.MessagingStyle.Message("Not sent. Open Beeper and try again.", now,
                        new Person.Builder().setName("Beeper").setKey("beeper").build());
        NotificationCompat.MessagingStyle style = extend(old, old.getConversationTitle() == null ? null
                : old.getConversationTitle().toString(), old.isGroupConversation(), line);
        Bundle extras = shown.getNotification().extras;
        CharSequence title = extras.getCharSequence(Notification.EXTRA_TITLE);
        post(context, manager, chat, title == null ? "" : title.toString(), shown.getNotification().getChannelId(),
                cached(chat), style, line.getText().toString(), now, false);
    }

    /** Marks the chat read from its notification. */
    static void read(Context context, String chat) {
        try {
            nativeRead(chat);
        } catch (UnsatisfiedLinkError e) {
            // Without the app running the chat stays unread; the notification still goes away.
        }
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        manager.cancel(chat, 0);
        summarize(context, manager, chat, false);
    }

    private static NotificationCompat.MessagingStyle extend(NotificationCompat.MessagingStyle old, String title,
            boolean group, NotificationCompat.MessagingStyle.Message add) {
        NotificationCompat.MessagingStyle style = new NotificationCompat.MessagingStyle(old == null ? ME : old.getUser());
        style.setConversationTitle(title).setGroupConversation(group);
        if (old != null) {
            List<NotificationCompat.MessagingStyle.Message> before = old.getMessages();
            for (int i = Math.max(0, before.size() - (KEPT - 1)); i < before.size(); i++) {
                style.addMessage(before.get(i));
            }
        }
        return style.addMessage(add);
    }

    private static void post(Context context, NotificationManager manager, String chat, String title, String channel,
            Bitmap picture, NotificationCompat.MessagingStyle style, String last, long time, boolean alert) {
        int code = chat.hashCode();
        Intent open = new Intent(context, activityClass(context))
                .putExtra(EXTRA_CHAT, chat)
                .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP | Intent.FLAG_ACTIVITY_CLEAR_TOP);
        PendingIntent tap = PendingIntent.getActivity(
                context, code, open, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);

        RemoteInput input = new RemoteInput.Builder(KEY_REPLY).setLabel("Reply").build();
        PendingIntent reply = PendingIntent.getBroadcast(context, code, receiver(context, ACTION_REPLY, chat),
                PendingIntent.FLAG_MUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        PendingIntent read = PendingIntent.getBroadcast(context, code, receiver(context, ACTION_READ, chat),
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);

        NotificationCompat.Builder builder = new NotificationCompat.Builder(context, channel)
                .setSmallIcon(android.R.drawable.stat_notify_chat)
                .setContentTitle(title)
                .setContentText(last)
                .setStyle(style)
                .setCategory(NotificationCompat.CATEGORY_MESSAGE)
                .setGroup(GROUP)
                .setWhen(time)
                .setShowWhen(true)
                .setAutoCancel(true)
                .setSilent(!alert)
                .setContentIntent(tap)
                .addAction(new NotificationCompat.Action.Builder(android.R.drawable.ic_menu_send, "Reply", reply)
                        .addRemoteInput(input)
                        .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_REPLY)
                        .setShowsUserInterface(false)
                        .setAllowGeneratedReplies(true)
                        .build())
                .addAction(new NotificationCompat.Action.Builder(android.R.drawable.ic_menu_view, "Mark as read", read)
                        .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_MARK_AS_READ)
                        .setShowsUserInterface(false)
                        .build());
        if (picture != null) {
            builder.setLargeIcon(picture);
        }
        manager.notify(chat, 0, builder.build());
        summarize(context, manager, chat, true);
    }

    /**
     * The line the chats fold under when two or more are waiting (Android hides it while there is
     * one). It stays while any chat is, because Android cancels a summary's chats with it; it goes
     * when the last chat does.
     */
    private static void summarize(Context context, NotificationManager manager, String chat, boolean includes) {
        int chats = includes ? 1 : 0;
        for (StatusBarNotification shown : manager.getActiveNotifications()) {
            if (shown.getId() == 0 && shown.getTag() != null && !shown.getTag().equals(chat)
                    && !shown.getTag().equals(SUMMARY) && GROUP.equals(shown.getNotification().getGroup())) {
                chats++;
            }
        }
        if (chats == 0) {
            manager.cancel(SUMMARY, 0);
            return;
        }
        PendingIntent open = PendingIntent.getActivity(context, 0, new Intent(context, activityClass(context))
                .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP | Intent.FLAG_ACTIVITY_CLEAR_TOP),
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        manager.notify(SUMMARY, 0, new NotificationCompat.Builder(context, MESSAGES)
                .setSmallIcon(android.R.drawable.stat_notify_chat)
                .setContentTitle("Beeper")
                .setContentText(chats == 1 ? "1 chat has new messages" : chats + " chats have new messages")
                .setGroup(GROUP)
                .setGroupSummary(true)
                // The chats alert on their own; the line over them stays quiet.
                .setGroupAlertBehavior(NotificationCompat.GROUP_ALERT_CHILDREN)
                .setAutoCancel(true)
                .setContentIntent(open)
                .build());
    }

    private static Intent receiver(Context context, String action, String chat) {
        return new Intent(context, NotifyReceiver.class).setAction(action).putExtra(EXTRA_CHAT, chat);
    }

    private static Class<?> activityClass(Context context) {
        try {
            return Class.forName(context.getPackageManager().getLaunchIntentForPackage(context.getPackageName())
                    .getComponent().getClassName());
        } catch (ClassNotFoundException | NullPointerException e) {
            throw new IllegalStateException("no launcher activity", e);
        }
    }

    private static StatusBarNotification active(NotificationManager manager, String chat) {
        for (StatusBarNotification shown : manager.getActiveNotifications()) {
            if (shown.getId() == 0 && chat.equals(shown.getTag())) {
                return shown;
            }
        }
        return null;
    }

    private static NotificationCompat.MessagingStyle conversation(NotificationManager manager, String chat) {
        StatusBarNotification shown = active(manager, chat);
        return shown == null ? null
                : NotificationCompat.MessagingStyle.extractMessagingStyleFromNotification(shown.getNotification());
    }

    private static Bitmap load(String chat, String path) {
        Bitmap bitmap = BitmapFactory.decodeFile(path);
        if (bitmap != null) {
            synchronized (icons) {
                icons.put(chat, bitmap);
            }
        }
        return bitmap;
    }

    private static Bitmap cached(String chat) {
        synchronized (icons) {
            return icons.get(chat);
        }
    }
}
