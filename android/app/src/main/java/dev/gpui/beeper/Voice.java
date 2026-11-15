package dev.gpui.beeper;

import android.Manifest;
import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.media.AudioAttributes;
import android.media.AudioFocusRequest;
import android.media.AudioManager;
import android.media.MediaPlayer;
import android.media.MediaRecorder;
import android.net.Uri;
import android.os.Build;
import android.os.SystemClock;
import android.provider.Settings;
import android.util.Log;
import java.io.File;
import java.io.IOException;

/**
 * Voice messages: one recorder and one player for the whole app. Called from Rust over JNI, off
 * the UI thread; every method holds the class lock, so calls from several threads queue up.
 */
public final class Voice {
    private static final String TAG = "Unisend";
    /** MAX_RECORDING in src/ui/voice.rs, which stops the recording there. */
    private static final int MAX_MS = 15 * 60 * 1000;
    /** Only for a recording Rust lost track of: a recorder stopped by this limit loses its file. */
    private static final int HARD_MAX_MS = MAX_MS + 30 * 1000;
    private static final long STALE_MS = 24 * 60 * 60 * 1000;

    private static MediaRecorder recorder;
    private static File recording;
    private static long startedAt;

    private static MediaPlayer player;
    private static String loaded;
    private static boolean finished;

    private static AudioFocusRequest focus;
    private static AudioManager focusOwner;

    private Voice() {}

    public static boolean access(Activity activity) {
        return activity.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED;
    }

    /** Asks for the microphone; once Android stops showing the question, opens the app's system page. */
    public static void ask(Activity activity) {
        SharedPreferences prefs = activity.getSharedPreferences("voice", Activity.MODE_PRIVATE);
        if (prefs.getBoolean("asked", false) && !access(activity)
                && !activity.shouldShowRequestPermissionRationale(Manifest.permission.RECORD_AUDIO)) {
            activity.startActivity(new Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
                    Uri.fromParts("package", activity.getPackageName(), null)));
            return;
        }
        prefs.edit().putBoolean("asked", true).apply();
        activity.requestPermissions(new String[] {Manifest.permission.RECORD_AUDIO}, 3);
    }

    /**
     * Starts recording into the cache and returns the file's path, or null. Ogg Opus where Android
     * has it (10 and later), the voice-note format of WhatsApp and Signal; AAC in MP4 before.
     */
    public static synchronized String start(Activity activity) {
        discard();
        stopPlayer();
        File dir = new File(activity.getCacheDir(), "voice");
        if (!dir.isDirectory() && !dir.mkdirs()) {
            return null;
        }
        File[] old = dir.listFiles();
        if (old != null) {
            long now = System.currentTimeMillis();
            for (File f : old) {
                if (now - f.lastModified() > STALE_MS) {
                    f.delete();
                }
            }
        }
        boolean opus = Build.VERSION.SDK_INT >= 29;
        File file = new File(dir, "voice-" + System.currentTimeMillis() + (opus ? ".ogg" : ".m4a"));
        MediaRecorder r = Build.VERSION.SDK_INT >= 31 ? new MediaRecorder(activity) : new MediaRecorder();
        try {
            r.setAudioSource(MediaRecorder.AudioSource.MIC);
            if (opus) {
                r.setOutputFormat(MediaRecorder.OutputFormat.OGG);
                r.setAudioEncoder(MediaRecorder.AudioEncoder.OPUS);
                // MSC3245's suggested settings for voice.
                r.setAudioSamplingRate(48000);
                r.setAudioEncodingBitRate(24000);
            } else {
                r.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4);
                r.setAudioEncoder(MediaRecorder.AudioEncoder.AAC);
                r.setAudioSamplingRate(44100);
                r.setAudioEncodingBitRate(48000);
            }
            r.setAudioChannels(1);
            r.setMaxDuration(HARD_MAX_MS);
            r.setOutputFile(file.getAbsolutePath());
            r.prepare();
            r.start();
        } catch (IOException | RuntimeException e) {
            Log.w(TAG, "start recording", e);
            r.release();
            file.delete();
            return null;
        }
        gainFocus(activity, AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_EXCLUSIVE, AudioAttributes.USAGE_VOICE_COMMUNICATION);
        recorder = r;
        recording = file;
        startedAt = SystemClock.elapsedRealtime();
        return file.getAbsolutePath();
    }

    /** The loudest sample since the last call, 0 to 32767; 0 when not recording. */
    public static synchronized int level() {
        if (recorder == null) {
            return 0;
        }
        try {
            return recorder.getMaxAmplitude();
        } catch (RuntimeException e) {
            return 0;
        }
    }

    /** Finishes the recording and returns its length in ms, or -1 when it holds nothing (then it is deleted). */
    public static synchronized long stop() {
        if (recorder == null) {
            return -1;
        }
        long length = SystemClock.elapsedRealtime() - startedAt;
        File file = recording;
        boolean ok = true;
        try {
            recorder.stop();
        } catch (RuntimeException e) {
            // Thrown when stopped before any audio was written.
            ok = false;
        }
        recorder.release();
        recorder = null;
        recording = null;
        dropFocus();
        if (!ok) {
            file.delete();
            return -1;
        }
        return Math.min(length, MAX_MS);
    }

    /** Stops recording and deletes what was recorded. */
    public static synchronized void cancel() {
        discard();
    }

    /** Deletes a finished recording the user threw away. */
    public static synchronized void delete(String path) {
        if (path.equals(loaded)) {
            stopPlayer();
        }
        new File(path).delete();
    }

    private static void discard() {
        if (recorder == null) {
            return;
        }
        try {
            recorder.stop();
        } catch (RuntimeException e) {
            // Nothing recorded yet; the file goes either way.
        }
        recorder.release();
        recorder = null;
        recording.delete();
        recording = null;
        dropFocus();
    }

    /**
     * Plays `path` from `from` (a share of its length, 0 to 1) at `speed`, loading it first unless
     * it is the one already loaded. Returns its length in ms, or -1 when it cannot be played.
     */
    public static synchronized int play(Activity activity, String path, float from, float speed) {
        try {
            if (player == null || !path.equals(loaded)) {
                stopPlayer();
                MediaPlayer p = new MediaPlayer();
                p.setAudioAttributes(new AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                        .build());
                // Without a looper on this thread the callback runs on the main one.
                p.setOnCompletionListener(mp -> completed());
                try {
                    p.setDataSource(path);
                    p.prepare();
                } catch (IOException | RuntimeException e) {
                    Log.w(TAG, "load voice message", e);
                    p.release();
                    return -1;
                }
                player = p;
                loaded = path;
            }
            finished = false;
            int length = player.getDuration();
            player.seekTo(Math.max(0, Math.min((int) (from * length), length - 1)));
            gainFocus(activity, AudioManager.AUDIOFOCUS_GAIN_TRANSIENT, AudioAttributes.USAGE_MEDIA);
            player.start();
            player.setPlaybackParams(player.getPlaybackParams().setSpeed(speed));
            return length;
        } catch (RuntimeException e) {
            Log.w(TAG, "play voice message", e);
            stopPlayer();
            return -1;
        }
    }

    private static synchronized void completed() {
        finished = true;
        dropFocus();
    }

    public static synchronized void pause() {
        if (player != null && player.isPlaying()) {
            player.pause();
        }
        dropFocus();
    }

    /** Where playback is, in ms; -1 once it has reached the end or nothing is loaded. */
    public static synchronized int position() {
        if (player == null || finished) {
            return -1;
        }
        return player.getCurrentPosition();
    }

    public static synchronized void seek(int ms) {
        if (player != null) {
            finished = false;
            player.seekTo(ms);
        }
    }

    /** Changes the speed of what is playing; a paused player takes it on its next `play`. */
    public static synchronized void speed(float speed) {
        if (player != null && player.isPlaying()) {
            player.setPlaybackParams(player.getPlaybackParams().setSpeed(speed));
        }
    }

    public static synchronized void stopPlayer() {
        if (player != null) {
            player.release();
            player = null;
            loaded = null;
        }
        dropFocus();
    }

    /** Other apps' sound pauses while a voice message records or plays. */
    private static void gainFocus(Context context, int gain, int usage) {
        dropFocus();
        AudioManager audio = context.getSystemService(AudioManager.class);
        if (audio == null) {
            return;
        }
        focus = new AudioFocusRequest.Builder(gain)
                .setAudioAttributes(new AudioAttributes.Builder().setUsage(usage).build())
                .build();
        audio.requestAudioFocus(focus);
        focusOwner = audio;
    }

    private static void dropFocus() {
        if (focus != null && focusOwner != null) {
            focusOwner.abandonAudioFocusRequest(focus);
        }
        focus = null;
        focusOwner = null;
    }
}
