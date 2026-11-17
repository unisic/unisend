# Design: Night

The look of the app, decided once so every screen agrees. Tokens live in `Palette` (`src/lib.rs`)
and the metrics at the top of `src/ui.rs`; this file says what they mean.

## The idea

True black, flat. Depth is a lightness step and nothing else: no borders around cards, no shadows,
no gradients. Colour is spent on what is addressed to you (unread, a mention) and nowhere on chrome.
The one primary control on a screen is made primary by inversion: white with black text.

## Colour (dark; the light set mirrors it)

| Token | Dark | Used for |
|---|---|---|
| `ground` | `#000000` | every screen, bars and navigation included |
| `surface` | `#111111` | cards, incoming messages, fields |
| `raised` | `#2a2a2a` | own messages, pressed and selected rows, secondary buttons |
| `rule` | `#1a1a1a` | the one hairline under a bar when content scrolls beneath it |
| `text` | `#ffffff` | names, message text, the active destination |
| `muted` | `#b4b4b4` | previews, secondary text, inactive destinations |
| `faint` | `#949494` | times, labels, placeholders |
| `accent` | `#c6f24e` | unread counts, mentions, the new-message mark. Never buttons, tabs or navigation |
| `negative` | `#e5484d` | deleting, signing out, errors |
| `warning` | `#e5a93b` on `#2b2414` | banners that ask for something |

Text on `accent` is black. The accent stays under about 3% of a screen.

## Type

Inter, compiled in, on every platform; weights 400, 500 and 600, nothing heavier. Sizes: screen
title 22, names and message text 16, secondary 14, labels and times 12. Sentence case everywhere,
no tracked capitals.

## Shape and space

Cards and messages: radius 20. Controls: fully round. Avatars: circles. One gap, 14, between
things; 16 from the screen edge. Anything tapped is at least 44 tall; a control may draw smaller
inside that (a filter chip is a 36 pill in a 44 target). Reaction chips are 32: they repeat what
the message's sheet offers at full size.

## Navigation

Four destinations with icon and label: Chats, Archive, Networks, Settings; an account on another
Matrix server has no Networks. On a phone they are a
bar at the bottom; on a wide window, a column on the left. The active one is white, the rest
`faint`; no pill, no accent (a pointer hovering gets a `surface` row). Settings and Networks are
screens, not menus; their content keeps to a 560 column.

Chats has its folders as round chips under the title, in one row that scrolls sideways when it does not
fit: All, Unread (with its count), Direct, Groups, one per chat network (only with two or more networks),
the user's own folders, and Low priority last. Settings, Chats, Chat folders hides any built-in chip and
makes, renames, orders and deletes the user's folders; a chat joins one from its sheet, "Add to folder".
The folders and the chip last chosen are kept on this device only, in `settings.json`.

## Chat list

Rows 72 tall, avatar 48. Line one: the name, then the network and the time on the right in
`faint`. Line two: the preview in `muted`, then what needs you: an accent count, an accent `@`
when you were mentioned. Unread rows have a white bold name and a white preview.

A Telegram forum is one row: its picture and name, the newest topic before the preview, the
topics' unread added up. Touching it puts its topics in the list's place under a back arrow and
the forum's name, as rows of their own with the topic's initials; a topic opens as a chat.

## Conversation

Incoming messages sit left on `surface`, own messages right on `raised`. In a group, each run of
messages from one person starts with their name and ends with their picture beside the last
message; the time is printed once under the run. A mention of you is accent text, one of someone else
semibold; a message that mentions you carries an accent bar on its leading edge. Replies quote in a lighter inset; touching the
quote goes to the message it quotes.

Text size (Settings, Appearance) has five steps, 85 to 130%, and scales every font size and line
height through `fs()`; paddings, rows and icons keep their size. The chat wallpaper is the one place a
gradient is allowed: behind the messages only, never behind the bars or the composer. Presets are
tones of `Palette` no lighter than `raised` at 85%; a photo is darkened to 60% when imported and
dimmed at least 30% more, so its brightest point is `#6b6b6b`. Text that sits straight on a photo is
`text`, not `faint`. The light theme has no wallpaper.

Own messages can take a chat colour from Settings: Default (`raised`) or one of Beeper's nine
(blue, indigo, purple, pink, red, orange, green, teal, slate), the same in dark and light, kept on this
device. Everything inside such a bubble (text, quote inset, link, mention, poll, place) is drawn in
the bubble's ink, white or near-black (orange), chosen for 4.5:1. Blue is Beeper's #3478F6 darkened
to #2E6EF0 to reach that. The lime accent keeps its meaning.

Reactions are badges pinned over the lower edge of their message, on `lifted` (`#3d3d3d`, one
step above `raised`) with a ring in the ground's colour; your own reaction has a white ring.
Standing free under a message they read as small messages of their own.

A link is underlined. When its sender attached a preview, the message carries a card: the
picture in its own shape, the site, the title, two lines about it. Touching the card opens the
link.

A poll is its question, then one row per answer, 44 tall, with the share of votes drawn as a `lifted`
bar behind the text, the count at the end, and a check before the answer you chose (no accent: a poll
is not addressed to you). Under the rows, the number of votes, and "Poll ended" when it has. Touching
an answer votes for it; touching your only choice takes it back.

A location is a card in the bubble: a map pin, the description when it says more than the
coordinates, and the coordinates to five decimals. Touching it opens the place in maps.

Lines about the chat itself (someone joined or left, a new name) are centred `faint` text with no
bubble; joins that follow one another are one line. "New", in the accent, stands above what
arrived since the chat was last read.

Under the newest message, when it is the user's, one `faint` line after the time says where it
is: Sending, Sent, Delivered, Seen. A message that failed says so in `negative` wherever it is,
and its panel offers to try again. A message that has not reached the server is drawn at 60%.
Who is typing is one `faint` line above the composer.

A lost connection is a state, not an error: one quiet line at the top while it is retried, gone
when it is back.

## Acting on things

The common acts take one gesture, and each also has a slower way that can be found by looking.

- A tap on a message shows or hides the line under it: its time and, for your own, its status
  (who has read it in a group); a second tap or a tap on another message takes it back.
- A long press on a message lifts it over a dimmed screen with six reactions above it and its
  acts under it: Reply, Edit (your own), Open link, Copy, Save, Forward, Select multiple, Reactions
  (who reacted, when it has any), Info, each where it applies, and one Delete that asks whether for you or for everyone. The right mouse button, or the dots beside a message
  on a wide window, opens the same acts at the pointer; the heart and arrow there react and
  reply. A tap anywhere else closes it.
- A double tap likes the message (the heart), and a second double tap takes it back.
- A sideways swipe on a message replies to it.
- A long press or the right mouse button on a chat in the list, or the dots in a chat's header,
  opens that chat's sheet: rows 52 tall rising from the bottom, named after the chat.
- The paperclip opens the attach sheet on a phone: the newest photos and videos in a grid, and
  under it, where the thumb is, a row of labelled round buttons (52, on `raised`) for File, GIF,
  Location, Contact and Poll, each shown only where it works. Choosing photos puts Send in that
  row's place. Without the sheet (desktop) the paperclip opens a sheet of the same entries as rows.
- On desktop attached files wait in a tray over the composer (88 thumbnails on `surface`, an X in
  each corner), not a full-screen preview; Enter sends them with the text.
