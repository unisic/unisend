//! Data at rest. On Android each sign-in gets a random 32-byte key; the Android Keystore wraps it
//! (`DataKey.java`) and only the wrapped form is written. The key opens the SDK's store and seals
//! the app's own files that hold chat content, so copied files are unreadable off the device.
//! Threat model, file layout and failure handling: docs/data-encryption.md.
//!
//! Cost: one Keystore call per start and one per sign-in. A sealed file costs one AES-256-GCM pass
//! over its bytes per read or write (hardware AES on arm64) plus 29 bytes; the key is random, so
//! nothing is derived from it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aws_lc_rs::aead::{AES_256_GCM, Aad, NONCE_LEN, Nonce, RandomizedNonceKey};
use zeroize::Zeroizing;

pub const KEY_LEN: usize = 32;
/// The store key as the Keystore wrapped it.
const KEY_FILE: &str = "store.key";
const STORE: &str = "store";
const VERSION: u8 = 1;
const TAG_LEN: usize = 16;
/// Files that hold chat content, by their plain names; sealed, each is `<stem>.enc`.
const SENSITIVE: [&str; 3] = ["session.json", "rooms.json", "drafts.json"];
pub const SESSION: &str = SENSITIVE[0];
pub const DRAFTS: &str = SENSITIVE[2];

pub type Key = Zeroizing<[u8; KEY_LEN]>;

/// How this install keeps its files.
#[derive(Clone)]
pub enum Seal {
    /// Desktop, and Android installs signed in before data was encrypted.
    Plain,
    Sealed(Arc<Key>),
}

impl Seal {
    /// For `SqliteStoreConfig::key`.
    pub fn key(&self) -> Option<&[u8]> {
        match self {
            Seal::Plain => None,
            Seal::Sealed(key) => Some(&key[..]),
        }
    }

    pub fn sealed(&self) -> bool {
        matches!(self, Seal::Sealed(_))
    }

    /// Where the file named `plain` lives under this seal.
    pub fn path(&self, dir: &Path, plain: &str) -> PathBuf {
        match self {
            Seal::Plain => dir.join(plain),
            Seal::Sealed(_) => sealed_path(dir, plain),
        }
    }

    pub fn read(&self, dir: &Path, plain: &str) -> io::Result<Vec<u8>> {
        let raw = fs::read(self.path(dir, plain))?;
        match self {
            Seal::Plain => Ok(raw),
            Seal::Sealed(key) => open(&key[..], plain, &raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{plain}: {e}"))),
        }
    }

    pub fn write(&self, dir: &Path, plain: &str, bytes: &[u8]) -> io::Result<()> {
        match self {
            Seal::Plain => write_private(&dir.join(plain), bytes, false),
            Seal::Sealed(key) => write_private(&sealed_path(dir, plain), &seal(&key[..], plain, bytes)?, false),
        }
    }
}

fn sealed_path(dir: &Path, plain: &str) -> PathBuf {
    dir.join(plain).with_extension("enc")
}

/// Removes the file named `plain` in both forms.
pub fn remove(dir: &Path, plain: &str) {
    let _ = fs::remove_file(dir.join(plain));
    let _ = fs::remove_file(sealed_path(dir, plain));
}

/// Signed in on this device, with either kind of session file.
pub fn signed_in(dir: &Path) -> bool {
    dir.join(SESSION).exists() || sealed_path(dir, SESSION).exists()
}

/// Deletes this account's data that the vault knows of: the store, its key and the sealed and
/// plain files with chat content.
pub fn wipe(dir: &Path) {
    for plain in SENSITIVE {
        remove(dir, plain);
    }
    let _ = fs::remove_dir_all(dir.join(STORE));
    let _ = fs::remove_file(dir.join(KEY_FILE));
}

/// Owner-only, through a temporary file renamed over the old one: a crash leaves one or the other.
/// `sync` flushes it to disk first, for files whose loss loses more than a cache.
pub fn write_private(path: &Path, bytes: &[u8], sync: bool) -> io::Result<()> {
    use std::io::Write as _;
    let tmp = path.with_extension("tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&tmp)?;
    // An older file of that name keeps its mode through `open`.
    #[cfg(unix)]
    file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    file.write_all(bytes)?;
    if sync {
        file.sync_all()?;
    }
    drop(file);
    fs::rename(&tmp, path)
}

/// Why a sealed file could not be opened.
#[derive(Debug, PartialEq)]
pub enum Broken {
    Short,
    Version(u8),
    /// Wrong key, another file's contents, or changed bytes.
    Tag,
}

impl std::fmt::Display for Broken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Broken::Short => f.write_str("truncated"),
            Broken::Version(v) => write!(f, "unknown format {v}"),
            Broken::Tag => f.write_str("does not authenticate"),
        }
    }
}

/// `VERSION`, a random nonce, then the ciphertext and its tag. `label` is authenticated too, so a
/// file cannot be passed off as another.
pub fn seal(key: &[u8], label: &str, plain: &[u8]) -> io::Result<Vec<u8>> {
    let cipher = RandomizedNonceKey::new(&AES_256_GCM, key).map_err(|_| io::Error::other("bad key"))?;
    let mut body = Vec::with_capacity(plain.len() + TAG_LEN);
    body.extend_from_slice(plain);
    let nonce = cipher.seal_in_place_append_tag(Aad::from(label.as_bytes()), &mut body).map_err(|_| io::Error::other("seal failed"))?;
    let mut out = Vec::with_capacity(1 + NONCE_LEN + body.len());
    out.push(VERSION);
    out.extend_from_slice(nonce.as_ref());
    out.extend_from_slice(&body);
    Ok(out)
}

pub fn open(key: &[u8], label: &str, data: &[u8]) -> Result<Vec<u8>, Broken> {
    let (&version, rest) = data.split_first().ok_or(Broken::Short)?;
    if version != VERSION {
        return Err(Broken::Version(version));
    }
    if rest.len() < NONCE_LEN + TAG_LEN {
        return Err(Broken::Short);
    }
    let (nonce, body) = rest.split_at(NONCE_LEN);
    let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| Broken::Short)?;
    let cipher = RandomizedNonceKey::new(&AES_256_GCM, key).map_err(|_| Broken::Tag)?;
    let mut body = body.to_vec();
    let len = cipher.open_in_place(nonce, Aad::from(label.as_bytes()), &mut body).map_err(|_| Broken::Tag)?.len();
    body.truncate(len);
    Ok(body)
}

/// A failure to reach the store key, by what it leaves of the data.
#[derive(Debug, PartialEq)]
pub enum KeyError {
    /// The key can never be had again: the data it protects is gone.
    Lost(String),
    /// Worth trying again later; nothing may be deleted.
    Transient(String),
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyError::Lost(e) => write!(f, "data key lost: {e}"),
            KeyError::Transient(e) => write!(f, "data key unavailable: {e}"),
        }
    }
}

/// The hardware key that wraps the store key. A seam, so the logic around it runs in tests.
pub trait Keystore {
    fn wrap(&self, key: &[u8]) -> Result<Vec<u8>, KeyError>;
    fn unwrap(&self, blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, KeyError>;
}

/// Sorts an exception from `DataKey` (`crate::java_error`'s "name: message (frame)"): these
/// mean the Keystore key is gone or does not fit the blob, anything else may pass.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn classify(java_error: String) -> KeyError {
    const LOST: [&str; 3] = ["KeyPermanentlyInvalidatedException", "UnrecoverableKeyException", "AEADBadTagException"];
    let name = java_error.split(':').next().unwrap_or_default();
    if LOST.iter().any(|lost| name.ends_with(lost)) { KeyError::Lost(java_error) } else { KeyError::Transient(java_error) }
}

/// What the files under `dir` say about how this install keeps its data.
#[derive(Debug, PartialEq)]
pub enum Layout {
    /// Not signed in.
    Fresh,
    /// Signed in with a plain store, from before encryption.
    Legacy,
    Sealed,
}

pub fn layout(dir: &Path) -> Layout {
    if dir.join(KEY_FILE).exists() {
        Layout::Sealed
    } else if dir.join(SESSION).exists() {
        Layout::Legacy
    } else {
        Layout::Fresh
    }
}

/// The seal of the files under `dir`: plain without a stored key, else the key unwrapped.
pub fn load(dir: &Path, keystore: Option<&dyn Keystore>) -> Result<Seal, KeyError> {
    let blob = match fs::read(dir.join(KEY_FILE)) {
        Ok(blob) => blob,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Seal::Plain),
        Err(e) => return Err(KeyError::Transient(format!("{KEY_FILE}: {e}"))),
    };
    let keystore = keystore.ok_or_else(|| KeyError::Lost("no key store on this platform".into()))?;
    let key = keystore.unwrap(&blob)?;
    let key: [u8; KEY_LEN] = key[..].try_into().map_err(|_| KeyError::Lost(format!("{KEY_FILE}: {} bytes", key.len())))?;
    Ok(Seal::Sealed(Arc::new(Zeroizing::new(key))))
}

/// A new key for a new sign-in, stored wrapped before anything is written with it.
pub fn create(dir: &Path, keystore: &dyn Keystore) -> Result<Seal, KeyError> {
    let mut key: Key = Zeroizing::new([0; KEY_LEN]);
    aws_lc_rs::rand::fill(&mut key[..]).map_err(|_| KeyError::Transient("no random bytes".into()))?;
    let blob = keystore.wrap(&key[..])?;
    write_private(&dir.join(KEY_FILE), &blob, true).map_err(|e| KeyError::Transient(format!("{KEY_FILE}: {e}")))?;
    Ok(Seal::Sealed(Arc::new(key)))
}

/// The platform's key store: Android's; desktop has none wired.
pub fn keystore() -> Option<&'static dyn Keystore> {
    #[cfg(target_os = "android")]
    return Some(&android::DataKey);
    // shortcut: desktop keeps the store unencrypted (no hardware key store wired), upgrade with the Secret Service through gpui's credentials API
    #[cfg(not(target_os = "android"))]
    None
}

/// Calls into `dev.gpui.beeper.DataKey`.
#[cfg(target_os = "android")]
mod android {
    use gpui_mobile::android::jni as helpers;
    use jni::objects::{JByteArray, JValue};
    use zeroize::Zeroizing;

    use super::{KeyError, Keystore, classify};

    pub struct DataKey;

    impl Keystore for DataKey {
        fn wrap(&self, key: &[u8]) -> Result<Vec<u8>, KeyError> {
            call("wrap", key).map(|blob| blob.to_vec())
        }

        fn unwrap(&self, blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, KeyError> {
            call("unwrap", blob)
        }
    }

    /// `byte[] DataKey.<method>(byte[])`. Both Java arrays are zeroed before they are let go,
    /// since either may be the bare key.
    fn call(method: &str, input: &[u8]) -> Result<Zeroizing<Vec<u8>>, KeyError> {
        let answer = helpers::with_env(|env| {
            let class = helpers::find_app_class(env, "dev.gpui.beeper.DataKey")?;
            let input_array = env.byte_array_from_slice(input).map_err(|e| e.to_string())?;
            let name = jni::strings::JNIString::new(method);
            let called = env.call_static_method(&class, &name, jni::jni_sig!("([B)[B"), &[JValue::Object(&input_array)]).and_then(|v| v.l());
            let zeros = vec![0i8; input.len()];
            let _ = input_array.set_region(env, 0, &zeros);
            let output = match called {
                Ok(output) => output,
                Err(e) => return Ok(Err(classify(crate::java_error(env, e)))),
            };
            let output = env.cast_local::<JByteArray>(output).map_err(|e| e.to_string())?;
            let bytes = Zeroizing::new(env.convert_byte_array(&output).map_err(|e| e.to_string())?);
            let zeros = vec![0i8; bytes.len()];
            let _ = output.set_region(env, 0, &zeros);
            Ok(Ok(bytes))
        });
        answer.map_err(|e| KeyError::Transient(format!("{method}: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("beeper-gpui-vault-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Stands in for the Keystore: wraps by sealing with a fixed key, or fails as told.
    struct Fake {
        key: [u8; KEY_LEN],
        fail: Cell<Option<KeyError>>,
    }

    impl Fake {
        fn new(byte: u8) -> Self {
            Fake { key: [byte; KEY_LEN], fail: Cell::new(None) }
        }
    }

    impl Keystore for Fake {
        fn wrap(&self, key: &[u8]) -> Result<Vec<u8>, KeyError> {
            if let Some(e) = self.fail.take() {
                return Err(e);
            }
            Ok(seal(&self.key, "wrap", key).unwrap())
        }

        fn unwrap(&self, blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, KeyError> {
            if let Some(e) = self.fail.take() {
                return Err(e);
            }
            // As the Java side reports a blob from another key.
            open(&self.key, "wrap", blob).map(Zeroizing::new).map_err(|_| classify("javax.crypto.AEADBadTagException: null (at x)".into()))
        }
    }

    #[test]
    fn sealed_file_round_trip_and_damage() {
        let (a, b) = ([1u8; KEY_LEN], [2u8; KEY_LEN]);
        let text = b"{\"access_token\":\"not a real one\"}";
        let sealed = seal(&a, "session.json", text).unwrap();
        assert_eq!(sealed.len(), 1 + NONCE_LEN + text.len() + TAG_LEN);
        assert!(!sealed.windows(6).any(|w| w == b"access"));
        assert_eq!(open(&a, "session.json", &sealed).unwrap(), text);
        // A fresh nonce each time.
        assert_ne!(seal(&a, "session.json", text).unwrap(), sealed);
        assert_eq!(open(&b, "session.json", &sealed), Err(Broken::Tag));
        assert_eq!(open(&a, "rooms.json", &sealed), Err(Broken::Tag));
        for i in [0, 1, 1 + NONCE_LEN, sealed.len() - 1] {
            let mut bad = sealed.clone();
            bad[i] ^= 1;
            assert!(open(&a, "session.json", &bad).is_err(), "flipped byte {i}");
        }
        assert_eq!(open(&a, "session.json", &sealed[..sealed.len() - 1]), Err(Broken::Tag));
        assert_eq!(open(&a, "session.json", &sealed[..1 + NONCE_LEN + TAG_LEN - 1]), Err(Broken::Short));
        assert_eq!(open(&a, "session.json", &[]), Err(Broken::Short));
        assert_eq!(open(&a, "session.json", b""), Err(Broken::Short));
        assert_eq!(open(&a, "session.json", &seal(&a, "session.json", b"").unwrap()).unwrap(), b"");
    }

    #[test]
    fn seal_reads_and_writes_its_own_form() {
        let dir = temp("files");
        let plain = Seal::Plain;
        plain.write(&dir, "rooms.json", b"[1]").unwrap();
        assert_eq!(fs::read(dir.join("rooms.json")).unwrap(), b"[1]");
        let sealed = Seal::Sealed(Arc::new(Zeroizing::new([7; KEY_LEN])));
        sealed.write(&dir, "rooms.json", b"[2]").unwrap();
        assert_ne!(fs::read(dir.join("rooms.enc")).unwrap(), b"[2]");
        assert_eq!(sealed.read(&dir, "rooms.json").unwrap(), b"[2]");
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&fs::metadata(dir.join("rooms.enc")).unwrap().permissions()) & 0o777, 0o600);
        let other = Seal::Sealed(Arc::new(Zeroizing::new([8; KEY_LEN])));
        assert_eq!(other.read(&dir, "rooms.json").unwrap_err().kind(), io::ErrorKind::InvalidData);
        remove(&dir, "rooms.json");
        assert!(!dir.join("rooms.json").exists() && !dir.join("rooms.enc").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn layout_and_key_file() {
        let dir = temp("layout");
        let keystore = Fake::new(3);
        assert_eq!(layout(&dir), Layout::Fresh);
        assert!(matches!(load(&dir, Some(&keystore)), Ok(Seal::Plain)));
        // A store and session from before encryption, with no wrapped key: legacy, kept plain.
        fs::create_dir_all(dir.join(STORE)).unwrap();
        fs::write(dir.join(SESSION), b"{}").unwrap();
        assert_eq!(layout(&dir), Layout::Legacy);
        assert!(matches!(load(&dir, Some(&keystore)), Ok(Seal::Plain)));
        assert!(signed_in(&dir));

        let made = create(&dir, &keystore).unwrap();
        assert_eq!(layout(&dir), Layout::Sealed);
        let loaded = load(&dir, Some(&keystore)).unwrap();
        assert_eq!(made.key(), loaded.key());
        assert_eq!(made.key().unwrap().len(), KEY_LEN);
        // Each sign-in gets its own key.
        assert_ne!(create(&dir, &keystore).unwrap().key(), made.key());

        keystore.fail.set(Some(KeyError::Transient("busy".into())));
        assert_eq!(load(&dir, Some(&keystore)).err(), Some(KeyError::Transient("busy".into())));
        assert!(matches!(load(&dir, Some(&Fake::new(4))), Err(KeyError::Lost(_))));
        assert!(matches!(load(&dir, None), Err(KeyError::Lost(_))));
        keystore.fail.set(Some(KeyError::Transient("busy".into())));
        assert!(matches!(create(&dir, &keystore), Err(KeyError::Transient(_))));

        wipe(&dir);
        assert_eq!(layout(&dir), Layout::Fresh);
        assert!(!dir.join(STORE).exists() && !signed_in(&dir));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn java_errors_sorted_by_what_they_lose() {
        let lost = [
            "android.security.keystore.KeyPermanentlyInvalidatedException: Key permanently invalidated (at DataKey.unwrap)",
            "java.security.UnrecoverableKeyException: no key murmur-data (at DataKey.unwrap)",
            "javax.crypto.AEADBadTagException: null (at android.security.keystore2.AndroidKeyStoreCipherSpiBase.engineDoFinal)",
        ];
        for e in lost {
            assert!(matches!(classify(e.into()), KeyError::Lost(_)), "{e}");
        }
        let transient = [
            "android.security.KeyStoreException: System error (at android.security.KeyStore2.getKeyEntry)",
            "java.security.ProviderException: Keystore operation failed: UnrecoverableKeyException (at x)",
            "java.lang.ClassNotFoundException: dev.gpui.beeper.DataKey (at y)",
            "Activity not available",
        ];
        for e in transient {
            assert!(matches!(classify(e.into()), KeyError::Transient(_)), "{e}");
        }
    }
}
