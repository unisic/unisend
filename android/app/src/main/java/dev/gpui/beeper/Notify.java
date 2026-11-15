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
import android.graphics.BitmapShader;
import android.graphics.Canvas;
import android.graphics.Matrix;
import android.graphics.Paint;
import android.graphics.Shader;
import android.os.Build;
import android.os.Bundle;
import android.provider.Settings;
import android.service.notification.StatusBarNotification;
import android.util.Log;
import androidx.core.app.NotificationCompat;
import androidx.core.app.Person;
import androidx.core.app.RemoteInput;
import androidx.core.content.ContextCompat;
import androidx.core.content.pm.ShortcutInfoCompat;
import androidx.core.content.pm.ShortcutManagerCompat;
import androidx.core.graphics.drawable.IconCompat;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * Message notifications and the background connection. Public static methods are called from Rust
 * over JNI; {@link NotifyReceiver} calls the ones taking a Context.
 *
 * <p>A chat has one notification, a conversation: the newest messages, who wrote each, the chat's
 * picture, and a reply box, a mark-as-read button and a mute button that work without opening the app. The
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
    static final String ACTION_MUTE = "dev.gpui.beeper.MUTE";
    static final String KEY_REPLY = "reply";
    private static final String GROUP = "dev.gpui.beeper.MESSAGES";
    private static final String SUMMARY = "summary";
    /** Messages a conversation notification keeps. */
    private static final int KEPT = 8;
    /** Chat pictures kept for replies that rebuild a notification, {@link #AVATAR} px circles. */
    private static final int ICONS = 16;
    /**
     * Side of the round pictures handed to Android, in px: the stored ones are 96 px squares, so
     * more would only enlarge them. 37 KB each.
     */
    static final int AVATAR = 96;

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

    private static native boolean nativeMute(String chat);

    static void channels(Context context) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        manager.createNotificationChannel(channel(MESSAGES, context.getString(R.string.channel_direct), NotificationManager.IMPORTANCE_HIGH));
        manager.createNotificationChannel(channel(GROUPS, context.getString(R.string.channel_groups), NotificationManager.IMPORTANCE_DEFAULT));
        manager.createNotificationChannel(channel(MENTIONS, context.getString(R.string.channel_mentions), NotificationManager.IMPORTANCE_HIGH));
        NotificationChannel connection = channel(CONNECTION, context.getString(R.string.channel_connection), NotificationManager.IMPORTANCE_MIN);
        connection.setShowBadge(false);
        manager.createNotificationChannel(connection);
    }

    /**
     * After a change of language: the channels' names, the waiting chats' buttons and summary, and
     * the connection's notice, posted again quietly. A notice the user swiped away stays away.
     */
    static void relocale(Context context) {
        channels(context);
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        for (StatusBarNotification shown : manager.getActiveNotifications()) {
            String chat = shown.getTag();
            if (chat == null && shown.getId() == SyncService.NOTICE) {
                manager.notify(SyncService.NOTICE, SyncService.notice(context));
                continue;
            }
            if (chat == null || shown.getId() != 0 || chat.equals(SUMMARY)) {
                continue;
            }
            repost(context, manager, shown);
        }
        summarize(context, manager, null, false);
    }

    /** Posts a chat's notification again, quietly, from what Android is showing. */
    private static void repost(Context context, NotificationManager manager, StatusBarNotification shown) {
        Notification n = shown.getNotification();
        NotificationCompat.MessagingStyle style = NotificationCompat.MessagingStyle.extractMessagingStyleFromNotification(n);
        if (style != null) {
            post(context, manager, shown.getTag(), text(n, Notification.EXTRA_TITLE), n.getChannelId(), cached(shown.getTag()), style,
                    text(n, Notification.EXTRA_TEXT), n.when, false);
        }
    }

    private static String text(Notification n, String key) {
        CharSequence text = n.extras.getCharSequence(key);
        return text == null ? "" : text.toString();
    }

    private static NotificationChannel channel(String id, String name, int importance) {
        NotificationChannel channel = new NotificationChannel(id, name, importance);
        if (importance >= NotificationManager.IMPORTANCE_DEFAULT) {
            channel.setShowBadge(true);
            // Android takes this only when a channel is first made; each notice also carries it.
            channel.setLockscreenVisibility(Notification.VISIBILITY_PRIVATE);
        }
        return channel;
    }

    /**
     * Asks for the notification permission where Android has one (13 and later). Messages are
     * shown without the background connection too, while the app runs.
     */
    public static void start(Activity activity) {
        channels(activity);
        if (Build.VERSION.SDK_INT >= 33
                && activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                        != PackageManager.PERMISSION_GRANTED) {
            activity.requestPermissions(new String[] {Manifest.permission.POST_NOTIFICATIONS}, 1);
        }
    }

    /**
     * Starts the service that keeps the app connected in the background, unless it runs: starting
     * it again would post its notice again after the user swiped it away. Called while the app is
     * in front, which Android requires for starting it.
     */
    public static void connect(Activity activity) {
        if (SyncService.running) {
            return;
        }
        channels(activity);
        try {
            ContextCompat.startForegroundService(activity, new Intent(activity, SyncService.class));
        } catch (IllegalStateException e) {
            // Refused in the background (12 and later); the next time in front tries again.
            Log.w("Notify", "background connection not started", e);
        }
    }

    /** Ends the background connection; message notifications stay. */
    public static void disconnect(Activity activity) {
        activity.stopService(new Intent(activity, SyncService.class));
    }

    /** Signed out: no background connection, no notifications, and no chats left in share sheets or the launcher. */
    public static void stop(Activity activity) {
        disconnect(activity);
        activity.getSystemService(NotificationManager.class).cancelAll();
        try {
            List<String> ids = new ArrayList<>();
            for (ShortcutInfoCompat s : ShortcutManagerCompat.getShortcuts(activity,
                    ShortcutManagerCompat.FLAG_MATCH_DYNAMIC | ShortcutManagerCompat.FLAG_MATCH_CACHED)) {
                ids.add(s.getId());
            }
            ShortcutManagerCompat.removeLongLivedShortcuts(activity, ids);
            ShortcutManagerCompat.removeAllDynamicShortcuts(activity);
        } catch (IllegalStateException | IllegalArgumentException e) {
            Log.w("Notify", "shortcuts not removed", e);
        }
    }

    /** The system's page for the connection notice's channel alone, where the user can hide it. */
    public static void connectionSettings(Activity activity) {
        activity.startActivity(new Intent(Settings.ACTION_CHANNEL_NOTIFICATION_SETTINGS)
                .putExtra(Settings.EXTRA_APP_PACKAGE, activity.getPackageName())
                .putExtra(Settings.EXTRA_CHANNEL_ID, CONNECTION));
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
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        // A conversation notification needs its chat's shortcut at every post, and Share.publish may
        // have replaced it since the last one; Android's guidance is a push per message anyway.
        try {
            ShortcutManagerCompat.pushDynamicShortcut(context, Share.shortcut(context, chat, name, picture, 0));
        } catch (IllegalStateException | IllegalArgumentException e) {
            Log.w("Notify", "conversation shortcut", e);
        }
        // In a one-to-one chat the picture is the sender's; in a group it is the chat's.
        Person person = new Person.Builder().setName(sender).setKey(sender).build();
        if (!group && picture != null) {
            person = new Person.Builder().setName(sender).setKey(sender).setIcon(IconCompat.createWithBitmap(picture)).build();
        }
        NotificationCompat.MessagingStyle old = conversation(manager, chat);
        NotificationCompat.MessagingStyle style = extend(context, old, group ? name : null, group,
                new NotificationCompat.MessagingStyle.Message(text, time, person));
        // Once a chat has been mentioned in, its notification stays on the mentions channel until
        // it is read, so the sound of a later message is the mention's.
        boolean mentioned = mention || (active(manager, chat) != null
                && MENTIONS.equals(active(manager, chat).getNotification().getChannelId()));
        String channel = mentioned ? MENTIONS : group ? GROUPS : MESSAGES;
        post(context, manager, chat, name, channel, picture, style, text, time, alert);
        summarize(context, manager, chat, true);
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
        // Updating the notification is what stops its reply box from spinning.
        append(context, chat, sent ? text : context.getString(R.string.reply_not_sent), sent);
    }

    /** Adds a line to the chat's notification, quietly: the user's own when {@code mine}, else the app's. */
    private static void append(Context context, String chat, CharSequence text, boolean mine) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        NotificationCompat.MessagingStyle old = conversation(manager, chat);
        StatusBarNotification shown = active(manager, chat);
        if (old == null || shown == null) {
            return;
        }
        long now = System.currentTimeMillis();
        NotificationCompat.MessagingStyle.Message line = new NotificationCompat.MessagingStyle.Message(text, now,
                mine ? old.getUser() : new Person.Builder().setName("Unisend").setKey("beeper").build());
        NotificationCompat.MessagingStyle style = extend(context, old, old.getConversationTitle() == null ? null
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

    /** Mutes the chat from its notification, as the chat list's Mute does. */
    static void mute(Context context, String chat) {
        boolean muted = false;
        try {
            muted = nativeMute(chat);
        } catch (UnsatisfiedLinkError e) {
            // The app's process was gone and this one was started for the receiver alone.
        }
        if (!muted) {
            append(context, chat, context.getString(R.string.mute_failed), false);
            return;
        }
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        manager.cancel(chat, 0);
        summarize(context, manager, chat, false);
    }

    private static NotificationCompat.MessagingStyle extend(Context context, NotificationCompat.MessagingStyle old, String title,
            boolean group, NotificationCompat.MessagingStyle.Message add) {
        NotificationCompat.MessagingStyle style = new NotificationCompat.MessagingStyle(old == null
                ? new Person.Builder().setName(context.getString(R.string.you)).setKey("me").build() : old.getUser());
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

        RemoteInput input = new RemoteInput.Builder(KEY_REPLY).setLabel(context.getString(R.string.reply)).build();
        PendingIntent reply = PendingIntent.getBroadcast(context, code, receiver(context, ACTION_REPLY, chat),
                PendingIntent.FLAG_MUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        PendingIntent read = PendingIntent.getBroadcast(context, code, receiver(context, ACTION_READ, chat),
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        PendingIntent mute = PendingIntent.getBroadcast(context, code, receiver(context, ACTION_MUTE, chat),
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
                .setShortcutId(chat)
                .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
                .setPublicVersion(locked(context, channel))
                .addAction(new NotificationCompat.Action.Builder(android.R.drawable.ic_menu_send, context.getString(R.string.reply), reply)
                        .addRemoteInput(input)
                        .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_REPLY)
                        .setShowsUserInterface(false)
                        .setAllowGeneratedReplies(false)
                        .build())
                .addAction(new NotificationCompat.Action.Builder(android.R.drawable.ic_menu_view, context.getString(R.string.mark_read), read)
                        .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_MARK_AS_READ)
                        .setShowsUserInterface(false)
                        .build())
                .addAction(new NotificationCompat.Action.Builder(android.R.drawable.ic_lock_silent_mode, context.getString(R.string.mute), mute)
                        .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_MUTE)
                        .setShowsUserInterface(false)
                        .build());
        if (picture != null) {
            builder.setLargeIcon(picture);
        }
        manager.notify(chat, 0, builder.build());
    }

    /**
     * The line the chats fold under while two or more are waiting. A lone chat has none, so its own
     * notification, with its buttons, is the one in the shade. Android cancels a summary's chats
     * with it, so when a chat goes and leaves one, that one is posted again on its own. {@code chat}
     * was just posted ({@code includes}) or taken away, and may still be listed; null for neither.
     */
    private static void summarize(Context context, NotificationManager manager, String chat, boolean includes) {
        int chats = includes ? 1 : 0;
        boolean summarized = false;
        StatusBarNotification other = null;
        for (StatusBarNotification shown : manager.getActiveNotifications()) {
            if (shown.getId() != 0 || shown.getTag() == null || shown.getTag().equals(chat)) {
                continue;
            }
            if (shown.getTag().equals(SUMMARY)) {
                summarized = true;
            } else if (GROUP.equals(shown.getNotification().getGroup())) {
                chats++;
                other = shown;
            }
        }
        if (chats == 0) {
            manager.cancel(SUMMARY, 0);
            return;
        }
        if (chats == 1 && !summarized) {
            return;
        }
        if (chats == 1 && !includes) {
            manager.cancel(SUMMARY, 0);
            repost(context, manager, other);
            return;
        }
        PendingIntent open = PendingIntent.getActivity(context, 0, new Intent(context, activityClass(context))
                .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP | Intent.FLAG_ACTIVITY_CLEAR_TOP),
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        manager.notify(SUMMARY, 0, new NotificationCompat.Builder(context, MESSAGES)
                .setSmallIcon(android.R.drawable.stat_notify_chat)
                .setContentTitle("Unisend")
                .setContentText(context.getResources().getQuantityString(R.plurals.chats_with_new, chats, chats))
                .setGroup(GROUP)
                .setGroupSummary(true)
                .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
                .setPublicVersion(locked(context, MESSAGES))
                // The chats alert on their own; the line over them stays quiet.
                .setGroupAlertBehavior(NotificationCompat.GROUP_ALERT_CHILDREN)
                .setAutoCancel(true)
                .setContentIntent(open)
                .build());
    }

    /** What a secure lock screen shows instead: the app and that something came, not from whom. */
    private static Notification locked(Context context, String channel) {
        return new NotificationCompat.Builder(context, channel)
                .setSmallIcon(android.R.drawable.stat_notify_chat)
                .setContentTitle(context.getString(R.string.app_name))
                .setContentText(context.getString(R.string.new_message))
                .setCategory(NotificationCompat.CATEGORY_MESSAGE)
                .build();
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
        Bitmap bitmap = round(path);
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

    /**
     * The picture at {@code path} cut to an {@link #AVATAR} px circle, or null when it cannot be
     * read. Android draws a bitmap icon as it is, square corners included; only adaptive icons get
     * a mask, and that mask is the maker's shape (a squircle on some phones), so pictures are
     * rounded here. Decoding is scaled down to at least {@link #AVATAR} px on the short side.
     */
    static Bitmap round(String path) {
        BitmapFactory.Options options = new BitmapFactory.Options();
        options.inJustDecodeBounds = true;
        BitmapFactory.decodeFile(path, options);
        int shorter = Math.min(options.outWidth, options.outHeight);
        if (shorter <= 0) {
            return null;
        }
        options.inJustDecodeBounds = false;
        options.inSampleSize = Integer.highestOneBit(Math.max(1, shorter / AVATAR));
        Bitmap source = BitmapFactory.decodeFile(path, options);
        if (source == null) {
            return null;
        }
        int w = source.getWidth(), h = source.getHeight(), side = Math.min(w, h);
        float scale = (float) AVATAR / side;
        Matrix fit = new Matrix();
        fit.setScale(scale, scale);
        fit.postTranslate((side - w) * scale / 2f, (side - h) * scale / 2f);
        BitmapShader shader = new BitmapShader(source, Shader.TileMode.CLAMP, Shader.TileMode.CLAMP);
        shader.setLocalMatrix(fit);
        Paint paint = new Paint(Paint.ANTI_ALIAS_FLAG | Paint.FILTER_BITMAP_FLAG);
        paint.setShader(shader);
        Bitmap circle = Bitmap.createBitmap(AVATAR, AVATAR, Bitmap.Config.ARGB_8888);
        new Canvas(circle).drawCircle(AVATAR / 2f, AVATAR / 2f, AVATAR / 2f, paint);
        source.recycle();
        return circle;
    }
}
