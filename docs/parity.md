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
| Add / remove chat networks | todo | bridge login flows, `fi.mau.*.login.*`, `com.beeper.bridge_state` |
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
| Avatars, network label, last message, time | done | `m.room.avatar`, `m.bridge` |
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
| Edits applied | part | `m.replace`; no "edited" mark or edit history |
| Sender grouping, timestamps | done | local |
| Day separators, unread divider | todo | local, `m.fully_read` |
| Replies (quote, jump to message) | todo | `m.in_reply_to` |
| Reactions (show, add, remove) | todo | `m.reaction`, `com.beeper.reaction.shortcode` |
| Deleted messages | todo | `m.room.redaction` |
| Images, video, audio, files, voice notes | part | shown as a text label; no media |
| Stickers, GIFs, custom emoji | todo | `m.sticker`, `fi.mau.gif`, `m.image_pack` |
| Link previews | todo | `com.beeper.linkpreviews` |
| Formatted text, mentions, code blocks | todo | `org.matrix.custom.html`, `m.mentions` |
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
| Reply, edit, delete, forward | todo | relations, redaction |
| Attachments, paste image, drag and drop, camera | todo | media upload, encrypted files |
| Voice notes | todo | `org.matrix.msc3245.voice` |
| Emoji / GIF / sticker pickers | todo | local, `fi.mau.gif` |
| Formatting shortcuts, mentions autocomplete | todo | local |
| Send later, undo send | todo | `com.beeper.is_scheduled`, local delay |
| AI rewrite, translate, transcription | todo | Beeper AI endpoints |

## App

| Feature | Status | Protocol |
|---|---|---|
| Notifications, badge, sounds | todo | push (FCM on Android), local on desktop |
| Background sync on Android | todo | foreground service |
| Settings (appearance, chat list, notifications, shortcuts) | todo | `com.beeper.desktop.prefs` |
| Incognito mode (no read receipts, no typing) | part | read receipts are always private for now |
| Command bar, hotkeys | todo | local |
| Tray icon, launch at login, deep links | todo | local |
| Media viewer, PDF preview, storage manager | todo | local |
| Feedback, help hub, subscription banners | todo | Beeper services |
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
