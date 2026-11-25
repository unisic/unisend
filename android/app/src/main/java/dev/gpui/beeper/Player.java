package dev.gpui.beeper;

import android.animation.Animator;
import android.animation.AnimatorListenerAdapter;
import android.animation.ValueAnimator;
import android.app.Activity;
import android.app.ActivityOptions;
import android.content.ActivityNotFoundException;
import android.content.Context;
import android.content.Intent;
import android.content.res.ColorStateList;
import android.graphics.Color;
import android.graphics.drawable.ColorDrawable;
import android.graphics.drawable.GradientDrawable;
import android.media.AudioManager;
import android.media.MediaPlayer;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;
import android.util.Log;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.MotionEvent;
import android.view.SurfaceHolder;
import android.view.VelocityTracker;
import android.view.View;
import android.view.ViewConfiguration;
import android.view.ViewGroup;
import android.view.Window;
import android.view.accessibility.AccessibilityManager;
import android.view.animation.DecelerateInterpolator;
import android.widget.FrameLayout;
import android.widget.ImageButton;
import android.widget.LinearLayout;
import android.widget.SeekBar;
import android.widget.TextView;
import android.widget.VideoView;
import androidx.core.graphics.Insets;
import androidx.core.view.ViewCompat;
import androidx.core.view.WindowCompat;
import androidx.core.view.WindowInsetsCompat;
import androidx.core.view.WindowInsetsControllerCompat;
import java.util.Locale;

/**
 * Full-screen player for a video attachment that the app has already downloaded and decrypted.
 * Turns to the video's orientation, hides the system bars, and closes with back or by dragging
 * the video down. A double tap on either side skips 10 s back or ahead, and each further quick tap
 * another 10 s. {@link #play} is called from Rust over JNI.
 */
public final class Player extends Activity {
    private static final String PATH = "path";
    private static final String MIME = "mime";
    private static final String LOOP = "loop";
    private static final String POSITION = "position";
    private static final String PLAYING = "playing";
    private static final String MUTED = "muted";
    private static final String SPEED = "speed";

    private static final long HIDE_MS = 3000;
    private static final long TICK_MS = 250;
    private static final long FADE_MS = 150;
    /** What a double tap on either side skips. */
    private static final int SKIP_MS = 10_000;
    /** How long the skip label stays after the last tap. */
    private static final long SKIP_SHOWN_MS = 600;
    /** The speeds the speed button steps through, as voice notes have them. */
    private static final float[] SPEEDS = {1f, 1.5f, 2f};
    private static final String[] SPEED_LABELS = {"1×", "1.5×", "2×"};
    /** Rate of the spring that settles the video back after a short drag, in 1/s. */
    private static final float SETTLE_RATE = 22f;

    private final Handler handler = new Handler(Looper.getMainLooper());
    private final Runnable hide = () -> showControls(false);
    private final Runnable tick = this::tick;
    private final Runnable single = this::tapped;
    private final Runnable unskip = () -> this.skipLabel.animate().alpha(0f).setDuration(FADE_MS);

    private String path;
    private String mime;
    private boolean loop;
    private int position;
    private boolean playing = true;
    private boolean muted;
    private boolean scrubbing;
    private boolean shown = true;
    /** Index into {@link #SPEEDS}. */
    private int speed;
    /** The last tap, for telling a double tap on a side from two taps. */
    private long lastTap;
    private int lastSide;
    /** What the taps of the current run skipped, for the label. */
    private int skipped;

    private VideoView video;
    /** Set while the video is prepared; VideoView releases it when its surface goes away. */
    private MediaPlayer player;
    private View controls;
    private ImageButton toggle;
    private ImageButton mute;
    private SeekBar seek;
    private TextView elapsed;
    private TextView total;
    private TextView speedButton;
    private TextView skipLabel;
    private ValueAnimator settle;

    /** Opens the player for a file in the app's media cache. False when it cannot start. */
    public static boolean play(Activity activity, String path, String mime, boolean loop) {
        Intent intent = new Intent(activity, Player.class)
                .putExtra(PATH, path)
                .putExtra(MIME, mime)
                .putExtra(LOOP, loop);
        Bundle fade = ActivityOptions.makeCustomAnimation(activity, R.anim.player_in, R.anim.hold).toBundle();
        try {
            activity.startActivity(intent, fade);
            return true;
        } catch (ActivityNotFoundException e) {
            return false;
        }
    }

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        Intent intent = getIntent();
        path = intent.getStringExtra(PATH);
        mime = intent.getStringExtra(MIME);
        loop = intent.getBooleanExtra(LOOP, false);
        // A GIF has no sound to offer.
        muted = loop;
        if (path == null) {
            finish();
            return;
        }
        if (state != null) {
            position = state.getInt(POSITION);
            playing = state.getBoolean(PLAYING, true);
            muted = state.getBoolean(MUTED, muted);
            speed = Math.min(SPEEDS.length - 1, Math.max(0, state.getInt(SPEED)));
        }
        immersive();

        Stage stage = new Stage(this);
        stage.setBackgroundColor(Color.BLACK);
        video = new VideoView(this);
        if (loop) {
            // Looping without sound must not pause the user's music.
            video.setAudioFocusRequest(AudioManager.AUDIOFOCUS_NONE);
        }
        stage.addView(video, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT, Gravity.CENTER));
        skipLabel = new TextView(this);
        skipLabel.setTextColor(Color.WHITE);
        skipLabel.setTextSize(TypedValue.COMPLEX_UNIT_SP, 15);
        skipLabel.setFontFeatureSettings("tnum");
        skipLabel.setPadding(dp(14), dp(8), dp(14), dp(8));
        GradientDrawable pill = new GradientDrawable();
        pill.setColor(0x99000000);
        pill.setCornerRadius(dp(20));
        skipLabel.setBackground(pill);
        skipLabel.setAlpha(0f);
        stage.addView(skipLabel, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.CENTER_VERTICAL));
        controls = buildControls();
        stage.addView(controls, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        setContentView(stage);

        video.setOnPreparedListener(this::prepared);
        video.setOnCompletionListener(mp -> {
            playing = false;
            position = 0;
            showControls(true);
            update();
        });
        video.setOnErrorListener((mp, what, extra) -> {
            // Another app may know the format; this player does not.
            Files.view(this, path, mime == null ? "video/*" : mime);
            finish();
            return true;
        });
        video.getHolder().addCallback(new SurfaceHolder.Callback() {
            @Override
            public void surfaceCreated(SurfaceHolder holder) {}

            @Override
            public void surfaceChanged(SurfaceHolder holder, int format, int width, int height) {}

            @Override
            public void surfaceDestroyed(SurfaceHolder holder) {
                // VideoView releases its player with the surface and prepares a new one after.
                player = null;
            }
        });
        video.setVideoPath(path);
        update();
    }

    private void immersive() {
        Window window = getWindow();
        window.setBackgroundDrawable(new ColorDrawable(Color.BLACK));
        WindowCompat.setDecorFitsSystemWindows(window, false);
        if (android.os.Build.VERSION.SDK_INT >= 28) {
            android.view.WindowManager.LayoutParams attributes = window.getAttributes();
            attributes.layoutInDisplayCutoutMode =
                    android.view.WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
            window.setAttributes(attributes);
        }
        WindowInsetsControllerCompat bars = WindowCompat.getInsetsController(window, window.getDecorView());
        bars.setSystemBarsBehavior(WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
        bars.hide(WindowInsetsCompat.Type.systemBars());
    }

    private void prepared(MediaPlayer mp) {
        player = mp;
        mp.setLooping(loop);
        volume();
        seek.setMax(Math.max(0, video.getDuration()));
        total.setText(clock(video.getDuration()));
        if (position > 0) {
            mp.seekTo(position, MediaPlayer.SEEK_CLOSEST);
        }
        if (playing) {
            video.start();
            applySpeed();
            hideLater();
        }
        update();
    }

    private View buildControls() {
        FrameLayout layer = new FrameLayout(this);

        LinearLayout top = new LinearLayout(this);
        top.setOrientation(LinearLayout.HORIZONTAL);
        top.setGravity(Gravity.CENTER_VERTICAL);
        top.setBackground(scrim(GradientDrawable.Orientation.TOP_BOTTOM));
        ImageButton close = button(R.drawable.player_close, getString(R.string.close));
        close.setOnClickListener(v -> finish());
        top.addView(close);
        View gap = new View(this);
        top.addView(gap, new LinearLayout.LayoutParams(0, 0, 1f));
        mute = button(R.drawable.player_volume, getString(R.string.mute));
        mute.setOnClickListener(v -> {
            muted = !muted;
            volume();
            update();
            hideLater();
        });
        top.addView(mute);
        layer.addView(top, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.TOP));

        LinearLayout bottom = new LinearLayout(this);
        bottom.setOrientation(LinearLayout.HORIZONTAL);
        bottom.setGravity(Gravity.CENTER_VERTICAL);
        bottom.setBackground(scrim(GradientDrawable.Orientation.BOTTOM_TOP));
        toggle = button(R.drawable.player_pause, getString(R.string.pause));
        toggle.setOnClickListener(v -> {
            if (video.isPlaying()) {
                video.pause();
                playing = false;
                handler.removeCallbacks(hide);
            } else {
                video.start();
                playing = true;
                applySpeed();
                hideLater();
            }
            update();
        });
        bottom.addView(toggle);
        elapsed = label();
        bottom.addView(elapsed);
        seek = new SeekBar(this);
        seek.setProgressTintList(ColorStateList.valueOf(Color.WHITE));
        seek.setProgressBackgroundTintList(ColorStateList.valueOf(Color.WHITE));
        seek.setThumbTintList(ColorStateList.valueOf(Color.WHITE));
        seek.setContentDescription(getString(R.string.position));
        seek.setOnSeekBarChangeListener(new SeekBar.OnSeekBarChangeListener() {
            @Override
            public void onProgressChanged(SeekBar bar, int progress, boolean fromUser) {
                if (fromUser) {
                    position = progress;
                    elapsed.setText(clock(progress));
                    // Nearest key frame while the finger moves: exact seeks lag behind it.
                    if (player != null) {
                        player.seekTo(progress, MediaPlayer.SEEK_PREVIOUS_SYNC);
                    }
                }
            }

            @Override
            public void onStartTrackingTouch(SeekBar bar) {
                scrubbing = true;
                handler.removeCallbacks(hide);
            }

            @Override
            public void onStopTrackingTouch(SeekBar bar) {
                scrubbing = false;
                if (player != null) {
                    player.seekTo(bar.getProgress(), MediaPlayer.SEEK_CLOSEST);
                }
                hideLater();
            }
        });
        bottom.addView(seek, new LinearLayout.LayoutParams(0, dp(48), 1f));
        total = label();
        bottom.addView(total);
        speedButton = label();
        speedButton.setText(SPEED_LABELS[speed]);
        speedButton.setContentDescription(getString(R.string.speed));
        speedButton.setGravity(Gravity.CENTER);
        speedButton.setMinWidth(dp(48));
        speedButton.setPadding(dp(8), 0, dp(12), 0);
        TypedValue ripple = new TypedValue();
        getTheme().resolveAttribute(android.R.attr.selectableItemBackgroundBorderless, ripple, true);
        speedButton.setBackgroundResource(ripple.resourceId);
        speedButton.setOnClickListener(v -> {
            speed = (speed + 1) % SPEEDS.length;
            speedButton.setText(SPEED_LABELS[speed]);
            applySpeed();
            hideLater();
        });
        bottom.addView(speedButton, new LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, dp(48)));
        if (loop) {
            // A GIF has no speed worth choosing.
            speedButton.setVisibility(View.GONE);
            total.setPadding(0, 0, dp(16), 0);
        }
        layer.addView(bottom, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.BOTTOM));

        // Insets as if the bars were showing, so the controls do not jump when they come and go.
        ViewCompat.setOnApplyWindowInsetsListener(layer, (v, insets) -> {
            Insets edge = insets.getInsetsIgnoringVisibility(
                    WindowInsetsCompat.Type.systemBars() | WindowInsetsCompat.Type.displayCutout());
            top.setPadding(edge.left + dp(4), edge.top + dp(4), edge.right + dp(4), dp(24));
            bottom.setPadding(edge.left + dp(4), dp(24), edge.right + dp(4), edge.bottom + dp(4));
            return insets;
        });
        return layer;
    }

    private ImageButton button(int icon, String label) {
        ImageButton b = new ImageButton(this);
        b.setImageResource(icon);
        b.setContentDescription(label);
        TypedValue ripple = new TypedValue();
        getTheme().resolveAttribute(android.R.attr.selectableItemBackgroundBorderless, ripple, true);
        b.setBackgroundResource(ripple.resourceId);
        b.setLayoutParams(new LinearLayout.LayoutParams(dp(48), dp(48)));
        return b;
    }

    private TextView label() {
        TextView t = new TextView(this);
        t.setTextColor(Color.WHITE);
        t.setTextSize(TypedValue.COMPLEX_UNIT_SP, 13);
        t.setFontFeatureSettings("tnum");
        t.setPadding(dp(4), 0, dp(4), 0);
        t.setText(clock(0));
        return t;
    }

    private static GradientDrawable scrim(GradientDrawable.Orientation from) {
        return new GradientDrawable(from, new int[] {0x99000000, 0x00000000});
    }

    private void volume() {
        if (player != null) {
            float v = muted ? 0f : 1f;
            player.setVolume(v, v);
        }
    }

    /** The chosen speed, only while playing: MediaPlayer starts a paused video when given a speed. */
    private void applySpeed() {
        if (player == null || !playing) {
            return;
        }
        try {
            player.setPlaybackParams(player.getPlaybackParams().setSpeed(SPEEDS[speed]));
        } catch (IllegalStateException | IllegalArgumentException e) {
            Log.w("Player", "playback speed", e);
        }
    }

    /**
     * A tap in the middle shows or hides the controls at once. On either side it waits for a
     * second tap, which skips; each quick tap after that skips again.
     */
    private void tap(float x, int width) {
        int side = loop ? 0 : x < width * 0.35f ? -1 : x > width * 0.65f ? 1 : 0;
        long now = SystemClock.uptimeMillis();
        boolean again = side != 0 && side == lastSide && now - lastTap < ViewConfiguration.getDoubleTapTimeout();
        handler.removeCallbacks(single);
        lastSide = side;
        lastTap = now;
        if (again) {
            skip(side);
        } else if (side == 0) {
            tapped();
        } else {
            handler.postDelayed(single, ViewConfiguration.getDoubleTapTimeout());
        }
    }

    private void skip(int side) {
        if (player == null) {
            return;
        }
        int from = video.getCurrentPosition();
        int to = Math.max(0, Math.min(video.getDuration(), from + side * SKIP_MS));
        boolean run = skipLabel.getAlpha() > 0 && Integer.signum(skipped) == side;
        skipped = (run ? skipped : 0) + to - from;
        player.seekTo(to, MediaPlayer.SEEK_CLOSEST);
        position = to;
        seek.setProgress(to);
        elapsed.setText(clock(to));

        if (skipped == 0) {
            // Already at the start or the end: nothing to show.
            return;
        }
        skipLabel.setText(String.format(Locale.ROOT, "%s%d s", side < 0 ? "\u2212" : "+", (Math.abs(skipped) + 500) / 1000));
        FrameLayout.LayoutParams at = (FrameLayout.LayoutParams) skipLabel.getLayoutParams();
        at.gravity = Gravity.CENTER_VERTICAL | (side < 0 ? Gravity.START : Gravity.END);
        at.leftMargin = dp(40);
        at.rightMargin = dp(40);
        skipLabel.setLayoutParams(at);
        skipLabel.animate().cancel();
        skipLabel.setAlpha(1f);
        handler.removeCallbacks(unskip);
        handler.postDelayed(unskip, SKIP_SHOWN_MS);
    }

    private void update() {
        toggle.setImageResource(playing ? R.drawable.player_pause : R.drawable.player_play);
        toggle.setContentDescription(getString(playing ? R.string.pause : R.string.play));
        video.setKeepScreenOn(playing);
        mute.setImageResource(muted ? R.drawable.player_muted : R.drawable.player_volume);
        mute.setContentDescription(getString(muted ? R.string.unmute : R.string.mute));
        handler.removeCallbacks(tick);
        if (shown) {
            tick();
        }
    }

    /** Moves the time and the bar along while the controls show. */
    private void tick() {
        if (player != null && !scrubbing) {
            int now = video.getCurrentPosition();
            seek.setProgress(now);
            elapsed.setText(clock(now));
        }
        if (shown && video.isPlaying()) {
            handler.postDelayed(tick, TICK_MS);
        }
    }

    private void showControls(boolean show) {
        shown = show;
        controls.animate().cancel();
        if (show) {
            controls.setVisibility(View.VISIBLE);
            controls.animate().alpha(1f).setDuration(FADE_MS).setListener(null);
            update();
        } else {
            handler.removeCallbacks(tick);
            controls.animate().alpha(0f).setDuration(FADE_MS).setListener(new AnimatorListenerAdapter() {
                @Override
                public void onAnimationEnd(Animator a) {
                    controls.setVisibility(View.GONE);
                }
            });
        }
    }

    /** Hides the controls after a while of playing, unless a screen reader needs them to stay. */
    private void hideLater() {
        handler.removeCallbacks(hide);
        AccessibilityManager a11y = (AccessibilityManager) getSystemService(Context.ACCESSIBILITY_SERVICE);
        if (playing && !scrubbing && (a11y == null || !a11y.isTouchExplorationEnabled())) {
            handler.postDelayed(hide, HIDE_MS);
        }
    }

    private void tapped() {
        showControls(!shown);
        if (shown) {
            hideLater();
        }
    }

    @Override
    protected void onPause() {
        super.onPause();
        if (video == null) {
            return;
        }
        if (player != null) {
            position = video.getCurrentPosition();
        }
        video.pause();
        handler.removeCallbacksAndMessages(null);
        skipLabel.setAlpha(0f);
    }

    @Override
    protected void onResume() {
        super.onResume();
        // After the window was hidden the surface is new and the video prepares again; this is for
        // a pause that kept it, such as the notification shade.
        if (player != null && playing && !video.isPlaying()) {
            video.start();
            applySpeed();
            hideLater();
            update();
        }
    }

    @Override
    protected void onSaveInstanceState(Bundle out) {
        super.onSaveInstanceState(out);
        out.putInt(POSITION, position);
        out.putBoolean(PLAYING, playing);
        out.putBoolean(MUTED, muted);
        out.putInt(SPEED, speed);
    }

    @Override
    protected void onDestroy() {
        handler.removeCallbacksAndMessages(null);
        if (settle != null) {
            settle.cancel();
        }
        if (video != null) {
            video.stopPlayback();
        }
        super.onDestroy();
    }

    @Override
    public void finish() {
        super.finish();
        overridePendingTransition(0, R.anim.player_out);
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }

    /** "m:ss", or "h:mm:ss" from an hour. */
    static String clock(int ms) {
        int s = Math.max(0, ms) / 1000;
        if (s >= 3600) {
            return String.format(Locale.ROOT, "%d:%02d:%02d", s / 3600, s / 60 % 60, s % 60);
        }
        return String.format(Locale.ROOT, "%d:%02d", s / 60, s % 60);
    }

    /**
     * Holds the video and the controls. A tap shows or hides the controls; a drag down moves the
     * video with the finger and closes the player when it is far or fast enough.
     */
    private final class Stage extends FrameLayout {
        private final int slop;
        private final float fling;
        private VelocityTracker velocity;
        private float downX;
        private float downY;
        /** Where the video stood when the finger came down, so a settling video can be caught. */
        private float startY;
        private boolean dragging;
        private boolean moved;

        Stage(Context context) {
            super(context);
            ViewConfiguration config = ViewConfiguration.get(context);
            slop = config.getScaledTouchSlop();
            fling = 1000 * getResources().getDisplayMetrics().density;
        }

        @Override
        public boolean dispatchTouchEvent(MotionEvent ev) {
            if (ev.getActionMasked() == MotionEvent.ACTION_DOWN) {
                if (velocity != null) {
                    velocity.recycle();
                }
                velocity = VelocityTracker.obtain();
                downX = ev.getX();
                downY = ev.getY();
                dragging = false;
                moved = false;
                if (settle != null) {
                    settle.cancel();
                }
                startY = video.getTranslationY();
            }
            if (velocity != null) {
                velocity.addMovement(ev);
            }
            return super.dispatchTouchEvent(ev);
        }

        @Override
        public boolean onInterceptTouchEvent(MotionEvent ev) {
            switch (ev.getActionMasked()) {
                case MotionEvent.ACTION_DOWN:
                    // A video still settling is caught where it is.
                    dragging = startY != 0;
                    return dragging;
                case MotionEvent.ACTION_MOVE:
                    return startDrag(ev);
                default:
                    return false;
            }
        }

        @Override
        public boolean onTouchEvent(MotionEvent ev) {
            switch (ev.getActionMasked()) {
                case MotionEvent.ACTION_DOWN:
                    return true;
                case MotionEvent.ACTION_MOVE:
                    if (dragging || startDrag(ev)) {
                        follow(startY + ev.getY() - downY);
                    } else if (Math.hypot(ev.getX() - downX, ev.getY() - downY) > slop) {
                        moved = true;
                    }
                    return true;
                case MotionEvent.ACTION_UP:
                    if (dragging) {
                        velocity.computeCurrentVelocity(1000);
                        release(velocity.getYVelocity());
                    } else if (!moved) {
                        tap(ev.getX(), getWidth());
                    }
                    return true;
                case MotionEvent.ACTION_CANCEL:
                    if (dragging) {
                        release(0);
                    }
                    return true;
                default:
                    return true;
            }
        }

        private boolean startDrag(MotionEvent ev) {
            float dx = ev.getX() - downX;
            float dy = ev.getY() - downY;
            if (!dragging && !moved && dy > slop && dy > Math.abs(dx)) {
                dragging = true;
                // Starts from the finger, not from where it was a few pixels ago.
                downY = ev.getY();
                getParent().requestDisallowInterceptTouchEvent(true);
            }
            return dragging;
        }

        /** Places the video `y` below its rest; above it, the video only gives a little. */
        private void follow(float y) {
            float offset = y >= 0 ? y : y * 0.2f;
            float progress = Math.min(1f, Math.max(0f, offset) / getHeight());
            video.setTranslationY(offset);
            video.setScaleX(1f - 0.15f * progress);
            video.setScaleY(1f - 0.15f * progress);
            if (controls.getVisibility() == View.VISIBLE && offset > 0) {
                controls.animate().cancel();
                controls.setAlpha(Math.max(0f, 1f - progress * 4f));
            }
        }

        private void release(float speed) {
            float y = video.getTranslationY();
            if (speed > fling || y > getHeight() * 0.2f && speed > -fling / 3) {
                close(y, speed);
            } else {
                back(y, speed);
            }
        }

        /** Carries the video off the bottom at the speed the finger left it. */
        private void close(float y, float speed) {
            float rest = getHeight() - y;
            long ms = (long) Math.min(250, Math.max(100, 1000 * rest / Math.max(speed, fling)));
            settle = ValueAnimator.ofFloat(y, getHeight());
            settle.setDuration(ms);
            settle.setInterpolator(new DecelerateInterpolator());
            settle.addUpdateListener(a -> follow((float) a.getAnimatedValue()));
            settle.addListener(new AnimatorListenerAdapter() {
                @Override
                public void onAnimationEnd(Animator a) {
                    if (video.getTranslationY() >= getHeight()) {
                        finish();
                    }
                }
            });
            settle.start();
        }

        /** A critically damped spring home, seeded with the release velocity: no bounce, no jump. */
        private void back(float y, float speed) {
            settle = ValueAnimator.ofFloat(0f, 1f);
            settle.setDuration(450);
            settle.setInterpolator(null);
            settle.addUpdateListener(a -> {
                float t = a.getAnimatedFraction() * 0.45f;
                float at = (float) ((y + (speed + SETTLE_RATE * y) * t) * Math.exp(-SETTLE_RATE * t));
                follow(a.getAnimatedFraction() >= 1f ? 0f : at);
                if (a.getAnimatedFraction() >= 1f && shown) {
                    controls.setAlpha(1f);
                }
            });
            settle.start();
        }
    }
}
