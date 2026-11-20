package dev.gpui.beeper;

import android.content.Context;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.NetworkInfo;
import android.os.Build;

/** Which kind of network the phone uses now, for media auto-download ({@code src/ui/download.rs}). */
public final class Net {
    private Net() {}

    /** 0 Wi-Fi or another unmetered network (also none at all), 1 a metered one, 2 roaming. */
    public static int kind(Context context) {
        ConnectivityManager manager = context.getSystemService(ConnectivityManager.class);
        Network network = manager == null ? null : manager.getActiveNetwork();
        NetworkCapabilities caps = network == null ? null : manager.getNetworkCapabilities(network);
        if (caps == null) {
            return 0;
        }
        boolean roaming;
        if (Build.VERSION.SDK_INT >= 28) {
            roaming = !caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_ROAMING);
        } else {
            NetworkInfo info = manager.getNetworkInfo(network);
            roaming = info != null && info.isRoaming();
        }
        if (roaming) {
            return 2;
        }
        return caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED) ? 0 : 1;
    }
}
