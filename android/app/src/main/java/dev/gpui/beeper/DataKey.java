package dev.gpui.beeper;

import android.os.Build;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyPermanentlyInvalidatedException;
import android.security.keystore.KeyProperties;
import android.util.Log;

import java.nio.ByteBuffer;
import java.security.GeneralSecurityException;
import java.security.KeyStore;
import java.security.UnrecoverableKeyException;

import javax.crypto.AEADBadTagException;
import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;

/**
 * Wraps the store key that Rust generates (src/vault.rs) with an AES-256-GCM key that never leaves
 * the Android Keystore: StrongBox when the phone has one, the TEE otherwise. The key asks for no
 * user authentication and no unlocked screen, so sync in the background works on a locked phone.
 * A blob is the 12-byte IV followed by the ciphertext and its tag. Only {@link Lost} means the data
 * is gone for good (no key under the alias, a key invalidated, a blob the key does not open); Rust
 * signs out on it and retries on any other exception.
 */
public final class DataKey {
    private static final String TAG = "DataKey";
    private static final String ALIAS = "murmur-data";
    private static final String STORE = "AndroidKeyStore";
    private static final String MODE = "AES/GCM/NoPadding";
    private static final int IV = 12;
    private static final int TAG_BITS = 128;

    /** The wrapping key or the blob is gone for good, so is what the blob wrapped. */
    public static final class Lost extends Exception {
        Lost(String message, Throwable cause) {
            super(message, cause);
        }
    }

    private DataKey() {}

    /** Called once per sign-in; a key that is invalidated is replaced, its data is gone already. */
    public static synchronized byte[] wrap(byte[] plain) throws Exception {
        SecretKey key = existing();
        if (key == null) key = create();
        Cipher cipher = Cipher.getInstance(MODE);
        try {
            cipher.init(Cipher.ENCRYPT_MODE, key);
        } catch (KeyPermanentlyInvalidatedException e) {
            keyStore().deleteEntry(ALIAS);
            cipher.init(Cipher.ENCRYPT_MODE, create());
        }
        byte[] iv = cipher.getIV();
        byte[] sealed = cipher.doFinal(plain);
        return ByteBuffer.allocate(iv.length + sealed.length).put(iv).put(sealed).array();
    }

    public static synchronized byte[] unwrap(byte[] blob) throws Exception {
        SecretKey key = existing();
        if (key == null) throw new Lost("no key " + ALIAS, null);
        if (blob.length < IV) throw new Lost("short blob", null);
        Cipher cipher = Cipher.getInstance(MODE);
        try {
            cipher.init(Cipher.DECRYPT_MODE, key, new GCMParameterSpec(TAG_BITS, blob, 0, IV));
        } catch (KeyPermanentlyInvalidatedException e) {
            throw new Lost("key invalidated", e);
        }
        try {
            return cipher.doFinal(blob, IV, blob.length - IV);
        } catch (AEADBadTagException e) {
            // keystore2 throws this only for KM_ERROR_VERIFICATION_FAILED: the blob is not this key's.
            throw new Lost("blob does not authenticate", e);
        }
    }

    private static KeyStore keyStore() throws Exception {
        KeyStore store = KeyStore.getInstance(STORE);
        store.load(null);
        return store;
    }

    /** The key, or null when the alias holds none. */
    private static SecretKey existing() throws Exception {
        try {
            return (SecretKey) keyStore().getKey(ALIAS, null);
        } catch (UnrecoverableKeyException e) {
            // keystore2 returns null for KEY_NOT_FOUND and wraps every other getKeyEntry failure in
            // this, a busy or restarted keystore daemon included, so it is not a lost key.
            throw new GeneralSecurityException(e.getMessage() + describe(e.getCause()), e);
        }
    }

    /** The Keystore's error code behind a failure, for the log; it never holds key material. */
    private static String describe(Throwable cause) {
        if (cause == null) return "";
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU && cause instanceof android.security.KeyStoreException) {
            android.security.KeyStoreException e = (android.security.KeyStoreException) cause;
            return " (code " + e.getNumericErrorCode() + (e.isTransientFailure() ? ", transient)" : ")");
        }
        return " (" + cause.getClass().getName() + ")";
    }

    private static SecretKey create() throws GeneralSecurityException {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            try {
                return generate(true);
            } catch (GeneralSecurityException | RuntimeException e) {
                // StrongBox missing, busy or refusing this spec; the TEE holds the key instead.
                Log.w(TAG, "StrongBox key failed: " + e.getClass().getName());
            }
        }
        return generate(false);
    }

    private static SecretKey generate(boolean strongBox) throws GeneralSecurityException {
        KeyGenParameterSpec.Builder spec = new KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256);
        if (strongBox) spec.setIsStrongBoxBacked(true);
        KeyGenerator generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, STORE);
        generator.init(spec.build());
        return generator.generateKey();
    }
}
