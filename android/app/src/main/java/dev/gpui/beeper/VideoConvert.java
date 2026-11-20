package dev.gpui.beeper;

import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaExtractor;
import android.media.MediaFormat;
import android.media.MediaMuxer;
import android.util.Log;
import android.view.Surface;
import java.io.File;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.List;

/**
 * A video made into an MP4 (H.264 and AAC) with the phone's own codecs, for one the chat's network
 * refuses or whose metadata cannot be removed: Android's MediaCodec in place of the desktop's ffmpeg.
 * The video decoder draws into the encoder's input surface, so frames never pass through Java; the
 * sound is decoded to PCM and copied into the AAC encoder. Nothing is copied but the first video and
 * the first sound track. Called from Rust over JNI, off the UI thread. Memory: the codecs' own buffers.
 */
public final class VideoConvert {
    private static final String TAG = "Unisend";
    private static final String AVC = MediaFormat.MIMETYPE_VIDEO_AVC;
    private static final String AAC = MediaFormat.MIMETYPE_AUDIO_AAC;
    private static final long WAIT_US = 10_000;
    /** No buffer moved for this long: a codec has stalled, and the conversion gives up. */
    private static final long STALL_NS = 10_000_000_000L;

    private VideoConvert() {}

    /** Writes {@code path} as an MP4 to {@code out}; false on failure, with nothing left at {@code out}. */
    public static boolean convert(String path, String out) {
        List<Lane> lanes = new ArrayList<>();
        MediaMuxer muxer = null;
        boolean started = false;
        boolean ok = false;
        try {
            int video = -1;
            int audio = -1;
            MediaExtractor probe = new MediaExtractor();
            try {
                probe.setDataSource(path);
                for (int i = 0; i < probe.getTrackCount(); i++) {
                    String mime = probe.getTrackFormat(i).getString(MediaFormat.KEY_MIME);
                    if (mime != null && mime.startsWith("video/") && video < 0) {
                        video = i;
                    } else if (mime != null && mime.startsWith("audio/") && audio < 0) {
                        audio = i;
                    }
                }
            } finally {
                probe.release();
            }
            if (video < 0) {
                throw new IOException("no video track");
            }
            lanes.add(new Lane(path, video, true));
            if (audio >= 0) {
                lanes.add(new Lane(path, audio, false));
            }
            muxer = new MediaMuxer(out, MediaMuxer.OutputFormat.MUXER_OUTPUT_MPEG_4);
            muxer.setOrientationHint(VideoTrim.rotation(path));
            long last = System.nanoTime();
            while (true) {
                boolean moved = false;
                boolean done = true;
                boolean ready = true;
                for (Lane lane : lanes) {
                    moved |= lane.step(muxer, started);
                    done &= lane.done;
                    ready &= lane.track >= 0;
                }
                // Every track is added before the muxer starts; an encoder waits for that with its first output.
                if (!started && ready) {
                    muxer.start();
                    started = true;
                }
                if (done) {
                    break;
                }
                long now = System.nanoTime();
                if (moved) {
                    last = now;
                } else if (now - last > STALL_NS) {
                    throw new IllegalStateException("stalled");
                }
            }
            ok = started;
        } catch (Exception e) {
            Log.w(TAG, "video convert: " + e.getClass().getSimpleName());
        } finally {
            for (Lane lane : lanes) {
                lane.release();
            }
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
        }
        if (!ok) {
            new File(out).delete();
        }
        return ok;
    }

    /** One track on its way: extractor, decoder, encoder, then the muxer. */
    private static final class Lane {
        final boolean video;
        final MediaExtractor extractor = new MediaExtractor();
        final MediaCodec decoder;
        MediaCodec encoder;
        Surface surface;
        final MediaCodec.BufferInfo decoded = new MediaCodec.BufferInfo();
        final MediaCodec.BufferInfo encoded = new MediaCodec.BufferInfo();
        int track = -1;
        boolean inputDone;
        boolean decoderDone;
        boolean done;
        /** Sound: a decoder buffer partly copied into the encoder, its next byte and end. */
        int pending = -1;
        int pendingStart;
        int pendingAt;
        int pendingEnd;
        long pendingUs;
        boolean pendingLast;
        int bytesPerSecond;

        Lane(String path, int index, boolean video) throws IOException {
            this.video = video;
            extractor.setDataSource(path);
            extractor.selectTrack(index);
            MediaFormat in = extractor.getTrackFormat(index);
            decoder = MediaCodec.createDecoderByType(in.getString(MediaFormat.KEY_MIME));
            if (video) {
                int width = in.getInteger(MediaFormat.KEY_WIDTH) & ~1;
                int height = in.getInteger(MediaFormat.KEY_HEIGHT) & ~1;
                MediaFormat f = MediaFormat.createVideoFormat(AVC, width, height);
                f.setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface);
                f.setInteger(MediaFormat.KEY_FRAME_RATE, frameRate(in));
                f.setInteger(MediaFormat.KEY_BIT_RATE, Math.max(1_000_000, Math.min(12_000_000, width * height * 4)));
                f.setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 2);
                encoder = MediaCodec.createEncoderByType(AVC);
                encoder.configure(f, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
                surface = encoder.createInputSurface();
                encoder.start();
                decoder.configure(in, surface, null, 0);
            } else {
                decoder.configure(in, null, null, 0);
            }
            decoder.start();
        }

        /** Moves what can move without waiting long; true when anything did. */
        boolean step(MediaMuxer muxer, boolean started) {
            boolean moved = false;
            if (!inputDone) {
                moved |= feed();
            }
            if (!decoderDone) {
                moved |= video ? drawFrame() : copySound();
            }
            if (encoder != null && !done) {
                moved |= drain(muxer, started);
            }
            return moved;
        }

        boolean feed() {
            int i = decoder.dequeueInputBuffer(WAIT_US);
            if (i < 0) {
                return false;
            }
            int n = extractor.readSampleData(decoder.getInputBuffer(i), 0);
            if (n < 0) {
                decoder.queueInputBuffer(i, 0, 0, 0, MediaCodec.BUFFER_FLAG_END_OF_STREAM);
                inputDone = true;
            } else {
                decoder.queueInputBuffer(i, 0, n, extractor.getSampleTime(), 0);
                extractor.advance();
            }
            return true;
        }

        boolean drawFrame() {
            int i = decoder.dequeueOutputBuffer(decoded, WAIT_US);
            if (i < 0) {
                return i == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED;
            }
            boolean last = (decoded.flags & MediaCodec.BUFFER_FLAG_END_OF_STREAM) != 0;
            decoder.releaseOutputBuffer(i, decoded.size > 0);
            if (last) {
                encoder.signalEndOfInputStream();
                decoderDone = true;
            }
            return true;
        }

        boolean copySound() {
            boolean moved = false;
            if (pending < 0) {
                int i = decoder.dequeueOutputBuffer(decoded, WAIT_US);
                if (i == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                    startAac(decoder.getOutputFormat());
                    return true;
                }
                if (i < 0) {
                    return false;
                }
                startAac(decoder.getOutputFormat());
                pending = i;
                pendingStart = decoded.offset;
                pendingAt = decoded.offset;
                pendingEnd = decoded.offset + decoded.size;
                pendingUs = decoded.presentationTimeUs;
                pendingLast = (decoded.flags & MediaCodec.BUFFER_FLAG_END_OF_STREAM) != 0;
                moved = true;
            }
            int j = encoder.dequeueInputBuffer(WAIT_US);
            if (j < 0) {
                return moved;
            }
            ByteBuffer from = decoder.getOutputBuffer(pending);
            ByteBuffer to = encoder.getInputBuffer(j);
            int n = Math.min(to.remaining(), pendingEnd - pendingAt);
            from.limit(pendingAt + n);
            from.position(pendingAt);
            to.put(from);
            long us = pendingUs + (pendingAt - pendingStart) * 1_000_000L / bytesPerSecond;
            pendingAt += n;
            boolean whole = pendingAt >= pendingEnd;
            encoder.queueInputBuffer(j, 0, n, us, whole && pendingLast ? MediaCodec.BUFFER_FLAG_END_OF_STREAM : 0);
            if (whole) {
                decoder.releaseOutputBuffer(pending, false);
                pending = -1;
                decoderDone = pendingLast;
            }
            return true;
        }

        /** The AAC encoder, made once the decoder names its rate and channels; 16-bit PCM in. */
        void startAac(MediaFormat pcm) {
            if (encoder != null) {
                return;
            }
            int rate = pcm.getInteger(MediaFormat.KEY_SAMPLE_RATE);
            int channels = pcm.getInteger(MediaFormat.KEY_CHANNEL_COUNT);
            bytesPerSecond = Math.max(1, rate * channels * 2);
            MediaFormat f = MediaFormat.createAudioFormat(AAC, rate, channels);
            f.setInteger(MediaFormat.KEY_AAC_PROFILE, MediaCodecInfo.CodecProfileLevel.AACObjectLC);
            f.setInteger(MediaFormat.KEY_BIT_RATE, 128_000);
            f.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, 1 << 16);
            try {
                encoder = MediaCodec.createEncoderByType(AAC);
            } catch (IOException e) {
                throw new IllegalStateException("no AAC encoder");
            }
            encoder.configure(f, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
            encoder.start();
        }

        /** Encoder output to the muxer; it waits while the muxer has not started. */
        boolean drain(MediaMuxer muxer, boolean started) {
            if (track >= 0 && !started) {
                return false;
            }
            int i = encoder.dequeueOutputBuffer(encoded, WAIT_US);
            if (i == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                track = muxer.addTrack(encoder.getOutputFormat());
                return true;
            }
            if (i < 0) {
                return false;
            }
            if ((encoded.flags & MediaCodec.BUFFER_FLAG_CODEC_CONFIG) == 0 && encoded.size > 0) {
                muxer.writeSampleData(track, encoder.getOutputBuffer(i), encoded);
            }
            encoder.releaseOutputBuffer(i, false);
            done = (encoded.flags & MediaCodec.BUFFER_FLAG_END_OF_STREAM) != 0;
            return true;
        }

        void release() {
            for (MediaCodec codec : new MediaCodec[] {decoder, encoder}) {
                if (codec == null) {
                    continue;
                }
                try {
                    codec.stop();
                } catch (RuntimeException ignored) {
                    // Never started, or already failed: release frees it either way.
                }
                codec.release();
            }
            if (surface != null) {
                surface.release();
            }
            extractor.release();
        }
    }

    /** The source's frame rate where it names one (an int or a float, by container), else 30. */
    private static int frameRate(MediaFormat in) {
        try {
            return Math.max(1, in.getInteger(MediaFormat.KEY_FRAME_RATE));
        } catch (RuntimeException notInt) {
            try {
                return Math.max(1, Math.round(in.getFloat(MediaFormat.KEY_FRAME_RATE)));
            } catch (RuntimeException none) {
                return 30;
            }
        }
    }
}
