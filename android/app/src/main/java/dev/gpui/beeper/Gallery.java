package dev.gpui.beeper;

import android.Manifest;
import android.app.Activity;
import android.content.ContentResolver;
import android.content.ContentUris;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.database.Cursor;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.provider.MediaStore;
import android.provider.Settings;
import android.util.Log;
import android.util.Size;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;

/**
 * The newest photos and videos on the phone for the in-app gallery. Called from Rust over JNI,
 * always off the UI thread.
 */
public final class Gallery {
    private static final String TAG = "Murmur";
    private static final int TYPE_IMAGE = 1;
    private static final int TYPE_VIDEO = 3;

    private Gallery() {}

    /** "full", "partial" (Android 14's chosen photos only) or "none". */
    public static String access(Activity activity) {
        if (Build.VERSION.SDK_INT >= 33) {
            if (granted(activity, Manifest.permission.READ_MEDIA_IMAGES)
                    || granted(activity, Manifest.permission.READ_MEDIA_VIDEO)) {
                return "full";
            }
            if (Build.VERSION.SDK_INT >= 34
                    && granted(activity, "android.permission.READ_MEDIA_VISUAL_USER_SELECTED")) {
                return "partial";
            }
            return "none";
        }
        return granted(activity, Manifest.permission.READ_EXTERNAL_STORAGE) ? "full" : "none";
    }

    /**
     * Asks for access, which on Android 14 also lets the user choose more photos. Once Android
     * stops showing the question (denied twice), opens the app's system page instead.
     */
    public static void ask(Activity activity) {
        String[] wanted;
        if (Build.VERSION.SDK_INT >= 34) {
            wanted = new String[] {Manifest.permission.READ_MEDIA_IMAGES, Manifest.permission.READ_MEDIA_VIDEO,
                    "android.permission.READ_MEDIA_VISUAL_USER_SELECTED"};
        } else if (Build.VERSION.SDK_INT >= 33) {
            wanted = new String[] {Manifest.permission.READ_MEDIA_IMAGES, Manifest.permission.READ_MEDIA_VIDEO};
        } else {
            wanted = new String[] {Manifest.permission.READ_EXTERNAL_STORAGE};
        }
        SharedPreferences prefs = activity.getSharedPreferences("gallery", Activity.MODE_PRIVATE);
        boolean asked = prefs.getBoolean("asked", false);
        if (asked && access(activity).equals("none") && !activity.shouldShowRequestPermissionRationale(wanted[0])) {
            activity.startActivity(new Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
                    Uri.fromParts("package", activity.getPackageName(), null)));
            return;
        }
        prefs.edit().putBoolean("asked", true).apply();
        activity.requestPermissions(wanted, 2);
    }

    /**
     * Up to `limit` photos and videos older than (date, id), newest first; date -1 starts at the
     * newest. One line each: id, content uri, mime type, date added (s), duration (ms), width,
     * height, separated by tabs. Null when MediaStore refuses.
     */
    public static String page(Activity activity, long date, long id, int limit) {
        String selection = "media_type IN (" + TYPE_IMAGE + "," + TYPE_VIDEO + ")";
        String[] args = null;
        if (date >= 0) {
            selection += " AND (date_added < ? OR (date_added = ? AND _id < ?))";
            args = new String[] {Long.toString(date), Long.toString(date), Long.toString(id)};
        }
        Bundle query = new Bundle();
        query.putString(ContentResolver.QUERY_ARG_SQL_SELECTION, selection);
        query.putStringArray(ContentResolver.QUERY_ARG_SQL_SELECTION_ARGS, args);
        query.putString(ContentResolver.QUERY_ARG_SQL_SORT_ORDER, "date_added DESC, _id DESC");
        query.putInt(ContentResolver.QUERY_ARG_LIMIT, limit);
        String[] columns = {"_id", "media_type", "mime_type", "date_added", "duration", "width", "height"};
        StringBuilder out = new StringBuilder();
        try (Cursor c = activity.getContentResolver()
                .query(MediaStore.Files.getContentUri("external"), columns, query, null)) {
            if (c == null) {
                return null;
            }
            // Providers before Android 11 may ignore the limit.
            for (int n = 0; n < limit && c.moveToNext(); n++) {
                long itemId = c.getLong(0);
                Uri base = c.getInt(1) == TYPE_VIDEO
                        ? MediaStore.Video.Media.EXTERNAL_CONTENT_URI
                        : MediaStore.Images.Media.EXTERNAL_CONTENT_URI;
                out.append(itemId).append('\t')
                        .append(ContentUris.withAppendedId(base, itemId)).append('\t')
                        .append(c.isNull(2) ? "" : c.getString(2)).append('\t')
                        .append(c.getLong(3)).append('\t')
                        .append(c.getLong(4)).append('\t')
                        .append(c.getInt(5)).append('\t')
                        .append(c.getInt(6)).append('\n');
            }
        } catch (RuntimeException e) {
            Log.w(TAG, "gallery page", e);
            return null;
        }
        return out.toString();
    }

    /**
     * A square JPEG of the item's middle, at most `size` px a side, or null. `width` and `height`
     * are the item's (0 when unknown): a very tall or wide picture is asked for long enough that
     * its short side still fills the square.
     */
    public static byte[] thumb(Activity activity, String uriString, boolean video, int size, int width, int height) {
        Uri uri = Uri.parse(uriString);
        ContentResolver resolver = activity.getContentResolver();
        Bitmap bitmap = null;
        try {
            if (Build.VERSION.SDK_INT >= 29) {
                long shorter = Math.min(width, height), longer = Math.max(width, height);
                int far = shorter > 0 ? (int) Math.min(size * longer / shorter, size * 8L) : size;
                bitmap = resolver.loadThumbnail(uri, width >= height ? new Size(far, size) : new Size(size, far), null);
            } else if (video) {
                bitmap = MediaStore.Video.Thumbnails.getThumbnail(
                        resolver, ContentUris.parseId(uri), MediaStore.Video.Thumbnails.MINI_KIND, null);
            } else {
                bitmap = decodeSmall(resolver, uri, size);
            }
            if (bitmap == null) {
                return null;
            }
            // The grid shows the middle square; the app keeps these decoded, so no bigger than asked.
            int side = Math.min(bitmap.getWidth(), bitmap.getHeight());
            Bitmap square = Bitmap.createBitmap(bitmap, (bitmap.getWidth() - side) / 2, (bitmap.getHeight() - side) / 2, side, side);
            if (side > size) {
                Bitmap scaled = Bitmap.createScaledBitmap(square, size, size, true);
                if (square != scaled && square != bitmap) {
                    square.recycle();
                }
                square = scaled;
            }
            if (square != bitmap) {
                bitmap.recycle();
                bitmap = square;
            }
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            bitmap.compress(Bitmap.CompressFormat.JPEG, 85, out);
            return out.toByteArray();
        } catch (IOException | RuntimeException e) {
            Log.w(TAG, "gallery thumbnail", e);
            return null;
        } finally {
            if (bitmap != null) {
                bitmap.recycle();
            }
        }
    }

    private static Bitmap decodeSmall(ContentResolver resolver, Uri uri, int size) throws IOException {
        BitmapFactory.Options bounds = new BitmapFactory.Options();
        bounds.inJustDecodeBounds = true;
        try (InputStream in = resolver.openInputStream(uri)) {
            BitmapFactory.decodeStream(in, null, bounds);
        }
        BitmapFactory.Options options = new BitmapFactory.Options();
        options.inSampleSize = 1;
        while (Math.min(bounds.outWidth, bounds.outHeight) / (options.inSampleSize * 2) >= size) {
            options.inSampleSize *= 2;
        }
        try (InputStream in = resolver.openInputStream(uri)) {
            return BitmapFactory.decodeStream(in, null, options);
        }
    }

    private static boolean granted(Activity activity, String permission) {
        return activity.checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED;
    }
}
