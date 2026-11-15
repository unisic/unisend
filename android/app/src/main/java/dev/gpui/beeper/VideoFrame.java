package dev.gpui.beeper;

import android.graphics.Bitmap;
import android.media.MediaMetadataRetriever;
import android.util.Log;
import java.io.ByteArrayOutputStream;

/** A still from a video file, for the preview before sending. Called from Rust over JNI, off the UI thread. */
public final class VideoFrame {
    private static final String TAG = "Unisend";
    private VideoFrame() {}

    /** The first frame as a JPEG with its long side at most {@code max} pixels, or null. */
    public static byte[] frame(String path, int max) {
        MediaMetadataRetriever retriever = new MediaMetadataRetriever();
        Bitmap bitmap = null;
        try {
            retriever.setDataSource(path);
            bitmap = retriever.getFrameAtTime(0, MediaMetadataRetriever.OPTION_CLOSEST_SYNC);
            if (bitmap == null) {
                return null;
            }
            int longer = Math.max(bitmap.getWidth(), bitmap.getHeight());
            if (longer > max) {
                Bitmap scaled = Bitmap.createScaledBitmap(
                        bitmap, bitmap.getWidth() * max / longer, bitmap.getHeight() * max / longer, true);
                if (scaled != bitmap) {
                    bitmap.recycle();
                    bitmap = scaled;
                }
            }
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            bitmap.compress(Bitmap.CompressFormat.JPEG, 85, out);
            return out.toByteArray();
        } catch (RuntimeException e) {
            Log.w(TAG, "video frame", e);
            return null;
        } finally {
            if (bitmap != null) {
                bitmap.recycle();
            }
            try {
                retriever.release();
            } catch (Exception ignored) {
                // Nothing left to free.
            }
        }
    }
}
