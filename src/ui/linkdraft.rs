//! The preview of a link being typed, over the composer, as Signal shows it: the page is read
//! once typing pauses, and the preview goes with the message unless the user closes it.

use super::*;

/// Typing pause after which the link's page is read, so a link half typed is not.
const LINK_WAIT: Duration = Duration::from_millis(700);
const PICTURE: f32 = 48.;

pub(super) struct LinkCard {
    pub room: OwnedRoomId,
    pub url: String,
    pub made: Made,
}

pub(super) enum Made {
    Reading,
    Shown { title: SharedString, site: SharedString, picture: Option<Arc<Image>> },
    /// The page gave nothing to show.
    Nothing,
}

impl BeeperApp {
    /// The composer changed: its first link gets a preview, made after a pause; another link, or
    /// none, drops the one there was.
    pub(super) fn link_typed(&mut self, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        let wanted = self.settings.link_previews && !matches!(self.compose, Compose::Edit(_));
        let found = urls(&text).into_iter().map(|r| text[r].to_owned()).find(|url| wanted && crate::backend::link_previewable(url));
        if self.link_closed.is_some() && self.link_closed != found {
            self.link_closed = None;
        }
        let Some(room) = self.open.clone() else { return };
        if found.is_some() && self.link.as_ref().is_some_and(|l| l.room == room && Some(&l.url) == found.as_ref()) {
            return;
        }
        self.close_link(false, cx);
        let Some(url) = found.filter(|url| self.link_closed.as_ref() != Some(url)) else { return };
        self.link_wait = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LINK_WAIT).await;
            let _ = this.update(cx, |this, cx| {
                this.link_wait = None;
                if this.open.as_ref() == Some(&room) && this.cmd.try_send(Cmd::LinkPreview(Some(url.clone()))).is_ok() {
                    this.link = Some(LinkCard { room, url, made: Made::Reading });
                    cx.notify();
                }
            });
        }));
    }

    /// Drops the preview, and with `by_user` keeps it from coming back while its link stays.
    pub(super) fn close_link(&mut self, by_user: bool, cx: &mut Context<Self>) {
        self.link_wait = None;
        let Some(link) = self.link.take() else { return };
        let _ = self.cmd.try_send(Cmd::LinkPreview(None));
        if let Made::Shown { picture: Some(image), .. } = link.made {
            self.released.push(image, cx);
        }
        if by_user {
            self.link_closed = Some(link.url);
        }
        cx.notify();
    }

    pub(super) fn render_link_card(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let link = self.link.as_ref().filter(|l| self.open.as_ref() == Some(&l.room))?;
        let p = Palette::of(cx);
        let host: SharedString = matrix_sdk::reqwest::Url::parse(&link.url).ok().and_then(|u| u.host_str().map(|h| h.trim_start_matches("www.").to_owned())).unwrap_or_default().into();
        let (title, site, picture) = match &link.made {
            Made::Reading => (SharedString::from(t!("Loading…")), host, None),
            Made::Shown { title, site, picture } => (title.clone(), if site.is_empty() { host } else { site.clone() }, picture.clone()),
            Made::Nothing => return None,
        };
        let v = Presence::new(format!("link-card-{}-{}", link.room, link.url), true).transition(motion::enter()).sample(window, cx).progress;
        let reading = matches!(link.made, Made::Reading);
        let thumb = div()
            .flex_none()
            .size(px(PICTURE))
            .rounded(px(10.))
            .overflow_hidden()
            .bg(p.text.opacity(0.07))
            .map(|el| match picture {
                Some(image) => el.child(img(image).size_full().object_fit(ObjectFit::Cover)),
                None => el.flex().items_center().justify_center().text_color(p.faint).child(Icon::new(IconName::Link).size_5()),
            });
        let line = |size: f32, height: f32| div().text_size(fs(size)).line_height(fs(height)).min_w_0().truncate();
        Some(
            div().flex_none().overflow_hidden().max_h(px((PICTURE + 24.) * v)).opacity(v).child(
                h_flex()
                    .mx(px(GUTTER))
                    .mt(px(8.))
                    .p(px(8.))
                    .gap(px(10.))
                    .rounded(px(RADIUS))
                    .bg(p.surface)
                    .child(thumb)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(line(14., 19.).font_weight(FontWeight::SEMIBOLD).text_color(if reading { p.faint } else { p.text }).child(title))
                            .child(line(12., 16.).text_color(p.faint).child(site)),
                    )
                    .child(round_button("link-card-close", IconName::X, t!("Remove link preview"), cx).on_click(cx.listener(|this, _, window, cx| {
                        this.close_link(true, cx);
                        this.composer.update(cx, |c, cx| c.focus(window, cx));
                    }))),
            ),
        )
    }
}
