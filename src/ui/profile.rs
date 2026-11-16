//! Settings' profile: the user's name and picture as the homeserver keeps them, at the top of
//! Settings and on a page to change them. On Beeper this is the Beeper account's Matrix profile;
//! bridged networks keep the name and picture the user has there.

use super::panels::{Page, card, footnote};
use super::*;
use crate::backend::group::{Mine, NAME_MAX, Profile};
use crate::i18n::t;

const FACE: f32 = 64.;
const BIG: f32 = 96.;

pub(super) struct Ui {
    name: String,
    picture: Option<Arc<Image>>,
    field: Entity<InputState>,
    loaded: bool,
}

impl Ui {
    pub(super) fn new(window: &mut Window, cx: &mut Context<BeeperApp>, subs: &mut Vec<Subscription>) -> Self {
        let field = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Your name")));
        crate::ime::follow(&field, window, cx);
        subs.push(cx.subscribe_in(&field, window, |this, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::PressEnter { shift: false, .. } => this.save_profile_name(cx),
            other => keyboard_follows(other, Keyboard::Text, cx),
        }));
        Self { name: String::new(), picture: None, field, loaded: false }
    }

    /// The name and picture, handed to the switcher as the window turns to another account.
    pub(super) fn take(&mut self) -> (String, Option<Arc<Image>>) {
        (std::mem::take(&mut self.name), self.picture.take())
    }

    /// What the switcher kept of the account, shown until the server's answer replaces it.
    pub(super) fn show(&mut self, name: String, picture: Option<Arc<Image>>) {
        (self.name, self.picture) = (name, picture);
    }

    pub(super) fn picture(&self) -> Option<Arc<Image>> {
        self.picture.clone()
    }
}

impl BeeperApp {
    pub(super) fn profile_arrived(&mut self, profile: Profile, cx: &mut Context<Self>) {
        let picture = profile.picture.and_then(|bytes| Some(Arc::new(Image::from_bytes(image_format(&bytes)?, bytes))));
        if let Some(old) = std::mem::replace(&mut self.profile.picture, picture) {
            old.remove_asset(cx);
        }
        (self.profile.name, self.profile.loaded) = (profile.name, true);
        cx.notify();
    }

    pub(super) fn profile_name(&self) -> String {
        if self.profile.name.is_empty() { self.user.as_ref().map(|u| u.localpart().to_owned()).unwrap_or_default() } else { self.profile.name.clone() }
    }

    /// The first card of Settings: picture, name and ID, leading to the profile page.
    pub(super) fn profile_card(&self, selected: bool, split: bool, cx: &mut Context<Self>) -> Div {
        let p = Palette::of(cx);
        let user = self.user.as_ref().map(ToString::to_string).unwrap_or_default();
        let name = self.profile_name();
        let toggle = self.accounts_toggle(cx);
        card(cx).child(
            h_flex().gap(px(4.)).child(
            h_flex()
                .id("settings-profile")
                .flex_1()
                .min_w_0()
                .min_h(px(FACE + 16.))
                .py(px(8.))
                .mx(px(-8.))
                .px(px(8.))
                .gap(px(GAP))
                .rounded(px(12.))
                .role(Role::Button)
                .aria_label(t!("Profile"))
                .cursor_pointer()
                .when(selected, |el| el.bg(p.lifted))
                .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.raised)))
                .active(|el| el.bg(p.raised))
                .child(avatar(&name, Some(&self.profile.picture), FACE, cx))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_size(fs(18.)).line_height(fs(24.)).font_weight(FontWeight::SEMIBOLD).truncate().child(name))
                        .child(div().text_size(fs(14.)).line_height(fs(20.)).text_color(p.faint).truncate().child(user)),
                )
                .child(Icon::new(IconName::ChevronRight).size_4().flex_none().text_color(p.faint))
                .on_click(cx.listener(move |this, _, window, cx| {
                    if split {
                        this.settings_pages.clear();
                    }
                    this.open_profile(window, cx);
                })),
            )
            // Beside the row, not in it: a click on it must not open the profile too.
            .child(toggle),
        )
    }

    fn open_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.profile.name.clone();
        self.profile.field.update(cx, |input, cx| input.set_value(name, window, cx));
        self.open_page(Page::Profile, cx);
    }

    fn save_profile_name(&mut self, cx: &mut Context<Self>) {
        let name = self.profile.field.read(cx).value().trim().to_owned();
        if !name.is_empty() && name != self.profile.name && name.chars().count() <= NAME_MAX {
            self.send(Cmd::SetProfile(Mine::Name(name)), cx);
        }
    }

    fn choose_profile_photo(&mut self, cx: &mut Context<Self>) {
        let picked = crate::pick_file(cx);
        cx.spawn(async move |this, cx| {
            let Some(path) = picked.await else { return };
            let _ = this.update(cx, |this, cx| {
                this.send(Cmd::SetProfile(Mine::Photo(path)), cx);
            });
        })
        .detach();
    }

    pub(super) fn render_profile(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let name = self.profile_name();
        let note = if self.beeper {
            t!("This is your Beeper account's name and picture. WhatsApp, Signal, Messenger and the other networks keep the name and picture you have there; change them in those apps.")
        } else {
            t!("People on Matrix see this name and picture.")
        };
        v_flex()
            .gap(px(GAP * 2.))
            .child(
                v_flex()
                    .items_center()
                    .gap(px(GAP))
                    .child(avatar(&name, Some(&self.profile.picture), BIG, cx))
                    .child(Button::new("profile-photo").label(t!("Change photo")).on_click(cx.listener(|this, _, _, cx| this.choose_profile_photo(cx)))),
            )
            .child(
                card(cx).py(px(8.)).gap(px(8.)).child(div().text_size(fs(12.)).text_color(p.faint).child(t!("Name"))).child(Input::new(&self.profile.field)).child(crate::ime::marks(&self.profile.field)).child(
                    h_flex().pb(px(4.)).child(Button::new("profile-save").primary().label(t!("Save")).on_click(cx.listener(|this, _, _, cx| this.save_profile_name(cx)))),
                ),
            )
            .child(footnote(note, cx))
            .when(!self.profile.loaded, |el| el.child(footnote(t!("Loading…"), cx)))
            .into_any_element()
    }
}
