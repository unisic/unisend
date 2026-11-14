package dev.gpui.beeper;

import android.app.Activity;
import android.content.ClipData;
import android.content.Intent;
import android.database.Cursor;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.net.Uri;
import android.provider.OpenableColumns;
import androidx.core.content.IntentCompat;
import androidx.core.content.pm.ShortcutInfoCompat;
import androidx.core.content.pm.ShortcutManagerCompat;
import androidx.core.graphics.drawable.IconCompat;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.Set;

/** What other apps share into this one, and the chats offered in their share sheets. Called from Rust over JNI. */
public final class Share {
    /** Must match the category in res/xml/shortcuts.xml. */
    private static final String CATEGORY = "dev.gpui.beeper.SHARE_TARGET";
    private static final Set<String> CATEGORIES = Collections.singleton(CATEGORY);

    private Share() {}

    /**
     * The share the activity was started or brought forward with, once: null when there is none.
     * Fields are separated by NUL: the chat a Direct Share target named (or empty), the shared
     * text (or empty), then one content address per shared file.
     */
    public static String take(Activity activity) {
        Intent intent = activity.getIntent();
        if (intent == null) {
            return null;
        }
        String action = intent.getAction();
        if (!Intent.ACTION_SEND.equals(action) && !Intent.ACTION_SEND_MULTIPLE.equals(action)) {
            return null;
        }
        // Reopened from Recents, the activity gets the share it was first started with again.
        boolean replayed = (intent.getFlags() & Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY) != 0;
        StringBuilder out = new StringBuilder();
        String chat = intent.getStringExtra(Intent.EXTRA_SHORTCUT_ID);
        out.append(clean(chat));
        CharSequence text = intent.getCharSequenceExtra(Intent.EXTRA_TEXT);
        out.append('\0').append(clean(text == null ? null : text.toString()));
        List<Uri> uris = new ArrayList<>();
        if (Intent.ACTION_SEND.equals(action)) {
            Uri one = IntentCompat.getParcelableExtra(intent, Intent.EXTRA_STREAM, Uri.class);
            if (one != null) {
                uris.add(one);
            }
        } else {
            ArrayList<Uri> many = IntentCompat.getParcelableArrayListExtra(intent, Intent.EXTRA_STREAM, Uri.class);
            if (many != null) {
                for (Uri u : many) {
                    if (u != null) uris.add(u);
                }
            }
        }
        // Some apps put the files only in the clip data, which also carries the read grant.
        ClipData clip = intent.getClipData();
        if (uris.isEmpty() && clip != null) {
            for (int i = 0; i < clip.getItemCount(); i++) {
                Uri u = clip.getItemAt(i).getUri();
                if (u != null) uris.add(u);
            }
        }
        for (Uri u : uris) {
            out.append('\0').append(u.toString());
        }
        activity.setIntent(new Intent(Intent.ACTION_MAIN));
        return replayed ? null : out.toString();
    }

    /** The size a shared file reports, or -1 when its provider does not say. */
    public static long size(Activity activity, String uri) {
        try (Cursor c = activity.getContentResolver()
                .query(Uri.parse(uri), new String[] {OpenableColumns.SIZE}, null, null, null)) {
            if (c != null && c.moveToFirst() && !c.isNull(0)) {
                return c.getLong(0);
            }
        } catch (RuntimeException e) {
            // Providers may refuse the query; the copy is then measured instead.
        }
        return -1;
    }

    /**
     * Replaces the chats offered as Direct Share targets. {@code chats} holds, separated by NUL,
     * a chat ID, its name and the path of its picture (or empty) for each chat, most recent first.
     */
    public static void publish(Activity activity, String chats) {
        String[] f = chats.isEmpty() ? new String[0] : chats.split("\0", -1);
        List<ShortcutInfoCompat> list = new ArrayList<>();
        int max = ShortcutManagerCompat.getMaxShortcutCountPerActivity(activity);
        for (int i = 0; i + 2 < f.length && list.size() < max; i += 3) {
            String id = f[i];
            String name = f[i + 1].isEmpty() ? activity.getString(R.string.chat) : f[i + 1];
            IconCompat icon = null;
            if (!f[i + 2].isEmpty()) {
                Bitmap picture = BitmapFactory.decodeFile(f[i + 2]);
                if (picture != null) icon = IconCompat.createWithBitmap(picture);
            }
            if (icon == null) icon = IconCompat.createWithResource(activity, R.mipmap.ic_launcher);
            // A tap on the shortcut itself opens the chat, as a notification does.
            Intent open = new Intent(Intent.ACTION_VIEW)
                    .setClassName(activity, "dev.gpui.mobile.GpuiActivity")
                    .putExtra(Notify.EXTRA_CHAT, id);
            list.add(new ShortcutInfoCompat.Builder(activity, id)
                    .setShortLabel(name)
                    .setIcon(icon)
                    .setIntent(open)
                    .setCategories(CATEGORIES)
                    .setLongLived(true)
                    .setRank(list.size())
                    .build());
        }
        try {
            ShortcutManagerCompat.setDynamicShortcuts(activity, list);
        } catch (IllegalStateException | IllegalArgumentException e) {
            // Rate limited or refused: the share sheet keeps the previous chats until the next change.
        }
    }

    private static String clean(String s) {
        return s == null ? "" : s.replace('\0', ' ');
    }
}
