package dev.gpui.beeper;

import android.app.Activity;
import android.app.LocaleManager;
import android.content.Context;
import android.content.res.Configuration;
import android.content.res.Resources;
import android.os.Build;
import android.os.LocaleList;

/**
 * The app's language. Android 13 and later keep one per app, the same one the system settings
 * page (Apps, Unisend, Language) shows; the picker in the app writes it there, and Android then
 * applies it to every text it draws for the app. Earlier versions have none: there the native
 * texts follow the phone. Public static methods are called from Rust over JNI.
 */
public final class Lang {
    /** The locales the native texts were last built in; on the main thread. */
    private static String built;

    private Lang() {}

    /** "pl", "en", "" when none is set and the app follows the phone, or null before Android 13. */
    public static String app(Context context) {
        if (Build.VERSION.SDK_INT < 33) {
            return null;
        }
        LocaleList list = context.getSystemService(LocaleManager.class).getApplicationLocales();
        return list.isEmpty() ? "" : list.get(0).getLanguage();
    }

    /** The phone's language, whatever is set for the app. */
    public static String system(Context context) {
        LocaleList list = Build.VERSION.SDK_INT >= 33
                ? context.getSystemService(LocaleManager.class).getSystemLocales()
                : Resources.getSystem().getConfiguration().getLocales();
        return list.isEmpty() ? "" : list.get(0).getLanguage();
    }

    /** Sets the app's language, "" to follow the phone. Before Android 13 it does nothing. */
    public static void set(Context context, String tag) {
        if (Build.VERSION.SDK_INT < 33) {
            return;
        }
        context.getSystemService(LocaleManager.class).setApplicationLocales(
                tag.isEmpty() ? LocaleList.getEmptyLocaleList() : LocaleList.forLanguageTags(tag));
    }

    /**
     * From the activity, at creation and at each configuration change it handles itself: once the
     * language changed, what Android already shows is posted again in the new one.
     */
    public static void configured(Activity activity, Configuration config) {
        String now = config.getLocales().toLanguageTags();
        if (built != null && !built.equals(now)) {
            Notify.relocale(activity);
        }
        built = now;
    }
}
