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
| Search chats by name | part | names only; Beeper also searches messages, contacts, networks |
| Unread filter | done | local |
| DMs / Groups / Drafts / Unanswered filters | todo | local |
| Avatars, network label, last message, time | done | `m.room.avatar`, `m.bridge`; in a group, each sender's picture from their member event |
| Network icons (per-network glyph) | todo | text label for now |
| Labels with schedules, snooze, reminders | todo | `com.beeper.label.*`, `com.beeper.chats.snoozed`, `com.beeper.chats.reminder` |
| Message requests, invitations | todo | a request is an ordinary chat whose `m.bridge` state has `channel."com.beeper.message_request": true` (read from Beeper Desktop), so it is in the list already, unlabelled; accepting goes through Beeper's SDK and is not known. Matrix invitations are not shown |
| Merged chats | todo | `com.beeper.union` |
| Drafts synced between devices | todo | `com.beeper.draft` |
| Multi-select, archive all read | todo | local |
| Swipe actions (Android) | todo | local |
| New chat, new group, contacts | todo | bridge `resolve_identifier`, `createRoom` |

## Conversation

| Feature | Status | Protocol |
|---|---|---|
| Text messages, history paging, live updates | done | `m.room.message` |
| Edits applied, "Edited" mark | part | `m.replace`, applied by the SDK's timeline; no edit history |
| Sender grouping, timestamps | done | local |
| Day separators | done | the timeline's own |
| Unread divider | done | "New messages" above what arrived since the chat was last read (`m.fully_read`, else the read receipt: matrix-sdk-ui `controller/mod.rs`); opening scrolls to it |
| Local drafts | done | per chat, kept in `drafts.json` (200 chats, 4 KB each), restored on open, "Draft:" in the chat list; not synced |
| Jump to newest | done | button once scrolled over a screen up, counts messages from others that arrived meanwhile |
| Replies (quote shown) | done | `m.in_reply_to`; a quoted message that is not loaded is fetched; a touch goes to it when it is loaded |
| Reactions (show, add, remove) | part | `m.reaction`; Signal's six in a bar over the held message (touch) or from the react button beside it (pointer), a double tap for the heart (short pop at the finger, light tick on Android), the user's own reaction marked and taken back with a second touch. The + at the end of the bar opens the emoji picker for any other emoji. No custom emoji |
| Deleted messages | done | `m.room.redaction`; stays a "Message deleted" row |
| Images, stickers | done | inline thumbnail (800px at most), full-screen view (2048px); plain and encrypted |
| Audio, voice notes, files | part | shown with name, length or size; a tap downloads and opens them in the system's app. No inline player |
| Video | part | thumbnail with play mark and length, a spinner while it downloads; on Android a tap plays it in the app's own full-screen player (play and pause, seek, mute, closes with back or a drag down, turns to the video's shape, immersive); bridged GIFs (`info` `fi.mau.gif` / `fi.mau.loop`) repeat without sound. Desktop opens it in the system's app. Not inline in the timeline, no autoplay |
| Stickers, GIFs, custom emoji | todo | `m.sticker`, `fi.mau.gif`, `m.image_pack` |
| Link previews | part | shows the preview that came with a message (`com.beeper.linkpreviews`); links are underlined and open in the browser. Does not make a preview for a link that has none, and attaches none to messages sent from here |
| Formatted text, mentions, code blocks | part | `formatted_body` (HTML) drawn: bold, italics, strikethrough, underline, links with their own text, quotes, code blocks, lists; previews and notifications read its plain text. Mentions are drawn in the accent, a message that mentions the user is marked, and the chat list shows an `@` (counted locally, a server cannot see into encrypted rooms). Inline code is tinted, not monospace; spoilers are plain text; formatted captions under media are not read |
| Send state (sending, sent, delivered, seen, failed) | done | a message shows at once and is sent from a queue that survives being offline and restarts; a failed one can be sent again or dropped. Under the newest own message: Sending, Sent, Delivered (`com.beeper.message_send_status` with `delivered_to_users`), Seen (`m.receipt`), or the failure with the bridge's reason |
| Read receipts of others, typing | part | "Seen" in a one-to-one chat, "Seen by ..." in a group; who is typing in the open chat, and the user's own typing is sent unless incognito is on (Beeper keeps sending it and asks separately). No per-message receipts, no typing in the chat list |
| Polls, locations, contacts | part | polls of the `org.matrix.msc3381` types (what bridges send) are shown with their votes, counted by the SDK, and can be voted on; the stable `m.poll.*` types only reach the chat list's preview. A poll is made from the attach sheet's Poll entry (question, 2 to 20 options, one or several answers; 140 characters each, the Messenger limit) and the user's own open polls end from the message menu. Locations show a card with coordinates that opens in maps (Android: the maps app; elsewhere OpenStreetMap). Sending the current location is built on Android (attach sheet, Location: one fix, preview card with accuracy, `m.location` with `geo_uri` and MSC3488 fields). Contact files (`.vcf`, as WhatsApp and Signal bridges send them) show as a card with name and numbers; Android adds it through the system's new-contact screen, desktop copies the number. Android can send a contact from the attach sheet as a `text/vcard` file |
| Threads | todo | `m.thread` |
| Disappearing messages | todo | `com.beeper.disappearing_timer` |
| Search in chat, media gallery, bookmarks | todo | `com.beeper.bookmark`, `com.beeper.gallery` |
| Chat info: members, admin actions, leave, block | todo | room state, `com.beeper.user_blocked` |
| Screen lock, screen security (Android) | done | local only: `AppLock.java` asks `BiometricPrompt` (API 29+, biometric or device credential) or the keyguard credential screen (API 26 to 28) after a chosen time in the background; `FLAG_SECURE` for screenshots and recents |
| Chat theme, wallpaper | part | own bubbles in Beeper's nine chat colours (blue a shade darker, for contrast), for every chat, kept on the device as Beeper Desktop keeps it (`chat_appearance` in its local prefs, not account data). One wallpaper for all chats with the dark theme: three dark presets (Graphite, Slate, Carbon, tones of `Palette`) or a photo of the user's own, dimmed 30 to 85%, darkened on import. Text size in five steps, 85 to 130%. No per-chat colour or wallpaper, no app-wide theme colour |

## Composer

| Feature | Status | Protocol |
|---|---|---|
| Send text | done | `m.room.message` |
| Reply, edit, delete, copy | done | relations, redaction |
| Forward | todo | new event in the target chat |
| Attachments | part | one file from the picker, any type, 100 MB at most; Photo and Video tiles in the attach sheet start the system camera (Android); no paste, drag and drop, caption or thumbnail upload |
| Voice notes | todo | `org.matrix.msc3245.voice` |
| Emoji / GIF / sticker pickers | part | emoji done: button in the composer swaps the keyboard for a panel (categories, search by name and shortcode, 32 recents, skin tone), also the sheet behind the reaction bar's +; GIF and sticker pickers todo |
| Formatting shortcuts, mentions autocomplete | todo | local |
| Send later, undo send | todo | `com.beeper.is_scheduled`, local delay |
| AI rewrite, translate, transcription | todo | Beeper AI endpoints |

## App

| Feature | Status | Protocol |
|---|---|---|
| Notifications | part | Android: one conversation notification per chat (last 8 messages, who wrote each, the chat's picture), reply box and Mark as read on the notification, a tap opens that chat, several chats fold under one line. Direct messages, group chats and mentions are separate channels (sounds set in the system's page, linked from Settings). Only for messages the push rules ask about, only while the app is not in front, cleared when the chat is read anywhere. Reply and Mark as read need the app's process: after the app is swiped away they do nothing. Desktop: none |
| Background connection on Android | part | a foreground service keeps the process connected while the app is in the background; it ends when the app is swiped away, and Android's Doze can delay it. No push (FCM) |
| Settings | part | theme (dark by default), incognito, chat networks, sign out; kept locally, not in `com.beeper.desktop.prefs`. Beeper's chat list, notification, shortcut and AI settings: todo |
| Incognito mode | done | on: `m.read.private`; off: `m.read`. On by default |
| Command bar, hotkeys | todo | local |
| Tray icon, launch at login, deep links | todo | local |
| Media viewer | part | pictures only; no zoom, no gallery |
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
| Images, video, files, voice notes | yes | yes | yes | yes | shown; no inline player |
| Polls and votes | yes | yes | groups | no | shown, vote, create, end |
| Locations | yes | to Signal only | from Messenger only | no | shown with coordinates, opens in maps; sent from Android: WhatsApp gets a location, Signal a map link as text, Messenger may refuse, Instagram cannot (the sheet says so first) |
| Contact cards | from WhatsApp | from Signal | no | no | not checked |
| Disappearing messages | no | yes | no | no | timer not shown |
| Typing, read receipts of others | yes | yes | yes | yes | shown in the open chat |
| GIFs, stickers received | not listed | yes | yes | yes | stickers shown; no picker |
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

1. Message requests labelled and accepted, Matrix invitations. M.
2. Typing in the chat list, per-message read state. S.
3. Inline playback for voice notes, audio and video, and recording voice notes. M each.
4. Creating and ending polls, sending a location. S each.
5. Mentions autocomplete; forward; paste and drag and drop attachments. S each.
6. Disappearing-message timer shown. S.
7. New chat from a contact, chat info and group admin. L.
8. Notifications on the desktop, and without the app running (needs Google's push, FCM). L.

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
