package dev.gpui.beeper;

import android.media.MediaCodec;
import android.media.MediaExtractor;
import android.media.MediaFormat;
import android.media.MediaMetadataRetriever;
import android.media.MediaMuxer;
import android.util.Log;
import java.io.File;
import java.nio.ByteBuffer;

/**
 * A stretch of a video copied to a new MP4 without re-encoding, for the preview's trim. It starts at
 * the key frame at or before the chosen start, as a copy must. Called from Rust over JNI, off the
 * UI thread. Memory: one sample buffer, the largest sample the file declares (1 to 32 MB).
 */
public final class VideoTrim {
    private static final String TAG = "Unisend";
    private static final int MIN_BUFFER = 1 << 20;
    private static final int MAX_BUFFER = 32 << 20;

    private VideoTrim() {}

    /** Copies {@code startUs} to {@code endUs} of {@code path} to {@code out}; false on failure, with nothing left at {@code out}. */
    public static boolean trim(String path, long startUs, long endUs, String out) {
        MediaExtractor extractor = new MediaExtractor();
        MediaMuxer muxer = null;
        boolean started = false;
        boolean ok = false;
        try {
            extractor.setDataSource(path);
            muxer = new MediaMuxer(out, MediaMuxer.OutputFormat.MUXER_OUTPUT_MPEG_4);
            int count = extractor.getTrackCount();
            int[] tracks = new int[count];
            int size = MIN_BUFFER;
            for (int i = 0; i < count; i++) {
                MediaFormat format = extractor.getTrackFormat(i);
                String mime = format.getString(MediaFormat.KEY_MIME);
                tracks[i] = -1;
                if (mime != null && (mime.startsWith("video/") || mime.startsWith("audio/"))) {
                    extractor.selectTrack(i);
                    tracks[i] = muxer.addTrack(format);
                    if (format.containsKey(MediaFormat.KEY_MAX_INPUT_SIZE)) {
                        size = Math.max(size, Math.min(MAX_BUFFER, format.getInteger(MediaFormat.KEY_MAX_INPUT_SIZE)));
                    }
                }
            }
            muxer.setOrientationHint(rotation(path));
            extractor.seekTo(startUs, MediaExtractor.SEEK_TO_PREVIOUS_SYNC);
            ByteBuffer buffer = ByteBuffer.allocate(size);
            MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
            muxer.start();
            started = true;
            long base = -1;
            while (true) {
                int read = extractor.readSampleData(buffer, 0);
                if (read < 0) {
                    break;
                }
                long time = extractor.getSampleTime();
                if (time > endUs) {
                    break;
                }
                int track = tracks[extractor.getSampleTrackIndex()];
                if (track >= 0) {
                    if (base < 0) {
                        base = time;
                    }
                    int flags = (extractor.getSampleFlags() & MediaExtractor.SAMPLE_FLAG_SYNC) != 0 ? MediaCodec.BUFFER_FLAG_KEY_FRAME : 0;
                    info.set(0, read, Math.max(0, time - base), flags);
                    muxer.writeSampleData(track, buffer, info);
                }
                extractor.advance();
            }
            ok = base >= 0;
        } catch (Exception e) {
            Log.w(TAG, "video trim: " + e.getClass().getSimpleName());
        } finally {
            if (muxer != null) {
                try {
                    if (started) {
                        muxer.stop();
                    }
                } catch (RuntimeException e) {
                    ok = false;
                }
                muxer.release();
            }
            extractor.release();
        }
        if (!ok) {
            new File(out).delete();
        }
        return ok;
    }

    private static int rotation(String path) {
        MediaMetadataRetriever retriever = new MediaMetadataRetriever();
        try {
            retriever.setDataSource(path);
            String degrees = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_ROTATION);
            return degrees == null ? 0 : Integer.parseInt(degrees);
        } catch (RuntimeException e) {
            return 0;
        } finally {
            try {
                retriever.release();
            } catch (Exception ignored) {
                // Nothing left to free.
            }
        }
    }
}
