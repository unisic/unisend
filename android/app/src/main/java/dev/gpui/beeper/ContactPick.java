package dev.gpui.beeper;

import android.Manifest;
import android.app.Activity;
import android.content.ActivityNotFoundException;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.provider.ContactsContract;
import android.util.Log;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * Chooses a contact in the system's picker and saves its vCard to the cache, to be sent as a file.
 * The picker hands this app read access to the chosen contact only. If the vCard cannot be read
 * with that alone, the Contacts permission is asked for once and the read retried. {@link #pick}
 * is called from Rust over JNI and blocks until the user is done.
 */
public final class ContactPick extends Activity {
    private static final String TAG = "Unisend";
    private static final int PICK = 1;
    private static final int ASK = 2;
    private static final long MAX_BYTES = 256 * 1024;
    private static final AtomicInteger PICKS = new AtomicInteger();
    private static CountDownLatch latch;
    private static volatile String result;

    private Uri picked;

    /** Returns the path of the saved ".vcf", or null when cancelled or unreadable. */
    public static String pick(Activity activity) {
        CountDownLatch wait = new CountDownLatch(1);
        latch = wait;
        result = null;
        activity.startActivity(new Intent(activity, ContactPick.class));
        try {
            wait.await();
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            return null;
        }
        return result;
    }

    /** Opens the system's screen for a new contact; the user saves it there. */
    public static void insert(Activity activity, String name, String phone, String email) {
        Intent intent = new Intent(ContactsContract.Intents.Insert.ACTION, ContactsContract.Contacts.CONTENT_URI);
        intent.putExtra(ContactsContract.Intents.Insert.NAME, name);
        if (!phone.isEmpty()) intent.putExtra(ContactsContract.Intents.Insert.PHONE, phone);
        if (!email.isEmpty()) intent.putExtra(ContactsContract.Intents.Insert.EMAIL, email);
        try {
            activity.startActivity(intent);
        } catch (ActivityNotFoundException e) {
            Log.w(TAG, "no contacts app to save a contact in", e);
        }
    }

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        if (state != null) return;
        try {
            startActivityForResult(new Intent(Intent.ACTION_PICK, ContactsContract.Contacts.CONTENT_URI), PICK);
        } catch (ActivityNotFoundException e) {
            Log.w(TAG, "no contact picker", e);
            finish(null);
        }
    }

    @Override
    protected void onActivityResult(int code, int status, Intent data) {
        if (code != PICK || status != RESULT_OK || data == null || data.getData() == null) {
            finish(null);
            return;
        }
        picked = data.getData();
        read();
    }

    @Override
    public void onRequestPermissionsResult(int code, String[] permissions, int[] grants) {
        if (code == ASK && grants.length > 0 && grants[0] == PackageManager.PERMISSION_GRANTED) {
            read();
        } else {
            Log.w(TAG, "contacts permission refused");
            finish(null);
        }
    }

    private void read() {
        String name = null;
        String key = null;
        try (Cursor c = getContentResolver()
                .query(picked, new String[] {ContactsContract.Contacts.DISPLAY_NAME, ContactsContract.Contacts.LOOKUP_KEY}, null, null, null)) {
            if (c != null && c.moveToFirst()) {
                name = c.getString(0);
                key = c.getString(1);
            }
        } catch (RuntimeException e) {
            Log.w(TAG, "picked contact", e);
            finish(null);
            return;
        }
        if (key == null) {
            finish(null);
            return;
        }
        Uri card = Uri.withAppendedPath(ContactsContract.Contacts.CONTENT_VCARD_URI, Uri.encode(key));
        try (InputStream in = getContentResolver().openInputStream(card)) {
            finish(in == null ? null : save(name, in));
        } catch (SecurityException e) {
            if (checkSelfPermission(Manifest.permission.READ_CONTACTS) == PackageManager.PERMISSION_GRANTED) {
                Log.w(TAG, "contact card", e);
                finish(null);
            } else {
                requestPermissions(new String[] {Manifest.permission.READ_CONTACTS}, ASK);
            }
        } catch (IOException | RuntimeException e) {
            Log.w(TAG, "contact card", e);
            finish(null);
        }
    }

    private String save(String name, InputStream in) throws IOException {
        String base = name == null ? "" : name.replaceAll("[\\\\/:*?\"<>|\\p{Cntrl}]", "").trim();
        if (base.isEmpty()) base = "contact";
        File dir = new File(new File(getCacheDir(), "outgoing"), System.currentTimeMillis() + "-c" + PICKS.getAndIncrement());
        if (!dir.mkdirs()) return null;
        File out = new File(dir, base + ".vcf");
        long total = 0;
        try (OutputStream os = new FileOutputStream(out)) {
            byte[] buffer = new byte[8192];
            for (int n = in.read(buffer); n > 0; n = in.read(buffer)) {
                total += n;
                if (total > MAX_BYTES) {
                    out.delete();
                    return null;
                }
                os.write(buffer, 0, n);
            }
        }
        return out.getAbsolutePath();
    }

    private void finish(String path) {
        result = path;
        CountDownLatch wait = latch;
        if (wait != null) wait.countDown();
        finish();
    }

    @Override
    protected void onDestroy() {
        // Back out of the permission dialog or a killed picker must not leave the caller waiting.
        CountDownLatch wait = latch;
        if (wait != null) wait.countDown();
        super.onDestroy();
    }
}
