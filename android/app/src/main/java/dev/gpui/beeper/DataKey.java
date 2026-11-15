package dev.gpui.beeper;

import android.os.Build;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyPermanentlyInvalidatedException;
import android.security.keystore.KeyProperties;
import android.security.keystore.StrongBoxUnavailableException;

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
 * A blob is the 12-byte IV followed by the ciphertext and its tag. Rust sorts the exceptions:
 * KeyPermanentlyInvalidatedException, UnrecoverableKeyException and AEADBadTagException mean the
 * data is lost, anything else is worth another try.
 */
public final class DataKey {
    private static final String ALIAS = "murmur-data";
    private static final String STORE = "AndroidKeyStore";
    private static final String MODE = "AES/GCM/NoPadding";
    private static final int IV = 12;
    private static final int TAG_BITS = 128;

    private DataKey() {}

    /** Called once per sign-in; a key that is gone or invalidated is replaced, its data is gone already. */
    public static synchronized byte[] wrap(byte[] plain) throws Exception {
        SecretKey key;
        try {
            key = existing();
        } catch (UnrecoverableKeyException e) {
            keyStore().deleteEntry(ALIAS);
            key = null;
        }
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
        if (key == null) throw new UnrecoverableKeyException("no key " + ALIAS);
        if (blob.length < IV) throw new AEADBadTagException("short blob");
        Cipher cipher = Cipher.getInstance(MODE);
        cipher.init(Cipher.DECRYPT_MODE, key, new GCMParameterSpec(TAG_BITS, blob, 0, IV));
        return cipher.doFinal(blob, IV, blob.length - IV);
    }

    private static KeyStore keyStore() throws Exception {
        KeyStore store = KeyStore.getInstance(STORE);
        store.load(null);
        return store;
    }

    private static SecretKey existing() throws Exception {
        return (SecretKey) keyStore().getKey(ALIAS, null);
    }

    private static SecretKey create() throws GeneralSecurityException {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            try {
                return generate(true);
            } catch (StrongBoxUnavailableException e) {
                // No secure element on this phone; the TEE holds the key instead.
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
