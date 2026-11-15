//! How the delivery of the user's own messages is shown beside the time: in words, as ticks
//! (WhatsApp), as circles (Signal, Messenger), as the readers' pictures (Element), or not at all.
//! A failure always shows.

use super::*;

/// Readers' pictures shown at most; the rest are counted.
const FACES: usize = 3;

/// Chosen in Settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum ReceiptStyle {
    #[default]
    Text,
    Ticks,
    Circles,
    Avatars,
    Off,
}

impl ReceiptStyle {
    pub(crate) const ALL: [Self; 5] = [Self::Text, Self::Ticks, Self::Circles, Self::Avatars, Self::Off];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Text => t!("Text"),
            Self::Ticks => t!("Ticks"),
            Self::Circles => t!("Circles"),
            Self::Avatars => t!("Avatars"),
            Self::Off => t!("No status"),
        }
    }
}

/// When someone else's message shows whether the user has read it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum IncomingStatus {
    /// Beside the time under each run of their messages.
    Always,
    /// Under a message once it is tapped.
    #[default]
    OnTap,
}

impl IncomingStatus {
    pub(crate) const ALL: [Self; 2] = [Self::Always, Self::OnTap];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Always => t!("Always"),
            Self::OnTap => t!("On tap"),
        }
    }
}

/// What became of one of the user's messages, or of someone else's on the user's side.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Receipt {
    Sending,
    Sent,
    Delivered,
    /// Read; `who` names the readers in a group and is `None` in a direct chat.
    Seen { who: Option<String>, readers: Vec<(OwnedUserId, String)> },
    /// The bridge's reason, empty when it gave none.
    Failed(String),
    /// Someone else's message the user has not read yet.
    Unread,
    /// Someone else's message the user has read; `public` when its sender can see that. `others`
    /// names who else read it, in a group.
    Heard { public: bool, others: Option<String> },
}

impl Receipt {
    /// As the Text style shows it and a screen reader says it.
    pub(super) fn words(&self) -> String {
        match self {
            Self::Sending => t!("Sending…").to_owned(),
            Self::Sent => t!("Sent").to_owned(),
            Self::Delivered => t!("Delivered").to_owned(),
            Self::Seen { who: None, .. } => t!("Seen").to_owned(),
            Self::Seen { who: Some(who), .. } => t!("Seen by {who}", who = who),
            Self::Failed(why) if why.is_empty() => t!("Not sent").to_owned(),
            Self::Failed(why) => t!("Not sent: {why}", why = why),
            Self::Unread => t!("Not read yet").to_owned(),
            Self::Heard { public, others } => {
                let status = if *public { t!("Seen") } else { t!("Seen privately") };
                match others {
                    Some(who) => t!("{status}, also by {who}", status = status, who = who),
                    None => status.to_owned(),
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Glyph {
    Clock,
    Tick,
    Ticks,
    Ring,
    RingTick,
    /// A filled circle with a tick cut out of it.
    Disc,
    /// A filled circle with two ticks.
    DiscTicks,
    /// Read without telling the sender: the ticks or the disc beside a crossed eye.
    TicksPrivate,
    DiscPrivate,
}

/// How a receipt is drawn after the time.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Mark {
    Words(String),
    /// `read` draws it in the stronger ink.
    Glyph { glyph: Glyph, read: bool },
    /// Readers' pictures (user, name) and how many more read it.
    Faces(Vec<(OwnedUserId, String)>, usize),
    Failed { icon: bool, words: String },
}

impl Mark {
    fn drawn(&self) -> bool {
        matches!(self, Self::Glyph { .. } | Self::Faces(..))
    }
}

/// `None`: only the time.
pub(super) fn mark(receipt: &Receipt, style: ReceiptStyle) -> Option<Mark> {
    use Glyph::*;
    let glyph = |glyph, read| Some(Mark::Glyph { glyph, read });
    let faces = |readers: &[(OwnedUserId, String)], most: usize| Some(Mark::Faces(readers.iter().take(most).cloned().collect(), readers.len().saturating_sub(most)));
    if let Receipt::Failed(_) = receipt {
        return Some(Mark::Failed { icon: matches!(style, ReceiptStyle::Ticks | ReceiptStyle::Circles | ReceiptStyle::Avatars), words: receipt.words() });
    }
    match (style, receipt) {
        (ReceiptStyle::Text, _) => Some(Mark::Words(receipt.words())),
        (ReceiptStyle::Off, _) => None,
        (ReceiptStyle::Ticks, Receipt::Sending) => glyph(Clock, false),
        (ReceiptStyle::Ticks, Receipt::Sent) => glyph(Tick, false),
        (ReceiptStyle::Ticks, Receipt::Delivered) => glyph(Ticks, false),
        (ReceiptStyle::Ticks, Receipt::Seen { .. }) => glyph(Ticks, true),
        (ReceiptStyle::Ticks, Receipt::Unread) => glyph(Tick, false),
        (ReceiptStyle::Ticks, Receipt::Heard { public: true, .. }) => glyph(Ticks, true),
        (ReceiptStyle::Ticks, Receipt::Heard { .. }) => glyph(TicksPrivate, false),
        (_, Receipt::Unread) => glyph(RingTick, false),
        (_, Receipt::Heard { public: true, .. }) => glyph(Disc, true),
        (_, Receipt::Heard { .. }) => glyph(DiscPrivate, false),
        (_, Receipt::Sending) => glyph(Ring, false),
        (_, Receipt::Sent) => glyph(RingTick, false),
        (_, Receipt::Delivered) => glyph(Disc, false),
        // Messenger: the reader's face in a direct chat; a group's row would hold too many.
        (ReceiptStyle::Circles, Receipt::Seen { who: None, readers }) => faces(readers, 1),
        (ReceiptStyle::Circles, Receipt::Seen { .. }) => glyph(DiscTicks, true),
        (_, Receipt::Seen { readers, .. }) => faces(readers, FACES),
        (_, Receipt::Failed(_)) => unreachable!("handled above"),
    }
}

/// Whether the user has read message `ix` of `msgs`: `None` if not, else whether its sender can
/// see that. Reads only what has arrived; never sends a receipt.
pub(super) fn heard(msgs: &[Msg], ix: usize, own: &OwnRead) -> Option<bool> {
    let at = |event: &str, shown: &Option<String>| {
        let find = |id: &str| msgs.iter().position(|row| row.id == id);
        find(event).or_else(|| shown.as_deref().and_then(find))
    };
    // Sending a message reads what came before it, for all to see.
    let answered = msgs[ix + 1..].iter().any(|row| row.own && row.kind == Kind::Message);
    let read = answered || own.read.as_ref().and_then(|(event, shown)| at(event, shown)).is_some_and(|row| row >= ix);
    // A public receipt on no row this chat has loaded is placed by when it was sent.
    read.then(|| answered || own.public.as_ref().is_some_and(|(event, shown, ts)| at(event, shown).map_or(*ts >= msgs[ix].ts, |row| row >= ix)))
}

impl BeeperApp {
    /// The receipt after the time, faded in to `level` while it changes; an icon it replaces
    /// fades out over it meanwhile.
    pub(super) fn receipt_mark(&self, receipt: &Receipt, now: &Mark, level: Option<(Option<&Mark>, f32)>, cx: &App) -> AnyElement {
        let p = Palette::of(cx);
        let read_ink = self.on_wallpaper(p.text, cx);
        let faint = self.on_wallpaper(p.faint, cx);
        // On a photo both are the text colour; in flight then stays a step back.
        let soft = if faint == read_ink { faint.opacity(0.6) } else { faint };
        let draw = |mark: &Mark| -> AnyElement {
            match mark {
                Mark::Words(words) => div().child(words.clone()).into_any_element(),
                Mark::Failed { icon, words } => h_flex()
                    .gap(px(3.))
                    .when(*icon, |el| el.child(Icon::new(IconName::CircleAlert).size(fs(13.)).text_color(p.negative)))
                    .child(words.clone())
                    .into_any_element(),
                Mark::Glyph { glyph, read } => glyph_el(*glyph, if *read { read_ink } else { soft }, p.ground),
                Mark::Faces(readers, more) => h_flex()
                    .gap(px(2.))
                    .children(readers.iter().map(|(user, name)| avatar(name, self.faces.get(user), fs(14.) / px(1.), cx)))
                    .when(*more > 0, |el| el.child(div().pl(px(1.)).text_color(soft).child(format!("+{more}"))))
                    .into_any_element(),
            }
        };
        let fading = level.and_then(|(old, v)| Some((old?, v))).filter(|(old, _)| old.drawn() && now.drawn());
        div()
            .id("receipt")
            .relative()
            .role(Role::Image)
            .aria_label(receipt.words())
            .when(now.drawn(), |el| el.ml(px(4.)))
            .child(div().when_some(level, |el, (_, v)| el.opacity(v)).child(draw(now)))
            .when_some(fading, |el, (old, v)| el.child(div().absolute().top_0().left_0().opacity(1. - v).child(draw(old))))
            .into_any_element()
    }
}

/// Lucide's stroke at the size of 12 px text; the filled ones are its circle, filled, with the
/// ticks in the ground's colour.
fn glyph_el(glyph: Glyph, ink: Hsla, ground: Hsla) -> AnyElement {
    let private = |first: Glyph| h_flex().gap(px(2.)).child(glyph_el(first, ink, ground)).child(Icon::new(IconName::EyeOff).size(fs(12.)).text_color(ink)).into_any_element();
    let icon = |name: IconName| Icon::new(name).size(fs(13.)).text_color(ink).into_any_element();
    let disc = |name: IconName, size: f32| {
        div()
            .size(fs(13.))
            .flex()
            .items_center()
            .justify_center()
            .child(div().size(fs(12.)).flex().items_center().justify_center().rounded_full().bg(ink).child(Icon::new(name).size(fs(size)).text_color(ground)))
            .into_any_element()
    };
    match glyph {
        Glyph::Clock => icon(IconName::Clock),
        Glyph::Tick => icon(IconName::Check),
        Glyph::Ticks => icon(IconName::CheckCheck),
        Glyph::Ring => icon(IconName::Circle),
        Glyph::RingTick => icon(IconName::CircleCheck),
        Glyph::Disc => disc(IconName::Check, 9.),
        Glyph::DiscTicks => disc(IconName::CheckCheck, 10.),
        Glyph::TicksPrivate => private(Glyph::Ticks),
        Glyph::DiscPrivate => private(Glyph::Disc),
    }
}

#[cfg(test)]
mod tests {
    use super::{Glyph, Kind, Mark, Msg, OwnRead, OwnedUserId, Receipt, ReceiptStyle, heard, mark};

    fn seen(dm: bool) -> Receipt {
        let readers = ["@a:x", "@b:x", "@c:x", "@d:x"].iter().take(if dm { 1 } else { 4 }).map(|u| (OwnedUserId::try_from(*u).unwrap(), u[1..2].to_owned())).collect();
        Receipt::Seen { who: (!dm).then(|| "a, b, c and d".to_owned()), readers }
    }

    #[test]
    fn text_is_the_words_of_today() {
        assert_eq!(mark(&Receipt::Sent, ReceiptStyle::Text), Some(Mark::Words("Sent".into())));
        assert_eq!(mark(&seen(true), ReceiptStyle::Text), Some(Mark::Words("Seen".into())));
        assert_eq!(mark(&seen(false), ReceiptStyle::Text), Some(Mark::Words("Seen by a, b, c and d".into())));
    }

    #[test]
    fn ticks_count_up_and_darken_when_read() {
        let g = |r: &Receipt| mark(r, ReceiptStyle::Ticks);
        assert_eq!(g(&Receipt::Sending), Some(Mark::Glyph { glyph: Glyph::Clock, read: false }));
        assert_eq!(g(&Receipt::Sent), Some(Mark::Glyph { glyph: Glyph::Tick, read: false }));
        assert_eq!(g(&Receipt::Delivered), Some(Mark::Glyph { glyph: Glyph::Ticks, read: false }));
        assert_eq!(g(&seen(false)), Some(Mark::Glyph { glyph: Glyph::Ticks, read: true }));
    }

    #[test]
    fn circles_fill_and_show_the_reader_in_a_direct_chat() {
        let g = |r: &Receipt| mark(r, ReceiptStyle::Circles);
        assert_eq!(g(&Receipt::Sending), Some(Mark::Glyph { glyph: Glyph::Ring, read: false }));
        assert_eq!(g(&Receipt::Sent), Some(Mark::Glyph { glyph: Glyph::RingTick, read: false }));
        assert_eq!(g(&Receipt::Delivered), Some(Mark::Glyph { glyph: Glyph::Disc, read: false }));
        assert!(matches!(g(&seen(true)), Some(Mark::Faces(f, 0)) if f.len() == 1));
        assert_eq!(g(&seen(false)), Some(Mark::Glyph { glyph: Glyph::DiscTicks, read: true }));
    }

    #[test]
    fn avatars_show_three_readers_and_count_the_rest() {
        assert!(matches!(mark(&seen(false), ReceiptStyle::Avatars), Some(Mark::Faces(f, 1)) if f.len() == 3 && f[0].1 == "a"));
        assert_eq!(mark(&Receipt::Sent, ReceiptStyle::Avatars), Some(Mark::Glyph { glyph: Glyph::RingTick, read: false }));
    }

    #[test]
    fn received_messages_say_whether_the_sender_sees_the_read() {
        let heard = |public| Receipt::Heard { public, others: None };
        assert_eq!(mark(&Receipt::Unread, ReceiptStyle::Ticks), Some(Mark::Glyph { glyph: Glyph::Tick, read: false }));
        assert_eq!(mark(&heard(true), ReceiptStyle::Ticks), Some(Mark::Glyph { glyph: Glyph::Ticks, read: true }));
        assert_eq!(mark(&heard(false), ReceiptStyle::Ticks), Some(Mark::Glyph { glyph: Glyph::TicksPrivate, read: false }));
        assert_eq!(mark(&heard(true), ReceiptStyle::Circles), Some(Mark::Glyph { glyph: Glyph::Disc, read: true }));
        assert_eq!(mark(&heard(false), ReceiptStyle::Avatars), Some(Mark::Glyph { glyph: Glyph::DiscPrivate, read: false }));
        assert_eq!(mark(&heard(false), ReceiptStyle::Off), None);
        assert_eq!(mark(&Receipt::Heard { public: false, others: Some("Ann".into()) }, ReceiptStyle::Text), Some(Mark::Words("Seen privately, also by Ann".into())));
        assert_eq!(mark(&Receipt::Unread, ReceiptStyle::Text), Some(Mark::Words("Not read yet".into())));
    }

    #[test]
    fn heard_places_the_users_receipts() {
        let them = OwnedUserId::try_from("@b:x").unwrap();
        let row = |id: &str, ts: u64, own: bool| Msg { ts, own, ..Msg::bare(id.into(), Kind::Message, them.clone()) };
        let mut msgs = vec![row("$1", 10, false), row("$2", 20, false), row("$3", 30, false)];
        let pair = |id: &str| Some((id.to_owned(), None));
        // Read privately up to $2: $3 is unread, $1 and $2 read without the sender knowing.
        let own = OwnRead { read: pair("$2"), public: None };
        assert_eq!((heard(&msgs, 0, &own), heard(&msgs, 1, &own), heard(&msgs, 2, &own)), (Some(false), Some(false), None));
        // The public receipt on a hidden event, sent after $1 arrived: $1 public, $2 private.
        let own = OwnRead { read: pair("$2"), public: Some(("$hidden".into(), None, 15)) };
        assert_eq!((heard(&msgs, 0, &own), heard(&msgs, 1, &own)), (Some(true), Some(false)));
        // A hidden event falls back to the row the timeline shows it on.
        let own = OwnRead { read: Some(("$hidden".into(), Some("$3".into()))), public: None };
        assert_eq!(heard(&msgs, 2, &own), Some(false));
        // Answering reads everything before it, publicly.
        msgs.push(row("$4", 40, true));
        assert_eq!(heard(&msgs, 2, &OwnRead::default()), Some(true));
    }

    #[test]
    fn off_shows_only_a_failure() {
        for r in [Receipt::Sending, Receipt::Sent, Receipt::Delivered, seen(true)] {
            assert_eq!(mark(&r, ReceiptStyle::Off), None);
        }
        assert_eq!(mark(&Receipt::Failed("no network".into()), ReceiptStyle::Off), Some(Mark::Failed { icon: false, words: "Not sent: no network".into() }));
        assert_eq!(mark(&Receipt::Failed(String::new()), ReceiptStyle::Ticks), Some(Mark::Failed { icon: true, words: "Not sent".into() }));
    }
}
