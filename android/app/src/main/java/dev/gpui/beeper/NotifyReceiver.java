package dev.gpui.beeper;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ApplicationInfo;
import android.os.Bundle;
import androidx.core.app.RemoteInput;

/** The reply box and the mark-as-read and mute buttons of a message notification. */
public final class NotifyReceiver extends BroadcastReceiver {
    /**
     * Debug builds only, from {@code adb shell run-as dev.gpui.beeper am broadcast
     * --user 0 -n dev.gpui.beeper/.NotifyReceiver -a dev.gpui.beeper.TEST --es chat junk}: shows a
     * group conversation and a direct one (chat plus "2") with made-up messages, to see how
     * notifications look and act without a real chat. With {@code --es reply text} it replies to
     * the chat as the reply box does. A chat that is not a room ID is not sent anywhere.
     */
    static final String ACTION_TEST = "dev.gpui.beeper.TEST";

    private static void test(Context context, String chat) {
        long now = System.currentTimeMillis();
        Notify.message(context, chat, "Test group", true, "Ann", "First message of three", now - 2000, false, "", "TG", false, Notify.SHOW_ALL);
        Notify.message(context, chat, "Test group", true, "Bob", "@you second, mentions you", now - 1000, true, "", "TG", false, Notify.SHOW_ALL);
        Notify.message(context, chat, "Test group", true, "Ann", "Third, and the one that makes the sound", now, false, "", "TG", true, Notify.SHOW_ALL);
        Notify.message(context, chat + "2", "Test person", false, "Test person", "A direct message", now, false, "", "TP", true, Notify.SHOW_ALL);
    }

    @Override
    public void onReceive(Context context, Intent intent) {
        String chat = intent.getStringExtra(Notify.EXTRA_CHAT);
        if (chat == null || intent.getAction() == null) {
            return;
        }
        boolean debuggable = (context.getApplicationInfo().flags & ApplicationInfo.FLAG_DEBUGGABLE) != 0;
        if (debuggable && intent.getAction().equals(ACTION_TEST)) {
            if (intent.hasExtra("reply")) {
                Notify.reply(context, chat, intent.getStringExtra("reply"));
            } else {
                test(context, chat);
            }
        } else if (intent.getAction().equals(Notify.ACTION_READ)) {
            Notify.read(context, chat);
        } else if (intent.getAction().equals(Notify.ACTION_MUTE)) {
            Notify.mute(context, chat);
        } else if (intent.getAction().equals(Notify.ACTION_REPLY)) {
            Bundle typed = RemoteInput.getResultsFromIntent(intent);
            CharSequence text = typed == null ? null : typed.getCharSequence(Notify.KEY_REPLY);
            if (text != null && text.toString().trim().length() > 0) {
                Notify.reply(context, chat, text);
            }
        }
    }
}
