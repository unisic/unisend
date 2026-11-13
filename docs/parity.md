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
| Encrypted history via recovery code | done | secret storage, key backup v1 |
| Device verification by another device (QR, emoji) | todo | `m.key.verification.*` |
| Chat networks: list, sign in, sign out | part | cloud bridges only: `api.beeper.com/whoami`, `POST /bridge/{name}/start`, then the bridge's mautrix provisioning API (`/v3/whoami`, `login/flows`, `login/start`, `login/step`, `logout`) proxied at `matrix.beeper.com/_matrix/client/unstable/com.beeper.bridge/{name}/`. Steps: form fields, QR or code, browser cookies (in-app browser on Android, paste on desktop). Not yet: cookie sources other than cookies, re-login of a broken account, Beeper's browser-session import |
| On-device bridges | todo | Beeper runs local mautrix bridges inside the client |
| Multiple accounts, account filter, spaces bar | todo | `m.space`, `com.beeper.chat_list_rules` |
| Connection state (offline, syncing, reconnect) | part | sync errors are shown; no dedicated indicator |

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
| Message requests, invitations | todo | `com.beeper.message_request`, invites |
| Merged chats | todo | `com.beeper.union` |
| Drafts synced between devices | todo | `com.beeper.draft` |
| Multi-select, archive all read | todo | local |
| Swipe actions (Android) | todo | local |
| New chat, new group, contacts | todo | bridge `resolve_identifier`, `createRoom` |

## Conversation

| Feature | Status | Protocol |
|---|---|---|
| Text messages, history paging, live updates | done | `m.room.message` |
| Edits applied, "Edited" mark | part | `m.replace`; no edit history |
| Sender grouping, timestamps | done | local |
| Day separators | done | local |
| Unread divider | todo | `m.fully_read` |
| Replies (quote shown) | part | `m.in_reply_to`; no jump to the quoted message |
| Reactions (show, add, remove) | part | `m.reaction`; six quick emoji in the message's panel, a double tap for the heart; no picker, no custom emoji |
| Deleted messages | part | `m.room.redaction`; in encrypted rooms a deleted message disappears after reload (it cannot be told from a deleted reaction) |
| Images, stickers | done | inline thumbnail (800px at most), full-screen view (2048px); plain and encrypted |
| Video, audio, voice notes, files | part | shown with name, length or size; a tap downloads and opens them in the system's app. No inline player |
| Stickers, GIFs, custom emoji | todo | `m.sticker`, `fi.mau.gif`, `m.image_pack` |
| Link previews | part | shows the preview that came with a message (`com.beeper.linkpreviews`); links are underlined and open in the browser. Does not make a preview for a link that has none, and attaches none to messages sent from here |
| Formatted text, mentions, code blocks | part | mentions are drawn in the accent, a message that mentions the user is marked, and the chat list shows an `@` (from the HTML links and `m.mentions`; counted locally, a server cannot see into encrypted rooms). Bold, italics, links and code blocks: todo |
| Send state (sending, sent, delivered, seen, failed) | todo | `com.beeper.message_send_status`, receipts |
| Read receipts of others, typing | todo | `m.receipt`, `m.typing` |
| Polls, locations, contacts | todo | `m.poll.*`, `m.location` |
| Threads | todo | `m.thread` |
| Disappearing messages | todo | `com.beeper.disappearing_timer` |
| Search in chat, media gallery, bookmarks | todo | `com.beeper.bookmark`, `com.beeper.gallery` |
| Chat info: members, admin actions, leave, block | todo | room state, `com.beeper.user_blocked` |
| Chat theme, wallpaper | todo | account data |

## Composer

| Feature | Status | Protocol |
|---|---|---|
| Send text | done | `m.room.message` |
| Reply, edit, delete, copy | done | relations, redaction |
| Forward | todo | new event in the target chat |
| Attachments | part | one file from the picker, any type, 100 MB at most; no paste, drag and drop, camera, caption or thumbnail upload |
| Voice notes | todo | `org.matrix.msc3245.voice` |
| Emoji / GIF / sticker pickers | todo | local, `fi.mau.gif` |
| Formatting shortcuts, mentions autocomplete | todo | local |
| Send later, undo send | todo | `com.beeper.is_scheduled`, local delay |
| AI rewrite, translate, transcription | todo | Beeper AI endpoints |

## App

| Feature | Status | Protocol |
|---|---|---|
| Notifications | part | Android: one per chat for messages the push rules ask about, while the app is not in front; cleared when the chat is read. No sounds per chat, no reply from the notification, tapping opens the app but not the chat. Desktop: none |
| Background connection on Android | part | a foreground service keeps the process connected while the app is in the background; it ends when the app is swiped away, and Android's Doze can delay it. No push (FCM) |
| Settings | part | theme (dark by default), incognito, chat networks, sign out; kept locally, not in `com.beeper.desktop.prefs`. Beeper's chat list, notification, shortcut and AI settings: todo |
| Incognito mode | done | on: `m.read.private`; off: `m.read`. On by default |
| Command bar, hotkeys | todo | local |
| Tray icon, launch at login, deep links | todo | local |
| Media viewer | part | pictures only; no zoom, no gallery |
| PDF preview, storage manager | todo | local |
| Feedback, help hub, subscription banners | todo | Beeper services |
| Emoji rendering | part | bundled bitmap Noto Color Emoji (GPUI cannot draw COLRv1). On Linux, emoji that a system text font also holds (the heart U+2764 among them) draw as that font's outline: cosmic-text asks DejaVu Sans before the emoji font |
| Windows, macOS builds | todo | GPUI targets, not built or tested |

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
