//! Contact cards: choosing one from the phone's address book to send, and what a received one
//! looks like in a chat.

use super::*;
use crate::vcard::{self, Card};

/// Cards kept, newest answers last; older ones are asked for again when scrolled back to.
const MAX_KEPT: usize = 100;
/// Cards of one file drawn; a file may hold fifty.
const MAX_SHOWN: usize = 3;

#[derive(Default)]
pub(super) struct State {
    cards: HashMap<String, Vec<Card>>,
    order: VecDeque<String>,
    asked: HashSet<String>,
}

impl State {
    pub(super) fn keep(&mut self, id: String, cards: Vec<Card>) {
        // An answer for a chat closed meanwhile.
        if !self.asked.contains(&id) {
            return;
        }
        self.order.push_back(id.clone());
        self.cards.insert(id, cards);
        while self.order.len() > MAX_KEPT {
            let Some(old) = self.order.pop_front() else { break };
            self.cards.remove(&old);
            self.asked.remove(&old);
        }
    }
}

impl BeeperApp {
    /// Asks once for the cards of a contact file that is being drawn. Leaves half the command
    /// channel to user actions.
    pub(super) fn want_card(&mut self, ix: usize) {
        let m = &self.msgs[ix];
        let Some(media) = m.media.as_ref().filter(|media| !m.deleted && is_card(media)) else { return };
        if self.contacts.asked.contains(&m.id) || self.contacts.asked.len() >= 4 * MAX_KEPT || self.cmd.capacity() < self.cmd.max_capacity() / 2 {
            return;
        }
        if self.cmd.try_send(Cmd::Contact(m.id.clone(), media.clone())).is_ok() {
            self.contacts.asked.insert(m.id.clone());
        }
    }

    /// The card drawn in place of a file's row, once its contents are read; `room` is the width
    /// a bubble has for its content.
    pub(super) fn contact_card(&self, m: &Msg, media: &Media, room: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !is_card(media) {
            return None;
        }
        let cards = self.contacts.cards.get(&m.id).filter(|c| !c.is_empty())?;
        let raised = cx.theme().foreground.opacity(0.1);
        let more = cards.len().saturating_sub(MAX_SHOWN);
        let blocks = cards.iter().take(MAX_SHOWN).enumerate().map(|(n, card)| {
            let action = add_label(card);
            let owned = card.clone();
            v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .size(px(40.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .bg(raised)
                                .child(Icon::new(IconName::User).size_5()),
                        )
                        .child(
                            v_flex()
                                .min_w_0()
                                .child(div().line_clamp(2).text_ellipsis().text_size(px(15.)).line_height(px(20.)).child(card.name.clone()))
                                .children(card.numbers.iter().map(|(kind, number)| {
                                    let text = if kind.is_empty() { number.clone() } else { format!("{kind} {number}") };
                                    div().truncate().text_size(px(13.)).font_features(tabular()).opacity(0.7).child(text)
                                })),
                        ),
                )
                .children(action.map(|label| {
                    div()
                        .id(SharedString::from(format!("{}-add-{n}", m.id)))
                        .h(px(36.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(raised)
                        .text_size(px(14.))
                        .cursor_pointer()
                        .active(|el| el.opacity(0.7))
                        .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            add_contact(cx, &owned);
                        }))
                        .child(label)
                }))
        });
        Some(
            v_flex()
                .gap_3()
                .w(px(room.min(260.)))
                .children(blocks)
                .when(more > 0, |el| el.child(div().text_size(px(12.)).opacity(0.7).child(format!("and {more} more"))))
                .into_any_element(),
        )
    }
}

fn is_card(media: &Media) -> bool {
    media.kind == MediaKind::File && vcard::is_vcard(media.mime.as_deref(), &media.name)
}

/// The label of the button that takes a card into the phone's contacts; none where it can do nothing.
fn add_label(card: &Card) -> Option<&'static str> {
    #[cfg(target_os = "android")]
    {
        let _ = card;
        Some("Add contact")
    }
    #[cfg(not(target_os = "android"))]
    card.numbers.first().map(|_| "Copy number")
}

/// Android: the system's screen for a new contact, filled in. Elsewhere: copies the first number.
fn add_contact(cx: &mut App, card: &Card) {
    #[cfg(target_os = "android")]
    {
        let _ = cx;
        let number = card.numbers.first().map_or("", |n| n.1.as_str());
        let email = card.emails.first().map_or("", String::as_str);
        if let Err(e) = android::insert(&card.name, number, email) {
            log::warn!("add contact: {e}");
        }
    }
    #[cfg(not(target_os = "android"))]
    if let Some((_, number)) = card.numbers.first() {
        crate::copy(cx, number.clone());
    }
}

/// The attach sheet's button for sending a contact; only the phone has an address book to choose from.
pub(super) fn sheet_entry(live: bool, cx: &mut Context<BeeperApp>) -> Option<AnyElement> {
    #[cfg(target_os = "android")]
    {
        let p = Palette::of(cx);
        Some(
            h_flex()
                .id("gallery-contact")
                .h(px(TAP))
                .px(px(12.))
                .gap(px(8.))
                .flex_none()
                .rounded_full()
                .text_size(px(14.))
                .text_color(p.text)
                .cursor_pointer()
                .active(|el| el.bg(p.raised))
                .child(Icon::new(IconName::User).size_4())
                .child("Contact")
                .when(live, |el| {
                    el.on_click(cx.listener(|this, _, _, cx| {
                        this.close_gallery(cx);
                        this.send_contact(cx);
                    }))
                })
                .into_any_element(),
        )
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (live, cx);
        None
    }
}

#[cfg(target_os = "android")]
impl BeeperApp {
    /// Sends a contact chosen in the system's picker as a `.vcf` file.
    fn send_contact(&mut self, cx: &mut Context<Self>) {
        // The picker blocks its caller until the user is done, so it runs off the UI thread.
        let picked = cx.background_spawn(async { android::pick().map_err(|e| log::warn!("contact picker: {e}")).ok().flatten().map(PathBuf::from) });
        cx.spawn(async move |this, cx| {
            let Some(path) = picked.await else { return };
            let _ = this.update(cx, |this, cx| {
                let Some(room) = this.open.clone() else { return };
                let reply_to = match std::mem::replace(&mut this.compose, Compose::New) {
                    Compose::Reply(id) => Some(id),
                    _ => None,
                };
                this.uploads += 1;
                this.queue_attach(room, vec![path], reply_to, cx);
            });
        })
        .detach();
    }
}

#[cfg(target_os = "android")]
mod android {
    use gpui_mobile::android::jni as helpers;
    use jni::objects::JValue;

    const CLASS: &str = "dev.gpui.beeper.ContactPick";

    /// Path of the chosen contact's `.vcf` in the cache, or `None` when cancelled or unreadable.
    pub fn pick() -> Result<Option<String>, String> {
        helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            let path = env
                .call_static_method(&class, jni::jni_str!("pick"), jni::jni_sig!("(Landroid/app/Activity;)Ljava/lang/String;"), &[JValue::Object(&activity)])
                .and_then(|v| v.l())
                .map_err(|e| {
                    env.exception_clear();
                    e.to_string()
                })?;
            Ok((!path.is_null()).then(|| helpers::get_string(env, &path)))
        })
    }

    /// Opens the system's screen for a new contact with these details.
    pub fn insert(name: &str, number: &str, email: &str) -> Result<(), String> {
        helpers::with_env(|env| {
            let activity = helpers::activity(env)?;
            let class = helpers::find_app_class(env, CLASS)?;
            let mut text = |s: &str| env.new_string(s).map_err(|e| e.to_string());
            let (name, number, email) = (text(name)?, text(number)?, text(email)?);
            env.call_static_method(
                &class,
                jni::jni_str!("insert"),
                jni::jni_sig!("(Landroid/app/Activity;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V"),
                &[JValue::Object(&activity), JValue::Object(&name), JValue::Object(&number), JValue::Object(&email)],
            )
            .map(|_| ())
            .map_err(|e| {
                env.exception_clear();
                e.to_string()
            })
        })
    }
}
