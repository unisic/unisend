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

Four destinations with icon and label: Chats, Archive, Networks, Settings. On a phone they are a
bar at the bottom; on a wide window, a column on the left. The active one is white, the rest
`faint`; no pill, no accent (a pointer hovering gets a `surface` row). Settings and Networks are
screens, not menus; their content keeps to a 560 column.

Chats has three filters as round chips under the title: All, Unread, Low priority.

## Chat list

Rows 72 tall, avatar 48. Line one: the name, then the network and the time on the right in
`faint`. Line two: the preview in `muted`, then what needs you: an accent count, an accent `@`
when you were mentioned. Unread rows have a white bold name and a white preview.

## Conversation

Incoming messages sit left on `surface`, own messages right on `raised`. In a group, each run of
messages from one person starts with their name and ends with their avatar beside the last
message; the time is printed once under the run. A mention is accent text; a message that mentions
you carries an accent bar on its leading edge. Replies quote in a lighter inset. Menus are sheets
that rise from the bottom with rows 52 tall.
