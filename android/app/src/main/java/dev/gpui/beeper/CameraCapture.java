package dev.gpui.beeper;

import android.app.Activity;
import android.content.ActivityNotFoundException;
import android.content.ContentResolver;
import android.content.ContentValues;
import android.content.Intent;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.provider.MediaStore;
import androidx.core.content.FileProvider;
import java.io.File;
import java.io.FileInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.text.SimpleDateFormat;
import java.util.Date;
import java.util.Locale;
import java.util.concurrent.CountDownLatch;

/**
 * Takes one photo or video with the system camera into the app cache. Rust calls {@link #capture}
 * off the UI thread and waits; this transparent Activity starts the camera and hands back the
 * result (the GPUI activity cannot receive results itself). The app declares no CAMERA
 * permission on purpose: with one declared, Android refuses the capture intents until it is
 * granted at run time; without it, the camera app does the capturing under its own permission.
 */
public class CameraCapture extends Activity {
    private static final int REQUEST = 9101;
    private static final long STALE_MS = 60 * 60 * 1000;
    private static final String EXTRA_VIDEO = "video";
    private static final String STATE_PATH = "path";
    private static final String STATE_VIDEO = "video";

    private static CountDownLatch latch;
    private static volatile String result;
    private static File pendingFile;
    private static boolean pendingVideo;

    private File file;
    private boolean video;

    /** The captured file's path, or null when cancelled or no camera app answered. */
    public static String capture(Activity activity, boolean video) {
        File dir = new File(activity.getCacheDir(), "camera");
        long now = System.currentTimeMillis();
        File[] old = dir.listFiles();
        if (old != null) {
            for (File f : old) {
                if (now - f.lastModified() > STALE_MS) f.delete();
            }
        }
        if (!dir.isDirectory() && !dir.mkdirs()) {
            return null;
        }
        String stamp = new SimpleDateFormat("yyyyMMdd_HHmmss", Locale.US).format(new Date(now));
        File out = new File(dir, (video ? "VID_" : "IMG_") + stamp + (video ? ".mp4" : ".jpg"));
        CountDownLatch mine = new CountDownLatch(1);
        synchronized (CameraCapture.class) {
            latch = mine;
            result = null;
            pendingFile = out;
            pendingVideo = video;
        }
        Intent proxy = new Intent(activity, CameraCapture.class)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                .putExtra(EXTRA_VIDEO, video);
        try {
            activity.startActivity(proxy);
            mine.await();
        } catch (RuntimeException e) {
            return null;
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            return null;
        }
        return result;
    }

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        if (state != null && state.getString(STATE_PATH) != null) {
            // Recreated while the camera was open; its result still comes to onActivityResult.
            file = new File(state.getString(STATE_PATH));
            video = state.getBoolean(STATE_VIDEO);
            return;
        }
        synchronized (CameraCapture.class) {
            file = pendingFile;
        }
        video = getIntent().getBooleanExtra(EXTRA_VIDEO, false);
        if (file == null) {
            finish();
            return;
        }
        Uri uri = FileProvider.getUriForFile(this, getPackageName() + ".files", file);
        Intent camera = new Intent(video ? MediaStore.ACTION_VIDEO_CAPTURE : MediaStore.ACTION_IMAGE_CAPTURE)
                .putExtra(MediaStore.EXTRA_OUTPUT, uri)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
        try {
            startActivityForResult(camera, REQUEST);
        } catch (ActivityNotFoundException | SecurityException e) {
            deliver(null);
            finish();
        }
    }

    @Override
    protected void onSaveInstanceState(Bundle state) {
        super.onSaveInstanceState(state);
        if (file != null) {
            state.putString(STATE_PATH, file.getAbsolutePath());
            state.putBoolean(STATE_VIDEO, video);
        }
    }

    @Override
    protected void onActivityResult(int request, int code, Intent data) {
        super.onActivityResult(request, code, data);
        boolean taken = request == REQUEST && code == RESULT_OK && file != null && file.length() > 0;
        if (!taken && file != null) {
            file.delete();
        }
        deliver(taken ? file.getAbsolutePath() : null);
        finish();
    }

    /** Wakes the waiting Rust call; when there is none (the app process died meanwhile), keeps the shot in the gallery. */
    private void deliver(String path) {
        CountDownLatch waiting;
        synchronized (CameraCapture.class) {
            waiting = latch;
            latch = null;
            pendingFile = null;
            result = path;
        }
        if (waiting != null) {
            waiting.countDown();
        } else if (path != null) {
            saveToGallery(new File(path), video);
        }
    }

    private void saveToGallery(File source, boolean isVideo) {
        if (Build.VERSION.SDK_INT < 29) {
            return;
        }
        ContentResolver resolver = getContentResolver();
        ContentValues values = new ContentValues();
        values.put(MediaStore.MediaColumns.DISPLAY_NAME, source.getName());
        values.put(MediaStore.MediaColumns.MIME_TYPE, isVideo ? "video/mp4" : "image/jpeg");
        values.put(MediaStore.MediaColumns.RELATIVE_PATH, (isVideo ? "Movies" : "Pictures") + "/Murmur");
        Uri target = resolver.insert(
                isVideo ? MediaStore.Video.Media.EXTERNAL_CONTENT_URI : MediaStore.Images.Media.EXTERNAL_CONTENT_URI,
                values);
        if (target == null) {
            return;
        }
        try (InputStream in = new FileInputStream(source); OutputStream out = resolver.openOutputStream(target)) {
            byte[] buffer = new byte[65536];
            for (int n = in.read(buffer); n > 0; n = in.read(buffer)) {
                out.write(buffer, 0, n);
            }
            source.delete();
        } catch (IOException | RuntimeException e) {
            resolver.delete(target, null, null);
        }
    }
}
