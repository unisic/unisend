package dev.gpui.beeper;

import android.app.Activity;
import android.content.ActivityNotFoundException;
import android.content.ContentResolver;
import android.content.ContentValues;
import android.content.res.AssetFileDescriptor;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Build;
import android.os.Environment;
import android.provider.MediaStore;
import android.provider.OpenableColumns;
import android.system.ErrnoException;
import android.system.Os;
import android.util.Log;
import android.webkit.MimeTypeMap;
import androidx.core.content.FileProvider;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.Locale;
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
     * Copies a picked or shared document into the cache under a neutral name (image.jpg,
     * video.mp4, file.pdf) and returns the path, or null when it cannot be read or is this app's
     * own. Each pick gets a directory; ones older than an hour are removed.
     */
    public static String copyToCache(Activity activity, String uriString) {
        Uri uri = Uri.parse(uriString);
        String pkg = activity.getPackageName();
        if (refused(uri.getScheme(), uri.getAuthority(), pkg)) {
            Log.w(TAG, "refused a file from this app's own storage");
            return null;
        }
        ContentResolver resolver = activity.getContentResolver();
        String display = null;
        try (Cursor c = resolver.query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (c != null && c.moveToFirst() && !c.isNull(0)) {
                display = c.getString(0);
            }
        } catch (RuntimeException e) {
            // Providers may refuse the query; the file is still readable, under the fallback name.
        }
        String mime = null;
        try {
            mime = resolver.getType(uri);
        } catch (RuntimeException e) {
            // As above: the extension alone then names the kind.
        }
        String ext = extension(display);
        MimeTypeMap types = MimeTypeMap.getSingleton();
        if (ext == null && mime != null) {
            String guessed = types.getExtensionFromMimeType(mime);
            ext = guessed == null ? null : extension("." + guessed);
        }
        if (mime == null && ext != null) {
            mime = types.getMimeTypeFromExtension(ext);
        }
        String name = neutralName(ext, mime);

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
        AssetFileDescriptor afd;
        try {
            afd = resolver.openAssetFileDescriptor(uri, "r");
        } catch (IOException | RuntimeException e) {
            Log.w(TAG, "open picked file", e);
            dir.delete();
            return null;
        }
        if (afd == null) {
            dir.delete();
            return null;
        }
        // Another app's provider can hand back a file of ours: check where the open file really is.
        try {
            String real = Os.readlink("/proc/self/fd/" + afd.getParcelFileDescriptor().getFd());
            if (insideOwn(real, pkg)) {
                Log.w(TAG, "refused a file from this app's own storage");
                afd.close();
                dir.delete();
                return null;
            }
        } catch (ErrnoException | IOException e) {
            // A pipe or socket has no path to check, and none of ours is shared that way.
        }
        try (InputStream in = afd.createInputStream();
                OutputStream os = new FileOutputStream(out)) {
            byte[] buffer = new byte[65536];
            for (int n = in.read(buffer); n > 0; n = in.read(buffer)) {
                os.write(buffer, 0, n);
            }
            return out.getAbsolutePath();
        } catch (IOException e) {
            Log.w(TAG, "copy picked file", e);
            out.delete();
            dir.delete();
            return null;
        }
    }

    /** A file address this app must not read on another's behalf: anything but another app's provider. */
    static boolean refused(String scheme, String authority, String pkg) {
        if (!"content".equalsIgnoreCase(scheme) || authority == null) {
            return true;
        }
        String own = pkg.toLowerCase(Locale.ROOT);
        String a = authority.toLowerCase(Locale.ROOT);
        // An authority may carry a user: "0@dev.gpui.beeper.files".
        a = a.substring(a.indexOf('@') + 1);
        return a.equals(own) || a.startsWith(own + ".");
    }

    /** Whether a real path lies in this app's own storage, internal or external. */
    static boolean insideOwn(String path, String pkg) {
        if (path == null) {
            return false;
        }
        for (String part : path.split("/+")) {
            // data/data/<pkg>, data/user/0/<pkg>, data/user_de/0/<pkg>, Android/data/<pkg>, ...
            if (part.equals(pkg)) {
                return true;
            }
        }
        return false;
    }

    /** The lower-case extension of a file name, letters and digits only, or null. */
    static String extension(String name) {
        if (name == null) {
            return null;
        }
        int dot = name.lastIndexOf('.');
        String ext = dot < 0 ? "" : name.substring(dot + 1).toLowerCase(Locale.ROOT);
        if (ext.isEmpty() || ext.length() > 10 || !ext.matches("[a-z0-9]+")) {
            return null;
        }
        return ext;
    }

    /** A name that says only the kind: image.jpg, video.mp4, file.pdf, or file. */
    static String neutralName(String ext, String mime) {
        String kind = mime == null ? "file"
                : mime.startsWith("image/") ? "image"
                : mime.startsWith("video/") ? "video"
                : "file";
        return ext == null ? kind : kind + "." + ext;
    }
}
