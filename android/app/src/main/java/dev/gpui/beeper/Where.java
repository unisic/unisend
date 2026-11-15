package dev.gpui.beeper;

import android.Manifest;
import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.location.Location;
import android.location.LocationListener;
import android.location.LocationManager;
import android.net.Uri;
import android.os.Bundle;
import android.os.HandlerThread;
import android.os.SystemClock;
import android.provider.Settings;
import android.util.Log;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;

/**
 * One location fix for sharing where the user is, from the GPS through the platform's
 * LocationManager (no Play Services). Called from Rust over JNI, {@link #fix} off the UI thread.
 */
public final class Where {
    private static final String TAG = "Unisend";
    /** A fix this accurate (m) ends the wait early. */
    private static final float GOOD_ENOUGH = 30f;
    /** A cached fix younger than this (ms) may stand in when nothing fresher arrives. */
    private static final long STALE_AFTER = 2 * 60 * 1000;

    private Where() {}

    /** "fine", "coarse" or "none". */
    public static String access(Activity activity) {
        if (granted(activity, Manifest.permission.ACCESS_FINE_LOCATION)) return "fine";
        if (granted(activity, Manifest.permission.ACCESS_COARSE_LOCATION)) return "coarse";
        return "none";
    }

    /** Asks for access; once Android stops showing the question, opens the app's system page. */
    public static void ask(Activity activity) {
        String[] wanted = {Manifest.permission.ACCESS_FINE_LOCATION, Manifest.permission.ACCESS_COARSE_LOCATION};
        SharedPreferences prefs = activity.getSharedPreferences("where", Activity.MODE_PRIVATE);
        boolean asked = prefs.getBoolean("asked", false);
        if (asked && access(activity).equals("none") && !activity.shouldShowRequestPermissionRationale(wanted[0])) {
            activity.startActivity(new Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
                    Uri.fromParts("package", activity.getPackageName(), null)));
            return;
        }
        prefs.edit().putBoolean("asked", true).apply();
        activity.requestPermissions(wanted, 3);
    }

    /**
     * Waits up to timeoutMs for a fix and returns "latitude\tlongitude\taccuracy in metres" (accuracy
     * 0 when unknown), "off" when no location source is switched on, "none" when permission is
     * missing, or "timeout". Takes the best fix seen, stopping early at one within 30 m.
     */
    public static String fix(Activity activity, long timeoutMs) {
        String access = access(activity);
        if (access.equals("none")) return "none";
        LocationManager manager = (LocationManager) activity.getSystemService(Context.LOCATION_SERVICE);
        if (manager == null) return "off";
        List<String> sources = new ArrayList<>();
        // GPS only: the network provider on most phones is Google's, which sends nearby Wi-Fi and cells to Google.
        try {
            if (manager.isProviderEnabled(LocationManager.GPS_PROVIDER)) {
                sources.add(LocationManager.GPS_PROVIDER);
            }
        } catch (SecurityException e) {
            Log.w(TAG, "location sources", e);
            return "none";
        }
        if (sources.isEmpty()) return "off";

        final Location[] best = {null};
        LocationListener listener = new LocationListener() {
            @Override
            public void onLocationChanged(Location l) {
                synchronized (best) {
                    if (better(l, best[0])) best[0] = l;
                    best.notifyAll();
                }
            }

            @Override
            public void onProviderEnabled(String provider) {}

            @Override
            public void onProviderDisabled(String provider) {}

            @Override
            @SuppressWarnings("deprecation")
            public void onStatusChanged(String provider, int status, Bundle extras) {}
        };
        HandlerThread thread = new HandlerThread("where");
        thread.start();
        try {
            for (String source : sources) {
                manager.requestLocationUpdates(source, 0L, 0f, listener, thread.getLooper());
                Location last = manager.getLastKnownLocation(source);
                if (last != null && ageMs(last) < STALE_AFTER) {
                    synchronized (best) {
                        if (better(last, best[0])) best[0] = last;
                    }
                }
            }
            long deadline = SystemClock.elapsedRealtime() + timeoutMs;
            synchronized (best) {
                while (true) {
                    long left = deadline - SystemClock.elapsedRealtime();
                    Location l = best[0];
                    boolean fresh = l != null && ageMs(l) < STALE_AFTER;
                    if (left <= 0 || (fresh && l.hasAccuracy() && l.getAccuracy() <= GOOD_ENOUGH && ageMs(l) < 15000)) break;
                    best.wait(left);
                }
            }
        } catch (SecurityException e) {
            Log.w(TAG, "location updates", e);
            return "none";
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        } finally {
            manager.removeUpdates(listener);
            thread.quitSafely();
        }
        Location l;
        synchronized (best) {
            l = best[0];
        }
        if (l == null) return "timeout";
        return String.format(Locale.ROOT, "%.6f\t%.6f\t%.0f", l.getLatitude(), l.getLongitude(),
                l.hasAccuracy() ? l.getAccuracy() : 0f);
    }

    private static boolean better(Location candidate, Location current) {
        if (current == null) return true;
        long newer = candidate.getElapsedRealtimeNanos() - current.getElapsedRealtimeNanos();
        if (newer > 30_000_000_000L) return true;
        if (newer < -30_000_000_000L) return false;
        float a = candidate.hasAccuracy() ? candidate.getAccuracy() : Float.MAX_VALUE;
        float b = current.hasAccuracy() ? current.getAccuracy() : Float.MAX_VALUE;
        return a < b || (a == b && newer > 0);
    }

    private static long ageMs(Location l) {
        return (SystemClock.elapsedRealtimeNanos() - l.getElapsedRealtimeNanos()) / 1_000_000L;
    }

    private static boolean granted(Activity activity, String permission) {
        return activity.checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED;
    }
}
