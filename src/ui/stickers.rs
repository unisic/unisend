//! The emoji panel's sticker tab: the packs of the account and the open chat (`backend/stickers.rs`)
//! in a grid, recently sent ones first; a tap sends one as `m.sticker`. Shown only for the composer
//! and only where the chat's network takes stickers (`com.beeper.room_features`), each sticker only
//! when it takes its type. No pack comes from outside the account's own rooms.
//!
//! Memory: at most `MAX_THUMBS` pictures of `stickers::THUMB_PX` (64 KB each decoded, 6 MB in all),
//! the oldest dropped first and all of them when the chat closes; the grid builds only the rows on
//! screen.

use super::*;
use crate::backend::stickers::{Pack, Sticker};
use crate::i18n::t;

const MAX_THUMBS: usize = 96;
const RECENTS: usize = 24;
const FILE: &str = "sticker-recents.json";
/// One cell of the grid, and the height of a row.
const CELL: f32 = 80.;
const PAD: f32 = 8.;

enum Row {
    Head(String),
    /// (pack, sticker) indices.
    Cells(Vec<(usize, usize)>),
}

pub(super) struct Stickers {
    /// The emoji panel shows the stickers.
    pub(super) shown: bool,
    room: Option<OwnedRoomId>,
    /// `None` until the chat's packs arrive.
    packs: Option<Vec<Pack>>,
    /// Sent lately, newest first, by URL.
    recents: Vec<String>,
    loaded: bool,
    rows: Vec<Row>,
    /// Columns `rows` was built for; `None` once packs or recents changed.
    built: Option<usize>,
    thumbs: HashMap<String, Arc<Image>>,
    order: VecDeque<String>,
    asked: HashSet<String>,
    list: ListState,
}

impl Default for Stickers {
    fn default() -> Self {
        Self {
            shown: false,
            room: None,
            packs: None,
            recents: Vec::new(),
            loaded: false,
            rows: Vec::new(),
            built: None,
            thumbs: HashMap::new(),
            order: VecDeque::new(),
            asked: HashSet::new(),
            list: ListState::new(0, ListAlignment::Top, px(400.)),
        }
    }
}

/// The rows for `cols` columns: the recents found in `packs`, then each pack under its name;
/// only what `takes` lets through.
fn build(cols: usize, packs: &[Pack], recents: &[String], takes: &dyn Fn(&Sticker) -> bool) -> Vec<Row> {
    let cols = cols.max(1);
    let mut rows = Vec::new();
    let find = |url: &str| packs.iter().enumerate().find_map(|(p, pack)| pack.stickers.iter().position(|s| s.url.as_str() == url && takes(s)).map(|s| (p, s)));
    let recent: Vec<(usize, usize)> = recents.iter().filter_map(|url| find(url)).collect();
    let mut push = |head: String, cells: Vec<(usize, usize)>| {
        if !cells.is_empty() {
            rows.push(Row::Head(head));
            rows.extend(cells.chunks(cols).map(|c| Row::Cells(c.to_vec())));
        }
    };
    push(t!("Recent").to_owned(), recent);
    for (p, pack) in packs.iter().enumerate() {
        push(pack.name.clone(), pack.stickers.iter().enumerate().filter(|(_, s)| takes(s)).map(|(s, _)| (p, s)).collect());
    }
    rows
}

impl BeeperApp {
    /// Opens the sticker tab, asking for the open chat's packs once per chat.
    pub(super) fn show_stickers(&mut self, cx: &mut Context<Self>) {
        let Some(room) = self.open.clone() else { return };
        let s = &mut self.stickers;
        if !s.loaded {
            s.recents = std::fs::read(self.dir.join(FILE)).ok().and_then(|raw| serde_json::from_slice(&raw).ok()).unwrap_or_default();
            s.recents.truncate(RECENTS);
            s.loaded = true;
        }
        if s.room.as_ref() != Some(&room) {
            (s.room, s.packs, s.built) = (Some(room.clone()), None, None);
            let _ = self.cmd.try_send(Cmd::Stickers(room));
        }
        self.stickers.shown = true;
        self.stickers.built = None;
        cx.notify();
    }

    pub(super) fn stickers_arrived(&mut self, room: OwnedRoomId, packs: Vec<Pack>, cx: &mut Context<Self>) {
        if self.stickers.room.as_ref() == Some(&room) {
            (self.stickers.packs, self.stickers.built) = (Some(packs), None);
            cx.notify();
        }
    }

    pub(super) fn sticker_thumb(&mut self, url: String, bytes: Vec<u8>, cx: &mut Context<Self>) {
        let s = &mut self.stickers;
        let Some(format) = image_format(&bytes).filter(|_| s.asked.contains(&url)) else { return };
        s.thumbs.insert(url.clone(), Arc::new(Image::from_bytes(format, bytes)));
        s.order.push_back(url);
        while s.order.len() > MAX_THUMBS {
            let Some(old) = s.order.pop_front() else { break };
            s.asked.remove(&old);
            if let Some(image) = s.thumbs.remove(&old) {
                self.released.push(image, cx);
            }
        }
        cx.notify();
    }

    /// Lets go of the chat's packs and pictures, when it closes.
    pub(super) fn forget_stickers(&mut self, cx: &mut Context<Self>) {
        let s = &mut self.stickers;
        (s.shown, s.room, s.packs, s.built) = (false, None, None, None);
        s.rows.clear();
        s.order.clear();
        s.asked.clear();
        for (_, image) in s.thumbs.drain() {
            self.released.push(image, cx);
        }
    }

    /// The composer's chat takes some sticker.
    pub(super) fn stickers_offered(&self) -> bool {
        self.caps().takes_sticker(None)
    }

    fn send_sticker(&mut self, pack: usize, ix: usize, cx: &mut Context<Self>) {
        let Some(sticker) = self.stickers.packs.as_ref().and_then(|p| p.get(pack)?.stickers.get(ix).cloned()) else { return };
        let Some(room) = self.open.clone() else { return };
        let reply_to = match std::mem::replace(&mut self.compose, Compose::New) {
            Compose::Reply(id) => Some(id),
            _ => None,
        };
        let url = sticker.url.to_string();
        if self.send(Cmd::SendSticker { room, sticker, reply_to }, cx) {
            let s = &mut self.stickers;
            s.recents.retain(|r| *r != url);
            s.recents.insert(0, url);
            s.recents.truncate(RECENTS);
            s.built = None;
            if let Err(e) = serde_json::to_vec(&s.recents).map_err(std::io::Error::other).and_then(|raw| std::fs::write(self.dir.join(FILE), raw)) {
                log::warn!("{FILE}: {e}");
            }
        }
        // Signal keeps the phone's panel up for the next one; a desktop's picker closes as Signal Desktop's does.
        if !gpui_kit::is_mobile() {
            self.close_emoji(cx);
        }
        cx.notify();
    }

    fn want_sticker(&mut self, url: &str) {
        let s = &mut self.stickers;
        if s.asked.contains(url) || self.cmd.capacity() < self.cmd.max_capacity() / 2 {
            return;
        }
        let Some(mxc) = s.packs.iter().flatten().flat_map(|p| &p.stickers).find(|st| st.url.as_str() == url).map(|st| st.url.clone()) else { return };
        if self.cmd.try_send(Cmd::StickerThumb(mxc)).is_ok() {
            s.asked.insert(url.to_owned());
        }
    }

    /// The tab that turns the emoji panel to stickers (`to: true`) or back, lit when it shows them.
    pub(super) fn sticker_tab(&self, to: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let p = Palette::of(cx);
        let on = to == self.stickers.shown;
        div()
            .id(if to { "sticker-tab" } else { "sticker-tab-emoji" })
            .h(px(40.))
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .text_color(if on { p.text } else { p.faint })
            .child(Icon::new(if to { IconName::Sticker } else { IconName::FaceSlightlySmiling }).size_5())
            .on_click(cx.listener(move |this, _, _, cx| {
                if to {
                    this.show_stickers(cx);
                } else {
                    this.stickers.shown = false;
                    cx.notify();
                }
            }))
    }

    /// The sticker grid and, under it, the tabs back to the emoji and of the stickers.
    pub(super) fn sticker_body(&mut self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let tabs = h_flex().h(px(40.)).px(px(PAD)).child(self.sticker_tab(false, cx)).child(self.sticker_tab(true, cx));
        let p = Palette::of(cx);
        let cols = (((width - 2. * PAD) / CELL) as usize).max(1);
        let cell_w = ((width - 2. * PAD) / cols as f32).max(CELL);
        if self.stickers.built != Some(cols) {
            let caps = self.caps().clone();
            let s = &mut self.stickers;
            s.rows = build(cols, s.packs.as_deref().unwrap_or_default(), &s.recents, &|st| caps.takes_sticker(st.mime()));
            s.list.reset(s.rows.len());
            s.built = Some(cols);
        }
        let grid = if self.stickers.rows.is_empty() {
            let note = if self.stickers.packs.is_none() { t!("Loading…") } else { t!("No sticker packs. Packs added in another Matrix app show here.") };
            div().flex_1().flex().items_center().justify_center().px(px(24.)).text_center().text_size(fs(14.)).text_color(p.faint).child(note).into_any_element()
        } else {
            // Rows differ in height (a pack's name, a row of stickers): `list`, not `uniform_list`.
            list(
                self.stickers.list.clone(),
                cx.processor(move |this, ix: usize, _, cx| {
                    let p = Palette::of(cx);
                    let cells = match this.stickers.rows.get(ix) {
                        Some(Row::Head(name)) => {
                            return div().h(px(36.)).px(px(12.)).flex().items_end().pb(px(6.)).text_size(fs(13.)).text_color(p.faint).truncate().child(name.clone()).into_any_element();
                        }
                        Some(Row::Cells(cells)) => cells.clone(),
                        None => return div().into_any_element(),
                    };
                    let mut row = h_flex().h(px(CELL)).px(px(PAD));
                    for (n, (pack, s)) in cells.into_iter().enumerate() {
                        let Some(sticker) = this.stickers.packs.as_ref().and_then(|ps| ps.get(pack)?.stickers.get(s)) else { continue };
                        let url = sticker.url.to_string();
                        this.want_sticker(&url);
                        let picture = this.stickers.thumbs.get(&url).cloned();
                        row = row.child(
                            div()
                                .id(("sticker", ix * 64 + n))
                                .w(px(cell_w))
                                .h(px(CELL))
                                .flex_none()
                                .p(px(6.))
                                .rounded(px(10.))
                                .cursor_pointer()
                                .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.raised)))
                                .active(|el| el.bg(p.raised))
                                .children(picture.map(|image| img(image).size_full().object_fit(ObjectFit::Contain)))
                                .on_click(cx.listener(move |this, _, _, cx| this.send_sticker(pack, s, cx))),
                        );
                    }
                    row.into_any_element()
                }),
            )
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        v_flex().size_full().pt(px(PAD)).child(grid).child(tabs).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{Pack, Row, Sticker, build};

    fn pack(name: &str, urls: &[&str]) -> Pack {
        let sticker = |url: &&str| Sticker { url: (*url).into(), body: String::new(), info: Default::default() };
        Pack { name: name.into(), stickers: urls.iter().map(sticker).collect() }
    }

    fn shape(rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| match r { Row::Head(h) => h.clone(), Row::Cells(c) => format!("{c:?}") }).collect()
    }

    #[test]
    fn recents_lead_then_each_pack_in_rows() {
        let packs = [pack("Cats", &["mxc://x/a", "mxc://x/b", "mxc://x/c"]), pack("Dogs", &["mxc://x/d"])];
        let recents = ["mxc://x/d".to_owned(), "mxc://x/gone".to_owned()];
        let rows = build(2, &packs, &recents, &|_| true);
        assert_eq!(shape(&rows), ["Recent", "[(1, 0)]", "Cats", "[(0, 0), (0, 1)]", "[(0, 2)]", "Dogs", "[(1, 0)]"]);
        let rows = build(2, &packs, &[], &|s| s.url.as_str() != "mxc://x/d");
        assert_eq!(shape(&rows), ["Cats", "[(0, 0), (0, 1)]", "[(0, 2)]"], "a pack with nothing the chat takes is left out");
    }
}
