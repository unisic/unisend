# Parity with Beeper

Target: what Beeper Desktop 4.3.308 and Beeper Android 4.64.0 do, on GPUI
instead of Electron. The feature list below comes from the shipped apps (UI
strings, hotkey table, Matrix event types in the desktop bundle); the protocol
notes say how each feature is stored so that changes sync with Beeper's own
clients. Nothing from those apps is copied into this repo.

Status: `done` works and is tested, `part` works with the gaps named, `todo`
is not started.

## Account and sync

| Feature | Status | Protocol |
|---|---|---|
| Email-code login | done | `api.beeper.com/user/login*`, then `org.matrix.login.jwt` |
| Other Matrix homeservers (not in Beeper) | part | `.well-known/matrix/client` discovery, `GET /login`, `m.login.password`; https only (http for localhost). Not yet: SSO, OIDC (MSC3861), registration. Networks show a note: bridges and the account API are Beeper's |
| Encrypted history via recovery code | done | secret storage, key backup v1 |
| Device verification by another device (QR, emoji) | todo | `m.key.verification.*` |
| Chat networks: list, sign in, sign out | part | cloud bridges only: `api.beeper.com/whoami`, `POST /bridge/{name}/start`, then the bridge's mautrix provisioning API (`/v3/whoami`, `login/flows`, `login/start`, `login/step`, `logout`) proxied at `matrix.beeper.com/_matrix/client/unstable/com.beeper.bridge/{name}/`. Steps: form fields, QR or code, browser cookies (in-app browser on Android, paste on desktop). Not yet: cookie sources other than cookies, re-login of a broken account, Beeper's browser-session import |
| On-device bridges | todo | Beeper runs local mautrix bridges inside the client |
| Multiple accounts, account filter, spaces bar | todo | `m.space`, `com.beeper.chat_list_rules` |
| Connection state (offline, syncing, reconnect) | part | one quiet line while the connection is lost and retried (5 s doubling to 60 s); no syncing indicator |

## Chat list

| Feature | Status | Protocol |
|---|---|---|
| Inbox, Archive, Low Priority | done | `com.beeper.inbox.done` `{updated_ts, at_order}`, `m.lowpriority` tag |
| Archive / unarchive, new message unarchives | done | same; rule mirrors Desktop's `isRoomDone` |
| Pin / unpin, pins on top | part | `m.favourite` tag; manual order (`com.beeper.favourites_order`) todo |
| Mute / unmute | done | room push rule without notify |
| Mark read / unread | done | `m.marked_unread`, `m.read.private` receipt |
| Unread counts | done | server notification counts |
| Search chats by name | part | names, and a Messages section with hits from every chat (local index, below). Beeper also searches contacts and networks |
| Unread filter | done | local |
| DMs / Groups / Drafts / Unanswered filters | todo | local |
| Avatars, network label, last message, time | done | `m.room.avatar`, `m.bridge`; in a group, each sender's picture from their member event |
| Network icons (per-network glyph) | todo | text label for now |
| Labels with schedules, snooze, reminders | todo | `com.beeper.label.*`, `com.beeper.chats.snoozed`, `com.beeper.chats.reminder` |
| Message requests, invitations, room upgrades | done | a request is a chat whose `m.bridge` state has `channel."com.beeper.message_request": true`: labelled in the list, and in the chat Block (`com.beeper.block_user`), Delete, Accept (`com.beeper.accept_message_request` `{}`, mautrix-go bridgev2/portal.go) in place of the composer; no receipts or typing until it is accepted. Matrix invitations from the stripped state, with Block (ignore), Decline, Accept. A tombstoned room points to its successor and leaves the list once that is joined. Groups in common not shown |
| Merged chats | todo | `com.beeper.union` |
| Drafts synced between devices | todo | `com.beeper.draft` |
| Multi-select, archive all read | part | local: selection mode in a chat (copy with sender and time, forward, delete for me, delete for everyone when all are yours); archive all read is todo |
| Swipe actions | done | a chat row swiped either way runs the action chosen in Settings (default: left archives, right marks read or unread; delete asks first); local setting |
| New chat, new group, contacts | done | + in the chat list: Beeper/Matrix (user directory, a Matrix ID; an existing DM is reused) or a network login, then a contact, a search result or a typed phone number or username. bridgev2 provisioning `GET capabilities`, `GET contacts`, `POST search_users`, `GET resolve_identifier/{id}`, `POST create_dm/{id}`, `POST create_group/{type}` (name and members as the network's capabilities allow); Matrix `createRoom` (private, encrypted). Contact list capped at 5000, 200 rows drawn; no avatars in the list |
| Leave or delete a chat | done | last row of the chat's sheet, after a dialog. Matrix: leave and forget. A bridged chat whose `com.beeper.room_features` says `delete_chat` gets `com.beeper.delete_chat` (the bridge leaves the group and deletes the chat on the user's account), then the user leaves once the bridge deleted the room or after 60 s. Other bridged chats are only left here: bridgev2 drops Matrix leaves unless `bridge_matrix_leave` |

## Conversation

| Feature | Status | Protocol |
|---|---|---|
| Text messages, history paging, live updates | done | `m.room.message` |
| Edits applied, "Edited" mark | part | `m.replace`, applied by the SDK's timeline; no edit history |
| Sender grouping, timestamps | done | local |
| Day separators | done | the timeline's own |
| Unread divider | done | "New messages" above what arrived since the chat was last read (`m.fully_read`, else the read receipt: matrix-sdk-ui `controller/mod.rs`); opening scrolls to it. It stays put while the chat is open, needs a message from someone else under it, and reading moves `m.fully_read` too, so a read chat reopens without it |
| Local drafts | done | per chat, kept in `drafts.json` (200 chats, 4 KB each), restored on open, "Draft:" in the chat list; not synced |
| Jump to newest | done | button once scrolled over a screen up, counts messages from others that arrived meanwhile |
| Replies (quote shown) | done | `m.in_reply_to`; a quoted message that is not loaded is fetched; a touch goes to it when it is loaded |
| Reactions (show, add, remove) | part | `m.reaction`; Signal's six in a bar over the held message (touch) or from the react button beside it (pointer), a double tap for the heart (its badge rises under the bubble, light tick on Android), the user's own reaction marked and taken back with a second touch. The + at the end of the bar opens the emoji picker for any other emoji. A chat whose bridge lists `allowed_reactions` (`com.beeper.room_features`) gets only those in the bar and in the + sheet, the + only when more remain, and a refusal for any other pick; `reaction` below 1 hides reacting and the double tap does nothing. No custom emoji |
| Deleted messages | done | `m.room.redaction`; stays a "Message deleted" row |
| Images, stickers | done | inline thumbnail (800px at most), full-screen view (2048px); plain and encrypted |
| Audio, voice notes, files | part | shown with name, length or size; a tap downloads and opens them in the system's app. Voice notes and audio play inside the bubble on Android (waveform, seek, 1×/1.5×/2×, one at a time); desktop shows the waveform and opens the system's player |
| Video | part | thumbnail with play mark and length, a spinner while it downloads; on Android a tap plays it in the app's own full-screen player (play and pause, seek, mute, closes with back or a drag down, turns to the video's shape, immersive); bridged GIFs (`info` `fi.mau.gif` / `fi.mau.loop`) repeat without sound. Desktop opens it in the system's app. Not inline in the timeline, no autoplay |
| Stickers, GIFs, custom emoji | todo | `m.sticker`, `fi.mau.gif`, `m.image_pack` |
| Link previews | part | shows the preview that came with a message (`com.beeper.linkpreviews`); links are underlined and open in the browser. Does not make a preview for a link that has none, and attaches none to messages sent from here |
| Formatted text, mentions, code blocks | part | `formatted_body` (HTML) drawn: bold, italics, strikethrough, underline, links with their own text, quotes, code blocks, lists; previews and notifications read its plain text. A mention of the user is drawn in the accent, one of someone else in semibold, a message that mentions the user is marked, and the chat list shows an `@` (counted locally, a server cannot see into encrypted rooms). Inline code is tinted, not monospace; spoilers are plain text; formatted captions under media are not read |
| Send state (sending, sent, delivered, seen, failed) | done | a message shows at once and is sent from a queue that survives being offline and restarts; a failed one can be sent again or dropped. Under the newest own message: Sending, Sent, Delivered (`com.beeper.message_send_status` with `delivered_to_users`), Seen (`m.receipt`), or the failure with the bridge's reason. Settings choose how: words, ticks, circles, readers' pictures, or only the time (a failure always shows) |
| Read receipts of others, typing | part | "Seen" in a one-to-one chat, "Seen by ..." in a group; under others' messages (always, or after a tap) whether the user has read them and whether the sender sees that (public `m.read`) or not (`m.read.private`); who is typing in the open chat, and the user's own typing is sent unless incognito is on (Beeper keeps sending it and asks separately). No per-message receipts, no typing in the chat list |
| Polls, locations, contacts | part | polls of the `org.matrix.msc3381` types (what bridges send) are shown with their votes, counted by the SDK, and can be voted on; the stable `m.poll.*` types only reach the chat list's preview. A poll is made from the attach sheet's Poll entry (question, 2 to 20 options, one or several answers; 140 characters each, the Messenger limit) and the user's own open polls end from the message menu. Locations show a card with coordinates that opens in maps (Android: the maps app; elsewhere OpenStreetMap). Sending the current location is built on Android (attach sheet, Location: one fix, preview card with accuracy, `m.location` with `geo_uri` and MSC3488 fields). Contact files (`.vcf`, as WhatsApp and Signal bridges send them) show as a card with name and numbers; Android adds it through the system's new-contact screen, desktop copies the number. Android can send a contact from the attach sheet as a `text/vcard` file. Poll, Location and Contact show only where the bridge's `com.beeper.room_features` takes them (`poll` and `location_message` at least partial, `text/vcard` in `m.file` not rejected); a room without that event offers all |
| Threads | todo | `m.thread` |
| Disappearing messages | done | state event `com.beeper.disappearing_timer` (`{type, timer}` in ms, `{}` off) shown under the chat's name and as a line when it changes; set from the chat's sheet with the timers the bridge lists in `com.beeper.room_features` `disappearing_timer` (read only, with the reason, where it lists none or power levels forbid it); a message whose content or bridge status carries the timer gets a timer mark. The bridge's own notice of a change is hidden as the line already tells it. Expired messages are redacted by the bridge |
| Search in chat | part | local SQLite FTS5 index of the text this device decrypts (sync, history pages, the open chat; edits and deletions followed), newest 50 000 messages, words matched as prefixes with accents folded (not `ł`). History this device never loaded is not found; the server's `/search` is not used |
| Media gallery, bookmarks | todo | `com.beeper.bookmark`, `com.beeper.gallery` |
| Chat info: members, admin actions, leave, block | part | Tapping the chat's name opens it (a panel beside the chat on a wide window): picture, name, network, topic, encryption, mute/pin/archive/unread, members with roles from power levels (at most 200), and Media, Links and Files from the newest 2000 events (at most 200 each). A DM shows the person's bridge identifiers (`com.beeper.bridge.identifiers` profile field, mautrix bridgev2), or the Matrix ID of a native contact. Disappearing messages and Leave or Delete chat are rows under the encryption line, the same as in the chat's sheet. On Instagram, WhatsApp, Telegram, X and Bluesky a DM gets "Open in {app}" (info screen and the chat's sheet, opened by a long press or right click on its row), an https profile link the network's app claims. No admin actions or blocking |
| App lock, screen security | done | local only, see `docs/applock.md`. Off, Phone lock (Android: `BiometricPrompt` with `BIOMETRIC_WEAK \| DEVICE_CREDENTIAL` on API 30+, device credential allowed on 29, the keyguard credential screen on 26 to 28) or App PIN (4 to 8 digits, PBKDF2-HMAC-SHA256, growing wait after 5 wrong tries, kept across restarts); leaving a mode asks for its credential; after a chosen time in the background on Android, at start on desktop. `FLAG_SECURE` for screenshots and recents (Android). Notifications still show message text while locked |
| Chat theme, wallpaper | part | own bubbles in Beeper's nine chat colours (blue a shade darker, for contrast), for every chat, kept on the device as Beeper Desktop keeps it (`chat_appearance` in its local prefs, not account data). One wallpaper for all chats with the dark theme: three dark presets (Graphite, Slate, Carbon, tones of `Palette`) or a photo of the user's own, dimmed 30 to 85%, darkened on import. Text size in five steps, 85 to 130%. No per-chat colour or wallpaper, no app-wide theme colour |

## Composer

| Feature | Status | Protocol |
|---|---|---|
| Send text | done | `m.room.message` |
| Reply, edit, delete, copy | done | relations, redaction; Reply, Edit and Delete for everyone hide where `com.beeper.room_features` says the network takes none (`reply`, `edit`, `delete` below 1) or the message is past `edit_max_age` or `delete_max_age` |
| Forward | done | the content sent again as a new event in up to 5 chats, relations and `m.mentions` dropped, the same `mxc://` file (and key) without a new upload, as Element's `ForwardDialog`; no forwarded marker (MSC2723 is open). Polls and view-once media are not forwarded |
| Delete for me | done | local only: hidden on this device, per account, the last 5000 kept in `hidden-<user>.txt`; Undo for 5 s. Delete for everyone is the redaction |
| Attachments | part | files from the picker, any type, 100 MB at most (the attach sheet's File button on Android, the paperclip's menu on desktop, which also offers GIF and Poll); Photo and Video tiles in the attach sheet start the system camera (Android); photos and videos first open a preview (large items with paging, a caption per item as an MSC2530 body, or as a text message right after the file where the bridge's `caption` level for that type is below 1 (Instagram), remove, HD switch, Send). HD off scales pictures to 2048 px, JPEG q85, EXIF applied then dropped; HD on sends the original pixels. Every photo and video loses its metadata (EXIF, XMP, GPS, maker notes, MP4 location; orientation kept; WebM and MKV videos are refused) and goes out as `image.<ext>` or `video.<ext>`; documents keep their name. Holding a photo or video in the attach sheet shows it whole until the finger lifts. File shows only where the bridge's `m.file` takes something. No paste, drag and drop or thumbnail upload |
| Voice notes | part | Android: hold the microphone to record, slide left to cancel, up to lock; Ogg Opus (AAC in MP4 before Android 10), 15 minutes at most; `m.audio` with `org.matrix.msc3245.voice` and `org.matrix.msc1767.audio` (ms, 64 samples 0 to 1024); the microphone hides where the bridge rejects `audio/ogg` voice notes. Not on desktop |
| Emoji / GIF / sticker pickers | part | emoji done: button in the composer swaps the keyboard for a panel (categories, search by name and shortcode, 32 recents, skin tone), also the sheet behind the reaction bar's +; it offers only emoji whose characters the emoji font in use maps (read once from its `cmap` at start); GIFs from KLIPY (needs `KLIPY_API_KEY` at build time, else hidden): trending, debounced search, static previews, tap to send as `m.video` MP4 with `fi.mau.gif` (a GIF file `m.image` in Messenger and Instagram chats), hidden where the bridge rejects that type, no categories; sticker picker todo |
| Formatting shortcuts, mentions autocomplete | part | Sending: CommonMark (bold, italics, `~~strike~~`, inline code, code blocks, quotes, lists, headings, http/https/mailto links) goes out as `formatted_body` HTML with a line break as `<br>`, as Element sends it; plain text stays body-only; typed HTML stays text. `@` in the composer lists the chat's members (picture and name, those who wrote lately first, narrowed as you type; tap, or Up/Down/Enter/Tab); a pick is one unit in the composer and leaves as a pill plus `m.mentions`, which the WhatsApp, Signal and Meta bridges turn into native mentions. Not yet: `@room`, keeping pills when an edit starts, toolbar or shortcuts for formatting |
| Text input, keyboard | done | Android: autocorrect and prediction from the text around the caret, the composing word underlined, the system spell checker's misspelled words dotted, and an Incognito keyboard switch in Privacy (secrets always incognito) |
| Send later, undo send | todo | `com.beeper.is_scheduled`, local delay |
| AI rewrite, translate, transcription | todo | Beeper AI endpoints |

## App

| Feature | Status | Protocol |
|---|---|---|
| Notifications | part | Android: one conversation notification per chat (last 8 messages, who wrote each, the chat's picture cut round), tied to the chat's share shortcut so Android shows it as a conversation, reply box and Mark as read on the notification, a tap opens that chat, several chats fold under one line. Direct messages, group chats and mentions are separate channels (sounds set in the system's page, linked from Settings). Only for messages the push rules ask about, only while the app is not in front, cleared when the chat is read anywhere. Reply and Mark as read need the app's process: after the app is swiped away they do nothing. Desktop, while the app runs and its window is not in front: one notification per chat replaced in place (last 8 messages as `sender: text`), closed when the chat is read anywhere, Mark as read and Mute buttons, a click brings the window up on that chat. Linux: `org.freedesktop.Notifications` on the session bus (chat picture, `message-new-instant` sound once per burst, KDE's inline reply box). Windows: local WinRT toasts under an AppUserModelID the app registers in HKCU, with a reply box; buttons answer only while the app runs. macOS: through GPUI's `UNUserNotificationCenter`, only from the `.app` bundle, no reply box, picture or sound. No push service on any desktop |
| Share into the app (Android) | part | other apps' share sheets: text, pictures, videos, any files (`ACTION_SEND`, `ACTION_SEND_MULTIPLE`). A "Send to" screen lists chats most recent first with search, the shared files and an optional message, sent before the files. Text alone goes into the chosen chat's message box, not sent. At most 20 files and 200 MB per share, 100 MB per file; the rest is left out and the screen says how many. The 4 most recent chats are Direct Share targets. One chat per share; no thumbnails of the shared pictures. Desktop: none |
| Background connection on Android | part | a foreground service (type `specialUse`, no daily limit) keeps the process connected while the app is in the background; it ends when the app is swiped away, and Android's Doze can delay it. Its quiet notice is posted once per run, so one swiped away stays away. Settings, Notifications, "Stay connected in the background" turns it off (then messages arrive only while the app is open). No push (FCM, UnifiedPush) |
| Settings | part | theme (dark by default), incognito, chat networks, sign out; kept locally, not in `com.beeper.desktop.prefs`. Beeper's chat list, notification, shortcut and AI settings: todo |
| Incognito mode | done | on: `m.read.private`; off: `m.read`. On by default |
| Interface language | part | English and Polish, following the system language (Android's locale; on desktop `LC_ALL`, `LC_MESSAGES`, `LANG`). No in-app switch. Glossary and how to add a string: `docs/i18n.md` |
| Command bar, hotkeys | todo | local |
| Tray icon, launch at login, deep links | todo | local |
| Media viewer | part | pictures only; pinch, double-tap and ctrl+wheel zoom, pan; a sideways swipe or Left/Right pages through the chat's loaded pictures (reaching the oldest asks for older history once); a swipe up or down closes; a tap shows or hides a bar with sender and time, the caption, and Reply, Forward, Save to gallery (Android 10+, desktop Pictures/Unisend), Share (Android). No position count, no videos in the row |
| PDF preview, storage manager | todo | local |
| Feedback, help hub, subscription banners | todo | Beeper services |
| Emoji rendering | part | bundled bitmap Noto Color Emoji (GPUI cannot draw COLRv1). On Linux, emoji that a system text font also holds (the heart U+2764 among them) draw as that font's outline: cosmic-text asks DejaVu Sans before the emoji font |
| Windows, macOS builds | part | a CI workflow builds them on GitHub's runners (`.github/workflows/build.yml`, never run: nothing is pushed); `packaging/macos` makes the `.app`. Not built locally: GPUI's Windows shaders need `fxc.exe` on a Windows host, Apple's SDK needs a Mac. Unsigned builds meet SmartScreen and Gatekeeper warnings |
| iOS | todo | gpui-mobile has an iOS back end; this app has no iOS entry point, no Xcode project, and no counterpart of its Android helpers (in-app browser sign-in, notifications, background connection, which iOS allows only through push). Needs an Apple Developer account to reach anyone |

## Per network

What the bridges for the networks in daily use send or accept, against what this client does.
Bridge features are from the mautrix roadmaps (whatsapp, signal, meta, which serves both
Messenger and Instagram); what the client does is from this file and the source. "Shown" means the
conversation displays it.

Verified in the source: typing, read receipts of others and membership changes are not read. Everything else below is a gap by the rows above.

| Feature | WhatsApp | Signal | Messenger | Instagram | Client |
|---|---|---|---|---|---|
| Replies, reactions, deletes | yes | yes | yes | yes | done |
| Edits, mentions | no | yes | yes | yes | done |
| Images, video, files, voice notes | yes | yes | yes | yes | shown; voice notes recorded and played on Android, video in the app's own player on Android; no inline video |
| Polls and votes | yes | yes | groups | no | shown, vote, create, end |
| Locations | yes | to Signal only | from Messenger only | no | shown with coordinates, opens in maps; sent from Android: WhatsApp gets a location, Signal a map link as text, Messenger and Instagram publish no `location_message`, so Location is not offered there; the name-based note is kept for Signal's partial support and for bridges without `com.beeper.room_features` |
| Contact cards | from WhatsApp | from Signal | no | no | not checked |
| Disappearing messages | yes: 24 h, 7 d, 90 d | yes: any timer, after read | shown, not set from Matrix | no | shown and set where the bridge allows |
| Typing, read receipts of others | yes | yes | yes | yes | shown in the open chat |
| GIFs, stickers received | not listed | yes | yes | yes | stickers shown; GIFs sent from the KLIPY picker: WhatsApp `GifPlayback` and Signal's GIF flag from `fi.mau.gif`, Meta gets the GIF file (not checked on a bridge) |
| Story, reel and profile shares | no | no | yes | stories | not checked how they arrive |
| Group changes (title, avatar, members) | yes | yes | yes | yes | shown as lines; a bridge's bookkeeping memberships are hidden as Beeper hides them |
| Group admin from Matrix | invite, leave, kick | all but join | invite, kick, rename | rename | none |
| History of an old chat | yes | no | unencrypted only | yes | only what the bridge sent |

The last row matters for Messenger: the meta bridge's roadmap lists reading the chat backup of
encrypted Messenger chats as not done, which is why their history does not come over.
Message state and message requests are not in the bridge roadmaps. How Beeper's own app reads
them was taken from its desktop bundle, and the state events were checked against the account:
190 of them among the newest 100 events of 24 chats, all `SUCCESS`, 73 with `delivered_to_users`.

## Next, by what a daily user of these networks would notice

1. Typing in the chat list, per-message read state. S.
2. Inline video playback. M.
3. Creating and ending polls, sending a location. S each.
4. Mentions autocomplete; forward; paste and drag and drop attachments. S each.
6. New chat from a contact, chat info and group admin. L.
7. Notifications without the app running (needs Google's push, FCM). L.

## How Beeper stores chat state

- Archive: room account data `com.beeper.inbox.done`. Archived means
  `at_order >=` the `com.beeper.hs.order` (event `unsigned`) of the newest
  message that counts as unread; without `at_order`, `updated_ts >=` the last
  activity and the chat is read. Pinned chats are never archived. Unarchive
  writes `{}`. Archiving marks the chat read unless
  `com.beeper.mark_read_when_archive` is `{enabled: false}` (Desktop also
  gates that setting on a feature flag, which this client does not read).
- Pin: tag `m.favourite` (order 1). Low priority: tag `m.lowpriority` (order 0).
- Mute: a push rule of kind `room` whose actions do not notify.
- Marked unread: `m.marked_unread` `{unread}`.
