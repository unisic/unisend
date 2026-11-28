# Feature parity with Signal, WhatsApp, Instagram and Messenger

What a daily user of the big chat apps would miss in Unisend, and what Matrix and the Beeper
bridges allow at all. Unisend status is from `docs/parity.md` and the code; the other apps are
from general knowledge of 2025 releases, `?` marks what is uncertain. The Beeper comparison is
`docs/parity.md`.

Legend: Y yes, N no, P partial. Last column: Y possible, B only if the bridge supports it,
N impossible over Matrix or bridges.

| feature | Signal | WhatsApp | Instagram | Messenger | Unisend | Matrix/bridges? |
|---|---|---|---|---|---|---|
| Text, replies, forward | Y | Y | Y | Y | `backend/forward.rs`, `ui/forward.rs` | Y |
| Edit, delete for everyone | Y | Y | Y | Y | `ui/menu.rs`; limits from `backend/features.rs`; edit history from "Edited" (`backend/edits.rs`) | B |
| Delete for me | Y | Y | Y | Y | local hide list | Y (local) |
| Reactions | Y | Y | Y | Y | `ui/heart.rs`, `ui/emojipick.rs` | B |
| Mentions | Y | Y | Y | Y | `backend/mentions.rs` | B |
| Formatting (bold, code, spoiler) | Y | P | N | N | `backend/rich.rs`, `ui/rich.rs`; `\|\|spoiler\|\|` sent, covered until tapped | Y (HTML) |
| Threads | N | N | N | P | part (`m.thread`, see parity.md) | Y natively, bridges flatten |
| Photos, video, files | Y | Y | Y | Y | `ui/gallery.rs`, `ui/viewer.rs`, `ui/video.rs` | Y |
| Edit photo, trim video before sending | Y | Y | Y | P | `ui/caption/edit.rs`, `ui/caption/trim.rs`; face blur Android only, trim Android and Linux | Y |
| Media gallery per chat | Y | Y | P | Y | `ui/info.rs` (Media, Links, Files) | Y |
| Voice notes | Y | Y | Y | Y | `ui/voice.rs`, `backend/voice.rs`; not recorded on desktop | Y |
| Stickers (send, packs) | Y | Y | Y | Y | `ui/stickers.rs`, `backend/stickers.rs`: Matrix image packs of the account and its rooms; none bought or downloaded | B (`m.sticker`) |
| GIF search | Y | Y | Y | Y | `ui/gifs.rs` (KLIPY, only with a build key) | Y |
| Polls | Y | Y | N | P | `ui/pollnew.rs`, `backend/polls.rs` | B |
| Location, send | P (no live) | Y + live | N | Y + live | `ui/location.rs`, `backend/location.rs` | B; live N |
| Contact cards | Y | Y | N | N | `src/vcard.rs`, `ui/contacts.rs` | B |
| Link previews (generated) | Y (sender side) | Y | Y | Y | `backend/linkprev.rs`, `ui/linkdraft.rs`, made by the sender | B |
| Disappearing messages | Y | Y | Y (vanish) | Y | `backend/disappear.rs`, `ui/disappear.rs` | B |
| View-once media | Y | Y | Y | Y | missing; forward refuses it | N (no standard event) |
| Calls (voice, video) | Y | Y | Y | Y | missing | N over bridges; Matrix-native calls only |
| Stories, status | ? | Y | Y | Y | missing | N |
| Contacts, find by username | Y | Y | Y | Y | `ui/contacts.rs`, `ui/people.rs` | B |
| Search in chat and in all chats | Y | Y | Y | Y | `backend/search.rs`, `ui/search.rs` (local FTS5) | Y (local) |
| Media auto-download by network | Y | Y | N? | N? | `ui/download.rs`: mobile data, Wi-Fi, roaming | Y (local) |
| Manage storage, keep messages | Y | Y | N? | N? | `ui/storage.rs`: sizes, clear media cache, keep 30 days / 1 year / forever | Y (local) |
| Search filters (date, type) | P | Y | N | P | `ui/search.rs`: media, links, files, sender, time, in a chat's search | Y (local index) |
| Archive | Y | Y | N | Y | done | Y |
| Keep muted chats archived | Y | ? | N | ? | `ui/mute.rs` (local setting) | Y (local) |
| Mark all read | Y | Y | N | ? | `ui/readall.rs` | Y |
| Pin chats | Y | Y | Y | Y | tag `m.favourite`; no manual order | Y |
| Pin messages in a chat | Y | Y | N | Y | `ui/pins.rs` (`m.room.pinned_events`) | B |
| Mute with durations | Y | Y | Y | Y | `ui/mute.rs` | Y |
| Folders / lists | N | Y | P | N | `ui/folders.rs` (local) | Y (local) |
| Starred / saved messages | N | Y | N | N | missing | Y (local) |
| Note to self | Y | Y | N | ? | done, Beeper's `com.beeper.rooms.note_to_self` | Y |
| Drafts | Y | Y | Y | Y | local only | Y |
| Notifications, quick reply | Y | Y | Y | Y | `src/notify/*` | Y |
| Per-chat notification settings (sound, mentions) | Y | Y | P | P | `src/quiet.rs`, `ui/quiet.rs` (local) | Y (local; push rules possible) |
| Notification profiles (schedule, exceptions) | Y | N | N | N | `src/quiet.rs`, `ui/quiet.rs` (local) | Y (local) |
| Notifications with the app in background | Y (own server) | Y (FCM) | Y | Y | Android: own background connection (`stay_connected`); desktop: runs on with the window closed (`src/desktop.rs`) | Y; UnifiedPush possible, FCM excluded |
| Message requests | Y | Y | Y | Y | `backend/rooms.rs` | B |
| Block a person | Y | Y | Y | Y | `backend/block.rs`, `ui/block.rs`: Matrix ignore list; on the network where the bridge has `block_user` | Y; B on the network |
| App lock, screen security | Y | Y | N | Y | `src/applock*.rs`, `ui/pinpad.rs`, `FLAG_SECURE` | Y (local) |
| Verify contacts / devices | Y | Y | N | Y | `backend/verify.rs`, `ui/verify.rs` | Y (Matrix SAS) |
| No read receipts / typing | Y | Y | N | P | default on | Y |
| Chat history backup | Y (local) | Y (cloud) | N | ? | key backup through the recovery code | Y |
| Linked devices list, sign one out | Y | Y | Y | Y | Settings, Devices (`backend/verify.rs` `devices`, `delete_devices`) | Y |
| Accessibility (screen reader, scaling) | Y | Y | Y | Y | text size steps; desktop: Tab, arrow keys in the chat list and messages, roles and spoken labels (AccessKit, untested with a real reader) | Y (client) |
| Chat colors, wallpaper | Y | Y | Y | Y | `ui/appearance.rs` | Y (local) |
| Per-chat wallpaper and color | Y | Y | Y | Y | `ui/chatlook.rs` | Y (local) |
| Language switch in app | Y | Y | Y | Y | Settings, Language (`ui/panels.rs`) | Y |
| Desktop app | Y | Y | N (web) | Y | Linux; tray `src/desktop/tray.rs`; Windows and macOS compile only | Y |
| Hotkeys, command bar | P | Y | P | P | missing | Y |
| Group admin (roles, rename, members) | Y | Y | P | Y | `backend/group.rs`, `ui/group.rs` | B |
| Invite links | Y | Y | Y | Y | `ui/invite.rs`: group link with approval (`knock`) in Matrix groups; no bridge carries it | Y on Matrix; N on bridges today |
| Nicknames and notes on contacts | Y | N | P | P | `ui/nick.rs`, on this device | Y (local) |
| QR code to add me | Y | Y | Y | ? | Settings, Profile (`ui/profile.rs`); no scanner | Y |
| Updates outside a store | N | N | N | N | `backend/update.rs` (Android) | Y |

## What bridges make impossible

- Calls: bridges carry messages, not media streams. Matrix-native calls (Element Call, needs a
  LiveKit server) reach only other Matrix users.
- View-once media: no stable Matrix event; bridges deliver a normal file.
- Stories and status as live objects.
- Live location: no stable event.
- Messenger chat theme and nicknames: not bridged.
- Chat backup restore from WhatsApp, Signal or Meta clouds.
- Threads on bridged networks: only Matrix-native rooms have real threads.

## The missing features most worth building

Effort: S small, M medium, L large.

2. Sticker picker and sending: done (`ui/stickers.rs`); adding or editing packs remains.
   Medium, M.
3. Inline video playback in the timeline. High, M. Bridged GIFs loop in their bubble (at most
   5 s, 10 frames a second, 320 px, made into a GIF on the device); other videos show their
   poster and length and play full screen. Playing any video in the bubble is left out: GPUI
   draws pictures, not a video surface, so each frame would be decoded, converted and uploaded
   as a new image, with sound kept in step by hand, per bubble on screen.
4. Voice notes on desktop (record and play in the bubble). Medium-high, M.
5. Chat folders and filters: DMs, Groups, user lists. Medium-high, M. Local. Done (`ui/folders.rs`).
6. Pinned messages in a chat (`m.room.pinned_events`). Medium, S-M. Done (`ui/pins.rs`).
7. Starred or saved messages (local list). Medium, S.
8. Search filters (media, links, date, sender) on the local index. Medium, M. Done in a chat's
   search; the chat list's search has none.
9. Generated link previews: owner's privacy choice (the linked site sees the user's IP, or the
   homeserver sees the link through its `preview_url`). Medium, M.
10. Push without Google through UnifiedPush, for phones that kill the background connection.
    Medium, L. The push server sees an opaque wake-up only.
11. Threads for Matrix-native rooms: a thread view exists (parity.md); a thread list and per-thread unread remain. Medium, M.
12. Per-chat wallpaper and color: done (`ui/chatlook.rs`).
13. Desktop hotkeys and a command bar. Medium, M.
14. Screen reader support: labels and keyboard reach are in (desktop); tested with Orca and TalkBack, and settings rows and sheets reachable by Tab, remain. High for some users, M.
15. Matrix-native calls. Only Matrix peers, needs a LiveKit server. L, last.

Not worth chasing: view-once, stories, calls through bridges, bridge-side backups.
