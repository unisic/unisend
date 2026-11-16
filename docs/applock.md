# App lock

Settings, Security, App lock: Off, Phone lock (Android only) or App PIN. The setting lives in
`settings.json` (`lock.mode`) and is mirrored in Android preferences (`applock`, `mode`), so a cold
start is locked before Rust draws anything. Code: `src/applock.rs`, `src/applock/pin.rs`,
`src/ui/pinpad.rs`, `android/.../AppLock.java`.

## Phone lock

`BiometricManager.canAuthenticate` and `BiometricPrompt.authenticate` need
`android.permission.USE_BIOMETRIC` (install-time, no prompt). Without it `AuthService.checkPermission`
throws `SecurityException("Must have USE_BIOMETRIC permission")`; until this was declared, every
phone read as having no lock.

- AOSP `core/java/android/hardware/biometrics/BiometricManager.java`: `canAuthenticate(int)` is
  `@RequiresPermission(USE_BIOMETRIC)`; `DEVICE_CREDENTIAL` "can be used to determine if the user
  has a PIN/Pattern/Password set up".
- AOSP `services/core/java/com/android/server/biometrics/AuthService.java`: `canAuthenticate` and
  `authenticate` call `checkPermission()`, which enforces `USE_BIOMETRIC` unless the app holds
  `USE_FINGERPRINT`.
- AOSP `core/java/android/hardware/biometrics/BiometricPrompt.java`: `authenticate` is
  `@RequiresPermission(USE_BIOMETRIC)`; `setAllowedAuthenticators` is API 30, replaces
  `setDeviceCredentialAllowed` (API 29, deprecated).
- AOSP `services/core/java/com/android/server/biometrics/Utils.java`,
  `isValidAuthenticatorConfig`: `BIOMETRIC_WEAK | DEVICE_CREDENTIAL` is a valid combination.

So: API 30+ `BIOMETRIC_WEAK | DEVICE_CREDENTIAL` (fingerprint, face of any class, or the phone's
PIN, pattern, password); API 29 `setDeviceCredentialAllowed(true)`, availability from
`KeyguardManager.isDeviceSecure`; API 26 to 28 the keyguard's credential screen.

Choosing Phone lock asks for the phone's lock once first, so a lock that cannot be passed is never
switched on. If the phone's lock is removed later, the app opens without asking.

## App PIN

- 4 to 8 digits. Only PBKDF2-HMAC-SHA256 of it is kept (600 000 iterations, OWASP's 2023 figure;
  16-byte random salt; aws-lc-rs, already in the tree through rustls), in `applock-pin.json` in the
  app's private data directory, written to a temporary file, synced and renamed, mode 0600.
  Checking compares in constant time (`aws_lc_rs::pbkdf2::verify`) and runs off the UI thread
  (about 0.17 s on a desktop debug build).
- Wrong tries: the first four cost nothing, the fifth waits 30 s, each after doubles up to an hour.
  The count and the time are in the same file, so killing the app does not reset them. A clock set
  back cannot stretch a wait past an hour; a clock set forward shortens it.
- Changing the PIN or leaving App PIN asks for the current PIN; leaving Phone lock asks for the
  phone's lock.
- Signing out, or being signed out by the server, removes the PIN and turns the lock off.
  Clearing the app's storage removes it too; that is also the only way back in after a forgotten
  PIN (it signs out).

## What it protects against

Someone holding the unlocked phone opening Unisend. It does not encrypt anything: the messages,
keys and session stay as readable as before to anyone who can read the app's files (root, a
backup tool with that access, a forensic copy), and four to eight digits are guessed offline in
little time whatever the hash. On Android notifications still show senders and message text while the app is
locked; the system's lock-screen notification settings decide that.

## Desktop

App PIN only. It is asked at start, and again when a window opens after the last one closed: with
the app running on in the background (Linux tray, macOS Dock), closing the window is leaving the
app. Desktop offers no auto-lock choice, so the lock comes at once, as Android's "Immediately"
(`BeeperApp::hidden`, `applock::lock`). While locked, desktop notifications name only the chat
("New message") and offer no Reply, Mark as read or Mute; clicking one opens the window on the PIN,
then the chat.
