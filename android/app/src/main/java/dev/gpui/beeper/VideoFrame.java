package dev.gpui.beeper;

import android.graphics.Bitmap;
import android.media.MediaMetadataRetriever;
import android.util.Log;
import java.io.ByteArrayOutputStream;
import java.nio.ByteBuffer;
import java.util.Arrays;

/** Stills from a video file: the preview before sending, and a bridged GIF's frames. Called from Rust over JNI, off the UI thread. */
public final class VideoFrame {
    private static final String TAG = "Unisend";
    private VideoFrame() {}

    /**
     * Up to {@code count} frames {@code stepMs} apart from the start, each scaled to
     * {@code width} x {@code height} and laid one after another as RGBA bytes, for a bridged GIF
     * that loops in its bubble; null when none could be read. One frame is held at a time.
     */
    public static byte[] frames(String path, int width, int height, int stepMs, int count) {
        MediaMetadataRetriever retriever = new MediaMetadataRetriever();
        try {
            retriever.setDataSource(path);
            String length = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION);
            long durationMs = length == null ? Long.MAX_VALUE : Long.parseLong(length);
            int frame = width * height * 4;
            ByteBuffer out = ByteBuffer.allocate(frame * count);
            int taken = 0;
            for (; taken < count && (long) taken * stepMs < durationMs; taken++) {
                Bitmap bitmap = retriever.getFrameAtTime((long) taken * stepMs * 1000, MediaMetadataRetriever.OPTION_CLOSEST);
                if (bitmap == null) {
                    break;
                }
                Bitmap scaled = Bitmap.createScaledBitmap(bitmap, width, height, true);
                Bitmap rgba = scaled.getConfig() == Bitmap.Config.ARGB_8888 ? scaled : scaled.copy(Bitmap.Config.ARGB_8888, false);
                // ARGB_8888 lies in memory as R, G, B, A.
                rgba.copyPixelsToBuffer(out);
                for (Bitmap used : new Bitmap[] {bitmap, scaled, rgba}) {
                    if (!used.isRecycled()) {
                        used.recycle();
                    }
                }
            }
            if (taken == 0) {
                return null;
            }
            return taken == count ? out.array() : Arrays.copyOf(out.array(), taken * frame);
        } catch (RuntimeException e) {
            Log.w(TAG, "video frames", e);
            return null;
        } finally {
            try {
                retriever.release();
            } catch (Exception ignored) {
                // Nothing left to free.
            }
        }
    }

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
