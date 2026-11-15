package dev.gpui.beeper;

import android.app.Activity;
import android.content.ActivityNotFoundException;
import android.content.ContentResolver;
import android.content.ContentValues;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Build;
import android.os.Environment;
import android.provider.MediaStore;
import android.provider.OpenableColumns;
import android.util.Log;
import androidx.core.content.FileProvider;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.concurrent.atomic.AtomicInteger;

/** File hand-offs between this app and others. All methods are called from Rust over JNI. */
public final class Files {
    private static final String TAG = "Murmur";
    private static final long STALE_MS = 60 * 60 * 1000;
    private static final AtomicInteger PICKS = new AtomicInteger();

    private Files() {}

    /** Opens a file from the media cache in whatever app handles its type. */
    public static boolean view(Activity activity, String path, String mime) {
        Uri uri = FileProvider.getUriForFile(activity, activity.getPackageName() + ".files", new File(path));
        Intent intent = new Intent(Intent.ACTION_VIEW)
                .setDataAndType(uri, mime)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        try {
            activity.startActivity(intent);
            return true;
        } catch (ActivityNotFoundException e) {
            return false;
        }
    }

    /** Offers a file from the media cache to other apps through the system's share sheet. */
    public static boolean share(Activity activity, String path, String mime) {
        Uri uri = FileProvider.getUriForFile(activity, activity.getPackageName() + ".files", new File(path));
        Intent send = new Intent(Intent.ACTION_SEND)
                .setType(mime)
                .putExtra(Intent.EXTRA_STREAM, uri)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        try {
            activity.startActivity(Intent.createChooser(send, null));
            return true;
        } catch (ActivityNotFoundException e) {
            return false;
        }
    }

    /** Saving to the gallery needs no permission from Android 10; before that it needs one. */
    public static boolean canSave() {
        return Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q;
    }

    /**
     * Copies a file into the gallery: Pictures/Murmur, or Movies/Murmur for a video. Returns the
     * folder it went to, or null when it could not be saved.
     */
    public static String save(Activity activity, String path, String name, String mime) {
        if (!canSave()) {
            return null;
        }
        boolean video = mime.startsWith("video/");
        String folder = (video ? Environment.DIRECTORY_MOVIES : Environment.DIRECTORY_PICTURES) + "/Murmur";
        Uri collection = video
                ? MediaStore.Video.Media.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
                : MediaStore.Images.Media.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY);
        ContentValues values = new ContentValues();
        values.put(MediaStore.MediaColumns.DISPLAY_NAME, name);
        values.put(MediaStore.MediaColumns.MIME_TYPE, mime);
        values.put(MediaStore.MediaColumns.RELATIVE_PATH, folder);
        values.put(MediaStore.MediaColumns.IS_PENDING, 1);
        ContentResolver resolver = activity.getContentResolver();
        Uri item = resolver.insert(collection, values);
        if (item == null) {
            return null;
        }
        try (InputStream in = new FileInputStream(path);
                OutputStream os = resolver.openOutputStream(item)) {
            if (os == null) {
                throw new IOException("no output stream");
            }
            byte[] buffer = new byte[65536];
            for (int n = in.read(buffer); n > 0; n = in.read(buffer)) {
                os.write(buffer, 0, n);
            }
        } catch (IOException | RuntimeException e) {
            Log.w(TAG, "save to gallery", e);
            resolver.delete(item, null, null);
            return null;
        }
        ContentValues done = new ContentValues();
        done.put(MediaStore.MediaColumns.IS_PENDING, 0);
        resolver.update(item, done, null, null);
        return folder;
    }

    /** Opens a web address in the browser. */
    public static boolean browse(Activity activity, String url) {
        try {
            activity.startActivity(new Intent(Intent.ACTION_VIEW, Uri.parse(url)));
            return true;
        } catch (ActivityNotFoundException e) {
            return false;
        }
    }

    /**
     * Copies a picked document into the cache under its own name and returns the path, or null
     * when it cannot be read. Each pick gets a directory; ones older than an hour are removed.
     */
    public static String copyToCache(Activity activity, String uriString) {
        Uri uri = Uri.parse(uriString);
        String name = "file";
        try (Cursor c = activity.getContentResolver()
                .query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (c != null && c.moveToFirst() && !c.isNull(0)) {
                name = new File(c.getString(0)).getName();
            }
        } catch (RuntimeException e) {
            // Providers may refuse the query; the file is still readable, under the fallback name.
        }

        File root = new File(activity.getCacheDir(), "outgoing");
        long now = System.currentTimeMillis();
        File[] picks = root.listFiles();
        if (picks != null) {
            for (File pick : picks) {
                if (now - pick.lastModified() > STALE_MS) {
                    File[] files = pick.listFiles();
                    if (files != null) {
                        for (File f : files) f.delete();
                    }
                    pick.delete();
                }
            }
        }
        // Several picks may be copied within one millisecond.
        File dir = new File(root, now + "-" + PICKS.getAndIncrement());
        if (!dir.mkdirs()) {
            return null;
        }
        File out = new File(dir, name);
        try (InputStream in = activity.getContentResolver().openInputStream(uri);
                OutputStream os = new FileOutputStream(out)) {
            if (in == null) {
                return null;
            }
            byte[] buffer = new byte[65536];
            for (int n = in.read(buffer); n > 0; n = in.read(buffer)) {
                os.write(buffer, 0, n);
            }
            return out.getAbsolutePath();
        } catch (IOException e) {
            Log.w(TAG, "copy picked file", e);
            return null;
        }
    }
}
