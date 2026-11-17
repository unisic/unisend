# Unisend

A Matrix chat client that also shows the chats of other networks through Beeper's bridges. These
are the words for its concepts in issues, docs, UI text and code names.

## Language

### Chats

**Chat**:
One conversation in the list, with one person or with a group. It is the word in every text a user
reads and in UI code.
_Avoid_: conversation, thread, channel, room (outside the Matrix side)

**Room**:
The Matrix object that carries a chat. It is the word in `src/backend/` and wherever a Matrix
event, ID or state is meant.
_Avoid_: chat (for the Matrix object)

**One-to-one chat**:
A chat with one other person (`dm` in code).
_Avoid_: DM, direct chat, private chat

**Group**:
A chat with more than one other person.
_Avoid_: channel, room

**Note to self**:
The chat only the user is in (`com.beeper.rooms.note_to_self`).
_Avoid_: saved messages, notes

**Invitation**:
A chat on a Matrix homeserver that waits for the user to join or decline. The inviter sees the
answer.
_Avoid_: invite (as a noun), request

**Message request**:
A bridged chat from a stranger that waits for the user to accept or delete it. The other side
learns nothing of the user until it is accepted.
_Avoid_: invitation, pending chat

### The chat list

**Inbox**:
The section of the list with chats that are neither archived nor low priority.
_Avoid_: main list, all chats, home

**Archive**:
The section with chats the user has put away (`com.beeper.inbox.done`). An archived chat is not in
the inbox or in low priority.
_Avoid_: done, hidden, closed

**Low priority**:
The section with chats carrying the `m.lowpriority` tag that are not archived.
_Avoid_: muted, other, spam

**Pinned**:
A chat held at the top of the list (`m.favourite` tag). A pinned chat cannot be archived.
_Avoid_: favourite, starred

**Muted**:
A chat that notifies for mentions only, or for nothing. It stays in its section.
_Avoid_: silenced, low priority

**Marked unread**:
A chat the user flagged as unread by hand (`m.marked_unread`), apart from its count of unread
messages.
_Avoid_: unread (when the flag is meant)

### Messages

**Delivery**:
How far the user's own message got: Sending, Sent, Delivered or Failed.
_Avoid_: status, send state

**Read**:
The other person opened the message. "Seen" is the same state in Instagram's and Messenger's
wording, shown only for those networks.
_Avoid_: seen (as the general term), viewed

**Hide reading and typing**:
The setting that stops read receipts and typing notices from leaving the device.
_Avoid_: incognito, ghost mode, invisible

**Incognito keyboard**:
The Android setting that tells the keyboard to learn nothing from what is typed.
_Avoid_: incognito (alone), private mode

**Disappearing messages**:
A chat's timer after which its messages are deleted for everyone in it.
_Avoid_: ephemeral messages, self-destruct, expiring messages

### Account and devices

**Homeserver**:
The Matrix server an account lives on.
_Avoid_: server (when another server could be meant), instance

**Beeper account**:
An account on Beeper's homeserver. It is the only kind that has networks.
_Avoid_: Beeper user, bridged account

**Device**:
One signed-in copy of a Matrix client on the account. "This device" is the copy of Unisend in hand.
_Avoid_: session, login, client

**Verification**:
Two devices of one account comparing emoji, after which each trusts the other with message keys.
_Avoid_: pairing, confirmation, cross-signing (the mechanism under it)

**Recovery code**:
The text that unlocks the account's encrypted message history on a new device.
_Avoid_: recovery key, security key, passphrase, backup key

**App lock**:
The setting that asks for proof before Unisend opens: Off, Phone lock or App PIN.
_Avoid_: screen lock, passcode

**App PIN**:
The app lock that asks for a PIN known only to Unisend.
_Avoid_: passcode, password, PIN (alone: Messenger's history PIN is another thing)

**Phone lock**:
The app lock that asks Android for the phone's own unlock. Android only.
_Avoid_: biometrics, fingerprint, device lock

### Networks

**Network**:
A chat service other than Matrix whose chats reach Unisend: WhatsApp, Signal, Messenger.
_Avoid_: platform, service, protocol, account

**Bridge**:
The Beeper program that carries one network's chats into Matrix rooms and back.
_Avoid_: connector, integration, bot

**Bridged chat**:
A chat that lives on a network and reaches Unisend through a bridge. What the user may do in it is
what the network accepts.
_Avoid_: external chat, network chat

**Matrix chat**:
A chat that lives only on Matrix, with no network behind it.
_Avoid_: native chat, normal chat

### The app

**Platform**:
An operating system Unisend is built for: Android, Linux, macOS, Windows.
_Avoid_: network, target, OS

**Phone layout**:
The one-column layout a window narrower than `NARROW` gets, on any platform.
_Avoid_: mobile, Android layout
