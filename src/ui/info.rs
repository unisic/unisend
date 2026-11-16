//! A chat's info screen: its picture and name, quick actions, encryption, members, and the
//! photos, links and files found in its recent history. Full screen on a phone; on a wide window
//! a panel beside the conversation when there is room, else in the conversation's place.

use std::ops::Range;

use super::*;
use crate::backend::{Block, Details, MAX_MEMBERS, MAX_SCAN, Role};
use crate::i18n::{t, tn};

/// Kept of each kind; the history scan stops after `backend::info::MAX_SCAN` events anyway.
const MAX_ITEMS: usize = 200;
/// Links taken from one message: a list of fifty addresses is not fifty things to browse.
const LINKS_PER_MESSAGE: usize = 5;
/// Members listed before "Show all".
const MEMBERS_SHOWN: usize = 8;
pub(super) const INFO_W: f32 = 384.;
/// The conversation keeps at least this beside the panel; a narrower window swaps them.
const CHAT_MIN: f32 = 420.;
const BIG_AVATAR: f32 = 96.;
const MEMBER_AVATAR: f32 = 40.;
const CELL_MIN: f32 = 110.;
const CELL_GAP: f32 = 2.;
const ROW_H: f32 = 56.;

#[derive(Clone, Copy, PartialEq, Default)]
pub(super) enum Shelf {
    #[default]
    Media,
    Links,
    Files,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Row {
    Head,
    Actions,
    /// Facts about the chat on one surface. Leaving and disappearing messages join it here.
    Facts,
    Members,
    /// Adding people, for whoever may.
    Add,
    Member(usize),
    AllMembers,
    Shelves,
    /// A row of the media grid.
    Cells(usize),
    Link(usize),
    File(usize),
    Note,
}

#[derive(Clone)]
struct LinkItem {
    /// The message's event ID.
    id: String,
    url: String,
    title: String,
    site: String,
    image: Option<Media>,
}

#[derive(Clone)]
struct FileItem {
    id: String,
    media: Media,
    ts: u64,
}

pub(super) struct Info {
    pub(super) room: Option<OwnedRoomId>,
    pub(super) details: Option<Details>,
    media: Vec<(String, Media)>,
    links: Vec<LinkItem>,
    files: Vec<FileItem>,
    /// Events read, and whether the scan ended (end of history, budget, or an error).
    scanned: usize,
    done: bool,
    shelf: Shelf,
    all_members: bool,
    list: ListState,
    rows: Vec<Row>,
    cols: usize,
    /// When the screen came (true) or went, for the phone's push.
    nav: Option<(Instant, bool)>,
    /// The member whose sheet is up.
    pub(super) member: Option<group::Picked>,
    /// Editing the group or adding people, in the list's place.
    pub(super) page: Option<group::Page>,
}

impl Default for Info {
    fn default() -> Self {
        Self {
            room: None,
            details: None,
            media: Vec::new(),
            links: Vec::new(),
            files: Vec::new(),
            scanned: 0,
            done: false,
            shelf: Shelf::Media,
            all_members: false,
            list: ListState::new(0, ListAlignment::Top, px(300.)),
            rows: Vec::new(),
            cols: 3,
            nav: None,
            member: None,
            page: None,
        }
    }
}

/// Adds what a message holds to the shelves, each kept to `MAX_ITEMS`.
fn take(info: &mut Info, m: &Msg) {
    if m.deleted {
        return;
    }
    if let Some(media) = &m.media {
        match media.kind {
            MediaKind::Image | MediaKind::Video if info.media.len() < MAX_ITEMS => info.media.push((m.id.clone(), media.clone())),
            MediaKind::File | MediaKind::Audio if info.files.len() < MAX_ITEMS => info.files.push(FileItem { id: m.id.clone(), media: media.clone(), ts: m.ts }),
            _ => {}
        }
    }
    let room = MAX_ITEMS.saturating_sub(info.links.len());
    info.links.extend(links(m).into_iter().take(room));
}

/// The web addresses in a message: its preview's first, then those in the text and behind
/// formatted links, each once.
fn links(m: &Msg) -> Vec<LinkItem> {
    let mut out: Vec<LinkItem> = m
        .link
        .iter()
        .map(|l| LinkItem { id: m.id.clone(), url: l.url.clone(), title: l.title.clone(), site: l.site.clone(), image: l.image.clone() })
        .collect();
    let mut found: Vec<String> = urls(&m.body).into_iter().map(|r| m.body[r].to_owned()).collect();
    if let Some(rich) = &m.rich {
        hrefs(&rich.blocks, &mut found);
    }
    for url in found {
        if out.len() >= LINKS_PER_MESSAGE {
            break;
        }
        if !out.iter().any(|l| l.url == url) {
            out.push(LinkItem { id: m.id.clone(), url, title: String::new(), site: String::new(), image: None });
        }
    }
    out
}

fn hrefs(blocks: &[Block], out: &mut Vec<String>) {
    for block in blocks {
        match block {
            Block::Para(runs) => out.extend(
                runs.iter()
                    .filter_map(|r| r.href.as_deref())
                    .filter(|h| (h.starts_with("https://") || h.starts_with("http://")) && !h.contains("matrix.to/#/"))
                    .map(ToOwned::to_owned),
            ),
            Block::Quote(inner) => hrefs(inner, out),
            Block::List { items, .. } => items.iter().for_each(|item| hrefs(item, out)),
            Block::Code(..) => {}
        }
    }
}

fn rows(info: &Info, group: bool) -> Vec<Row> {
    let mut out = vec![Row::Head, Row::Actions, Row::Facts];
    if group && let Some(d) = &info.details {
        out.push(Row::Members);
        if d.can.invite {
            out.push(Row::Add);
        }
        let shown = if info.all_members { d.members.len() } else { d.members.len().min(MEMBERS_SHOWN) };
        out.extend((0..shown).map(Row::Member));
        if d.members.len() > shown || (info.all_members && d.joined > d.members.len() as u64) {
            out.push(Row::AllMembers);
        }
    }
    out.push(Row::Shelves);
    match info.shelf {
        Shelf::Media => out.extend((0..info.media.len().div_ceil(info.cols)).map(Row::Cells)),
        Shelf::Links => out.extend((0..info.links.len()).map(Row::Link)),
        Shelf::Files => out.extend((0..info.files.len()).map(Row::File)),
    }
    out.push(Row::Note);
    out
}

/// What stands under the shelf: progress, an empty shelf, or where the list stops.
fn note(info: &Info) -> Option<&'static str> {
    let len = match info.shelf {
        Shelf::Media => info.media.len(),
        Shelf::Links => info.links.len(),
        Shelf::Files => info.files.len(),
    };
    if !info.done {
        return Some(t!("Looking through the chat's history."));
    }
    // The scan ran out of budget rather than history when it read that many events.
    let partial = info.scanned >= MAX_SCAN;
    Some(match (info.shelf, len) {
        (_, MAX_ITEMS..) => t!("Only the newest 200 are shown."),
        (Shelf::Media, 0) if partial => t!("No photos or videos in the recent history."),
        (Shelf::Media, 0) => t!("No photos or videos."),
        (Shelf::Links, 0) if partial => t!("No links in the recent history."),
        (Shelf::Links, 0) => t!("No links."),
        (Shelf::Files, 0) if partial => t!("No files in the recent history."),
        (Shelf::Files, 0) => t!("No files."),
        _ if partial => t!("Older history is not searched."),
        _ => return None,
    })
}

/// The range of `old` that `new` replaces, and how many rows stand there now.
fn changed(old: &[Row], new: &[Row]) -> (Range<usize>, usize) {
    let head = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let tail = old[head..].iter().rev().zip(new[head..].iter().rev()).take_while(|(a, b)| a == b).count();
    (head..old.len() - tail, new.len() - head - tail)
}

/// How a bridge's identifier reads: a phone number as itself, others without their scheme.
fn identifier(id: &str) -> &str {
    id.split_once(':').map_or(id, |(_, rest)| rest)
}

fn role_label(role: Role) -> Option<&'static str> {
    match role {
        Role::Creator => Some(t!("Owner")),
        Role::Admin => Some(t!("Admin")),
        Role::Moderator => Some(t!("Moderator")),
        Role::Service => Some(t!("Bot")),
        Role::Member => None,
    }
}

impl BeeperApp {
    pub(super) fn info_open(&self) -> bool {
        self.info.room.is_some()
    }

    /// Opens the screen for the open chat, or closes it when it is already there.
    pub(super) fn toggle_info(&mut self, cx: &mut Context<Self>) {
        if self.info_open() {
            self.close_info(cx);
            return;
        }
        let Some(room) = self.open.clone() else { return };
        if !self.send(Cmd::Info(room.clone()), cx) {
            return;
        }
        let list = self.info.list.clone();
        list.reset(0);
        self.info = Info { room: Some(room), list, nav: Some((Instant::now(), true)), ..Info::default() };
        self.info_rows();
        cx.notify();
    }

    pub(super) fn close_info(&mut self, cx: &mut Context<Self>) {
        if self.info.page.take().is_some() {
            cx.notify();
        } else if self.end_info() {
            self.info.nav = Some((Instant::now(), false));
            cx.notify();
        }
    }

    /// Forgets the screen without a move: the chat under it is closing too.
    pub(super) fn end_info(&mut self) -> bool {
        if self.info.room.take().is_none() {
            return false;
        }
        let _ = self.cmd.try_send(Cmd::InfoClose);
        let list = self.info.list.clone();
        list.reset(0);
        self.info = Info { list, ..Info::default() };
        true
    }

    pub(super) fn info_details(&mut self, room: OwnedRoomId, details: Details, cx: &mut Context<Self>) {
        if self.info.room.as_ref() == Some(&room) {
            self.info.details = Some(details);
            self.info_rows();
            cx.notify();
        }
    }

    pub(super) fn info_items(&mut self, room: OwnedRoomId, items: Vec<Msg>, scanned: usize, done: bool, cx: &mut Context<Self>) {
        if self.info.room.as_ref() != Some(&room) {
            return;
        }
        for m in &items {
            take(&mut self.info, m);
        }
        (self.info.scanned, self.info.done) = (scanned, done);
        self.info_rows();
        cx.notify();
    }

    /// An attachment listed on the screen, for opening it once it is fetched.
    pub(super) fn info_media(&self, id: &str) -> Option<&Media> {
        let file = self.info.files.iter().find(|f| f.id == id).map(|f| &f.media);
        file.or_else(|| self.info.media.iter().find(|(m, _)| m == id).map(|(_, media)| media))
    }

    /// The phone's push: the screen comes in from the right, the chat returns from the left.
    pub(super) fn info_slide(&self, cx: &App) -> Option<(f32, f32)> {
        let (at, push) = self.info.nav?;
        let t = at.elapsed().as_secs_f32() / motion::PUSH.as_secs_f32();
        (t < 1. && !cx.reduce_motion()).then(|| (motion::ease_out(t), if push { 32. } else { -32. }))
    }

    /// Whether a wide window has room for the screen beside the conversation.
    pub(super) fn info_beside(window: &Window) -> bool {
        frame::viewport(window).width >= px(Self::nav_w(window) + LIST_W + INFO_W + CHAT_MIN)
    }

    /// What the panel takes from the conversation's width.
    pub(super) fn info_aside(&self, window: &Window) -> f32 {
        if self.info_open() && frame::viewport(window).width >= px(NARROW) && Self::info_beside(window) { INFO_W } else { 0. }
    }

    pub(super) fn info_rows(&mut self) {
        let group = self.room_summary().is_some_and(|r| !r.dm);
        let new = rows(&self.info, group);
        let (old_range, count) = changed(&self.info.rows, &new);
        if !old_range.is_empty() || count > 0 {
            self.info.list.splice(old_range, count);
        }
        // Rows keep their place but not their content: a name or a count may change their height.
        self.info.list.remeasure_items(0..new.len());
        self.info.rows = new;
    }

    fn room_summary(&self) -> Option<&RoomSummary> {
        let id = self.info.room.as_ref()?;
        self.rooms.iter().find(|r| &r.id == id)
    }

    fn info_thumb(&mut self, key: String, media: &Media) {
        if self.asked.contains(&key) || self.cmd.capacity() < self.cmd.max_capacity() / 2 {
            return;
        }
        if self.cmd.try_send(Cmd::Thumbs(vec![(key.clone(), media.clone())])).is_ok() {
            self.asked.insert(key);
        }
    }

    fn info_face(&mut self, user: &OwnedUserId) {
        if self.faces.contains_key(user) || self.faces.len() >= MAX_FACES || self.cmd.capacity() < self.cmd.max_capacity() / 2 {
            return;
        }
        let Some(room) = self.info.room.clone() else { return };
        if self.cmd.try_send(Cmd::Face(room, user.clone())).is_ok() {
            self.faces.insert(user.clone(), None);
        }
    }

    fn set_shelf(&mut self, shelf: Shelf, cx: &mut Context<Self>) {
        self.info.shelf = shelf;
        self.info_rows();
        cx.notify();
    }

    /// `beside` draws it as a panel next to the conversation, with a close button instead of back.
    pub(super) fn render_info(&mut self, beside: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let view = frame::viewport(window).width;
        let narrow = view < px(NARROW);
        // The rightmost pane, under the window buttons.
        let end = frame::reserve(window);
        let width = if beside {
            INFO_W
        } else if narrow {
            f32::from(view)
        } else {
            f32::from(view) - Self::nav_w(window) - LIST_W
        };
        let inner = width - 2. * GUTTER;
        let cols = (((inner + CELL_GAP) / (CELL_MIN + CELL_GAP)).floor() as usize).clamp(3, 6);
        if cols != self.info.cols {
            self.info.cols = cols;
            self.info_rows();
        }
        let cell = ((inner - CELL_GAP * (cols - 1) as f32) / cols as f32).floor();
        let body = list(
            self.info.list.clone(),
            cx.processor(move |this, ix: usize, window, cx| {
                let Some(&row) = this.info.rows.get(ix) else { return div().into_any_element() };
                this.render_info_row(row, cell, window, cx)
            }),
        )
        .flex_1()
        .min_h_0();
        let page_title = self.group_page_title();
        let body = match self.render_group_page(cx) {
            Some(page) => page,
            None => body.into_any_element(),
        };
        let close = round_button("info-close", if beside && page_title.is_none() { IconName::X } else { IconName::ArrowLeft }, cx)
            .on_click(cx.listener(|this, _, _, cx| this.close_info(cx)));
        let slide = self.info_slide(cx).filter(|_| self.info.nav.is_some_and(|(_, push)| push));
        if slide.is_some() {
            window.request_animation_frame();
        }
        v_flex()
            .size_full()
            .bg(p.ground)
            .when_some(slide, |el, (v, far)| el.relative().left(px(far * (1. - v))).opacity(v))
            .child(
                header().when(end > 0., |el| el.pr(px(GUTTER - 10. + end))).map(|el| {
                    let title = div().flex_1().min_w_0().pl(px(if beside && end == 0. { 10. } else { 4. })).child(screen_title(page_title.unwrap_or(t!("Chat info"))));
                    let title = frame::drag(title.when(end > 0., |el| el.h_full().flex().items_center()), window, cx);
                    // Its close button keeps away from the window's own close button.
                    if beside && end == 0. { el.child(title).child(close) } else { el.child(close).child(title) }
                }),
            )
            .child(body)
            .into_any_element()
    }

    fn render_info_row(&mut self, row: Row, cell: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let summary = self.room_summary().cloned();
        let details = self.info.details.clone();
        let edge = || div().w_full().px(px(GUTTER));
        match row {
            Row::Head => {
                let Some(room) = summary else { return div().into_any_element() };
                let (dm, network) = (room.dm, room.network.clone());
                let mut lines: Vec<String> = Vec::new();
                if let Some(d) = details.as_ref().filter(|_| dm) {
                    lines.extend(d.identifiers.iter().map(|id| identifier(id).to_owned()));
                    // A Matrix person is known by their ID; a bridged one's ID means nothing to people.
                    if lines.is_empty() && network.is_none() {
                        lines.extend(d.peer.as_ref().map(ToString::to_string));
                    }
                }
                // A group's size heads its member list.
                let sub = network.unwrap_or_default();
                v_flex()
                    .w_full()
                    .items_center()
                    .px(px(GUTTER))
                    .pt(px(8.))
                    .pb(px(GAP + 4.))
                    .gap(px(4.))
                    .child(avatar(&room.name, self.avatars.get(&room.id), BIG_AVATAR, cx).mb(px(8.)))
                    .child(div().max_w_full().text_center().text_size(fs(22.)).line_height(fs(28.)).font_weight(FontWeight::SEMIBOLD).child(room.name.clone()))
                    .children(lines.into_iter().map(|line| div().max_w_full().text_size(fs(16.)).text_color(p.muted).truncate().child(line)))
                    .when(!sub.is_empty(), |el| el.child(div().text_size(fs(14.)).text_color(p.faint).child(sub)))
                    .into_any_element()
            }
            Row::Actions => {
                let Some(room) = summary else { return div().into_any_element() };
                let [(_, read_icon, read), pin, mute, archive, _] = chat_actions(&room);
                // The sheet's "Mark as unread" does not fit a quarter of a phone.
                let unread = (if room.is_unread() { t!("Mark read") } else { t!("Mark unread") }, read_icon, read);
                // A muted chat's tile says how: "Muted until 14:30"; a tap unmutes it.
                let mute_state = self.mute_state(&room);
                let id = room.id.clone();
                edge()
                    .pb(px(GAP))
                    .child(h_flex().items_stretch().gap(px(8.)).children([mute, pin, archive, unread].into_iter().enumerate().map(|(ix, (label, icon, action))| {
                        let id = id.clone();
                        let (label, icon): (SharedString, _) = match (ix, &mute_state) {
                            (0, Some(state)) => (state.clone().into(), IconName::BellOff),
                            _ => (label.into(), icon),
                        };
                        v_flex()
                            .id(("chat-action", ix))
                            .flex_1()
                            .min_w_0()
                            .min_h(px(64.))
                            .py(px(8.))
                            .items_center()
                            .justify_center()
                            .gap(px(4.))
                            .rounded(px(16.))
                            .bg(p.surface)
                            .cursor_pointer()
                            .active(|el| el.bg(p.raised))
                            .child(Icon::new(icon).size_5())
                            .child(div().max_w_full().px(px(4.)).text_size(fs(12.)).line_height(fs(15.)).text_center().text_color(p.muted).line_clamp(2).text_ellipsis().child(label))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.chat_action(id.clone(), action, true, cx);
                            }))
                    })))
                    .into_any_element()
            }
            Row::Facts => {
                let (icon, text) = match details.as_ref().and_then(|d| d.encrypted) {
                    Some(true) => (IconName::Lock, t!("Messages are end-to-end encrypted.")),
                    Some(false) => (IconName::LockOpen, t!("Messages are not end-to-end encrypted.")),
                    None => (IconName::Lock, t!("Checking encryption.")),
                };
                let fact = |icon: IconName, text: SharedString| {
                    h_flex()
                        .px(px(GAP))
                        .min_h(px(52.))
                        .py(px(8.))
                        .gap(px(GAP))
                        .child(Icon::new(icon).size_5().flex_none().text_color(p.muted))
                        .child(div().flex_1().min_w_0().text_size(fs(14.)).line_height(fs(20.)).text_color(p.muted).child(text))
                };
                let topic = details.as_ref().and_then(|d| d.topic.clone());
                // The chat sheet's own rows, inset as the facts are.
                let link = summary.as_ref().filter(|r| r.dm).zip(details.as_ref()).and_then(|(r, d)| crate::backend::profile_link(r.network.as_deref(), &d.identifiers));
                let open = link.map(|(app, url)| {
                    sheet_row("info-open-in-app", IconName::ExternalLink, t!("Open in {app}", app = app), p.text, window, cx)
                        .px(px(GAP))
                        .on_click(move |_, _, cx| crate::open_link(cx, &url))
                });
                let room = summary.map(|r| r.id);
                let timer = room.as_ref().and_then(|id| self.disappear_entry(id, true, cx)).map(|row| row.px(px(GAP)));
                let edit = details.as_ref().filter(|d| d.can.edit()).map(|_| {
                    sheet_row("info-edit-group", IconName::Pencil, t!("Edit group"), p.text, window, cx)
                        .px(px(GAP))
                        .on_click(cx.listener(|this, _, window, cx| this.open_group_edit(window, cx)))
                });
                let leave = room.map(|id| self.leave_row(id, true, window, cx).px(px(GAP)));
                edge()
                    .pb(px(GAP))
                    .child(
                        v_flex()
                            .py(px(4.))
                            .rounded(px(RADIUS))
                            .bg(p.surface)
                            .children(topic.map(|t| fact(IconName::Info, t.into())))
                            .child(fact(icon, text.into()))
                            .children(open)
                            .children(edit)
                            .children(timer)
                            .children(leave),
                    )
                    .into_any_element()
            }
            Row::Members => {
                let text = match details.as_ref() {
                    Some(d) if d.members_loaded => tn!(d.joined.max(d.members.len() as u64), "{n} member", "{n} members"),
                    _ => t!("Loading members").to_owned(),
                };
                edge().pt(px(8.)).pb(px(4.)).text_size(fs(12.)).text_color(p.faint).child(text).into_any_element()
            }
            Row::Add => h_flex()
                .id("info-add-members")
                .w_full()
                .h(px(ROW_H))
                .px(px(GUTTER))
                .gap(px(12.))
                .cursor_pointer()
                .active(|el| el.bg(p.surface))
                .child(div().size(px(MEMBER_AVATAR)).flex().flex_none().items_center().justify_center().rounded_full().bg(p.surface).child(Icon::new(IconName::UserPlus).size_5().text_color(p.text)))
                .child(div().text_size(fs(16.)).child(t!("Add members")))
                .on_click(cx.listener(|this, _, window, cx| this.open_group_add(window, cx)))
                .into_any_element(),
            Row::Member(ix) => {
                let Some(m) = details.as_ref().and_then(|d| d.members.get(ix)).cloned() else { return div().into_any_element() };
                self.info_face(&m.id);
                // Bridge bots and the user have nothing to offer here.
                let tappable = m.role != Role::Service && self.user.as_ref() != Some(&m.id);
                h_flex()
                    .id(("info-member", ix))
                    .when(tappable, |el| {
                        let m = m.clone();
                        el.cursor_pointer().active(|el| el.bg(p.surface)).on_click(cx.listener(move |this, e: &ClickEvent, _, cx| this.open_member(m.clone(), e.position(), cx)))
                    })
                    .w_full()
                    .h(px(ROW_H))
                    .px(px(GUTTER))
                    .gap(px(12.))
                    .child(avatar(&m.name, self.faces.get(&m.id), MEMBER_AVATAR, cx))
                    .child(div().flex_1().min_w_0().text_size(fs(16.)).truncate().child(m.name.clone()))
                    .children(role_label(m.role).map(|r| div().flex_none().text_size(fs(12.)).text_color(p.faint).child(r)))
                    .into_any_element()
            }
            Row::AllMembers => {
                let Some(d) = details else { return div().into_any_element() };
                if self.info.all_members {
                    let text = t!("Showing {shown} of {all} members.", shown = d.members.len(), all = d.joined);
                    return edge().py(px(8.)).text_size(fs(14.)).text_color(p.faint).child(text).into_any_element();
                }
                let more = d.joined.max(d.members.len() as u64).min(MAX_MEMBERS as u64);
                h_flex()
                    .id("info-all-members")
                    .w_full()
                    .h(px(ROW_H))
                    .px(px(GUTTER))
                    .gap(px(12.))
                    .cursor_pointer()
                    .active(|el| el.bg(p.surface))
                    .child(div().size(px(MEMBER_AVATAR)).flex().flex_none().items_center().justify_center().child(Icon::new(IconName::ChevronDown).size_5().text_color(p.muted)))
                    .child(div().text_size(fs(16.)).text_color(p.muted).child(t!("Show all {more}", more = more)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.info.all_members = true;
                        this.info_rows();
                        cx.notify();
                    }))
                    .into_any_element()
            }
            Row::Shelves => {
                let current = self.info.shelf;
                // The pill glides to the chosen third; the labels switch colour at once.
                let slot = transition("shelf-pill", current as usize as f32, motion::enter(), window, cx);
                let choice = |label: &'static str, shelf: Shelf, cx: &mut Context<Self>| {
                    let on = current == shelf;
                    div()
                        .id(label)
                        .flex_1()
                        .h(px(36.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .text_size(fs(14.))
                        .text_color(if on { p.ground } else { p.muted })
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| this.set_shelf(shelf, cx)))
                        .child(label)
                };
                edge()
                    .pt(px(GAP))
                    .pb(px(GAP))
                    .child(
                        h_flex()
                            .relative()
                            .p(px(4.))
                            .rounded_full()
                            .bg(p.raised)
                            .child(div().absolute().top(px(4.)).bottom(px(4.)).left(px(4.)).right(px(4.)).child(
                                div().absolute().top_0().h_full().w(relative(1. / 3.)).left(relative(slot / 3.)).rounded_full().bg(p.text),
                            ))
                            .child(choice(t!("Media"), Shelf::Media, cx))
                            .child(choice(t!("Links"), Shelf::Links, cx))
                            .child(choice(t!("Files"), Shelf::Files, cx)),
                    )
                    .into_any_element()
            }
            Row::Cells(r) => {
                let cols = self.info.cols;
                let cells: Vec<(String, Media)> = self.info.media.iter().skip(r * cols).take(cols).cloned().collect();
                for (id, media) in &cells {
                    self.info_thumb(id.clone(), media);
                }
                edge()
                    .pb(px(CELL_GAP))
                    .child(h_flex().gap(px(CELL_GAP)).children(cells.into_iter().map(|(id, media)| {
                        let thumb = self.thumbs.get(&id).cloned();
                        let video = (media.kind == MediaKind::Video).then(|| media.duration.map(length));
                        div()
                            .id(SharedString::from(format!("info-cell-{id}")))
                            .relative()
                            .size(px(cell))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .overflow_hidden()
                            .bg(p.surface)
                            .cursor_pointer()
                            .map(|el| match &thumb {
                                Some(image) => el.child(img(image.clone()).size_full().object_fit(ObjectFit::Cover)),
                                None => el.child(Icon::new(if video.is_some() { IconName::Video } else { IconName::Image }).size_6().text_color(p.faint)),
                            })
                            .children(video.map(|length| {
                                h_flex()
                                    .absolute()
                                    .left(px(6.))
                                    .bottom(px(6.))
                                    .gap(px(2.))
                                    .px(px(4.))
                                    .rounded(px(6.))
                                    .bg(gpui_kit::black().opacity(0.6))
                                    .text_color(gpui_kit::white())
                                    .text_size(fs(12.))
                                    .child(Icon::new(IconName::Play).size_3())
                                    .children(length.map(|l| div().font_features(tabular()).child(l)))
                            }))
                            .on_click(cx.listener(move |this, _, _, cx| this.open_media(id.clone(), media.clone(), cx)))
                    })))
                    .into_any_element()
            }
            Row::Link(ix) => {
                let Some(link) = self.info.links.get(ix).cloned() else { return div().into_any_element() };
                let key = link_key(&link.id);
                if let Some(image) = &link.image {
                    self.info_thumb(key.clone(), image);
                }
                let thumb = link.image.as_ref().and_then(|_| self.thumbs.get(&key).cloned());
                let host = link.url.split("://").nth(1).and_then(|rest| rest.split(['/', '?', '#']).next()).unwrap_or_default().trim_start_matches("www.").to_owned();
                let (title, second) = if link.title.is_empty() { (host, link.url.clone()) } else { (link.title.clone(), if link.site.is_empty() { host } else { link.site.clone() }) };
                let url = link.url.clone();
                self.info_item(SharedString::from(format!("info-link-{ix}")), thumb, IconName::Link, title, second, cx)
                    .on_click(cx.listener(move |_, _, _, cx| crate::open_link(cx, &url)))
                    .into_any_element()
            }
            Row::File(ix) => {
                let Some(file) = self.info.files.get(ix).cloned() else { return div().into_any_element() };
                let second = if self.fetching.contains(&file.id) {
                    t!("Downloading").to_owned()
                } else {
                    [file.media.size.map(file_size), Some(day_label(file.ts))].into_iter().flatten().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
                };
                let icon = if file.media.kind == MediaKind::Audio { IconName::Music } else { IconName::File };
                let name = if file.media.name.is_empty() { t!("File").to_owned() } else { file.media.name.clone() };
                let (id, media) = (file.id.clone(), file.media.clone());
                self.info_item(SharedString::from(format!("info-file-{ix}")), None, icon, name, second, cx)
                    .on_click(cx.listener(move |this, _, _, cx| this.open_media(id.clone(), media.clone(), cx)))
                    .into_any_element()
            }
            Row::Note => match note(&self.info) {
                Some(text) => edge().pt(px(GAP)).pb(px(GAP * 2.)).text_size(fs(14.)).text_color(p.faint).text_center().child(text).into_any_element(),
                None => div().h(px(GAP * 2.)).into_any_element(),
            },
        }
    }

    /// A link or file: a square picture or icon, a line saying what it is and one about it.
    fn info_item(&self, id: SharedString, thumb: Option<Arc<Image>>, icon: IconName, title: String, second: String, cx: &App) -> Stateful<Div> {
        let p = Palette::of(cx);
        h_flex()
            .id(id)
            .w_full()
            .min_h(px(64.))
            .px(px(GUTTER))
            .py(px(8.))
            .gap(px(12.))
            .cursor_pointer()
            .active(|el| el.bg(p.surface))
            .child(div().size(px(48.)).flex().flex_none().items_center().justify_center().rounded(px(12.)).overflow_hidden().bg(p.surface).map(|el| match thumb {
                Some(image) => el.child(img(image).size_full().object_fit(ObjectFit::Cover)),
                None => el.child(Icon::new(icon).size_5().text_color(p.muted)),
            }))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_size(fs(16.)).line_height(fs(22.)).truncate().child(title))
                    .when(!second.is_empty(), |el| el.child(div().text_size(fs(14.)).line_height(fs(20.)).text_color(p.faint).truncate().child(second))),
            )
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` attribute would shadow the standard one.
    use super::{Block, Details, Info, Kind, LINKS_PER_MESSAGE, MAX_ITEMS, MAX_SCAN, MEMBERS_SHOWN, Msg, Role, Row, Shelf, changed, identifier, links, note, rows, take};
    use crate::backend::{Link, Rich, Run};

    fn msg(id: &str, body: &str) -> Msg {
        let mut m = Msg::bare(id.to_owned(), Kind::Message, "@ann:x".try_into().unwrap());
        m.body = body.to_owned();
        m
    }

    #[test]
    fn links_come_from_preview_text_and_markup_once() {
        let mut m = msg("$1", "see https://a.example/x and https://b.example.");
        m.link = Some(Link { url: "https://a.example/x".into(), title: "A".into(), description: String::new(), site: "a".into(), image: None });
        let run = |href: &str| Run { text: "here".into(), href: Some(href.into()), ..Run::default() };
        m.rich = Some(Rich { blocks: vec![Block::Para(vec![run("https://c.example"), run("https://matrix.to/#/@ann:x"), run("mailto:a@b")])] });
        let urls: Vec<String> = links(&m).into_iter().map(|l| l.url).collect();
        assert_eq!(urls, ["https://a.example/x", "https://b.example", "https://c.example"]);
        assert_eq!(links(&m)[0].title, "A");
    }

    #[test]
    fn shelves_stop_at_their_cap_and_skip_deleted() {
        let mut info = Info::default();
        let body = (0..LINKS_PER_MESSAGE + 3).map(|n| format!("https://x.example/{n}")).collect::<Vec<_>>().join(" ");
        for n in 0..MAX_ITEMS {
            take(&mut info, &msg(&format!("${n}"), &body));
        }
        assert_eq!(info.links.len(), MAX_ITEMS);
        let mut gone = msg("$gone", "https://y.example");
        gone.deleted = true;
        let mut fresh = Info::default();
        take(&mut fresh, &gone);
        assert!(fresh.links.is_empty());
        take(&mut fresh, &msg("$one", &body));
        assert_eq!(fresh.links.len(), LINKS_PER_MESSAGE);
    }

    #[test]
    fn rows_diff_to_the_changed_middle() {
        use Row::*;
        let old = [Head, Actions, Shelves, Cells(0), Note];
        assert_eq!(changed(&old, &[Head, Actions, Shelves, Cells(0), Cells(1), Note]), (4..4, 1));
        assert_eq!(changed(&old, &[Head, Actions, Shelves, Link(0), Note]), (3..4, 1));
        assert_eq!(changed(&old, &old), (5..5, 0));
        assert_eq!(changed(&[], &old), (0..0, 5));
        assert_eq!(changed(&old, &[Head, Note]), (1..4, 0));
    }

    #[test]
    fn group_rows_show_a_few_members_then_all() {
        let mut info = Info::default();
        let member = |n: usize| crate::backend::Member { id: format!("@u{n}:x").try_into().unwrap(), name: format!("u{n}"), role: Role::Member, removable: false };
        info.details = Some(Details { members: (0..12).map(member).collect(), joined: 300, members_loaded: true, ..Details::default() });
        let r = rows(&info, true);
        assert_eq!(r.iter().filter(|r| matches!(r, Row::Member(_))).count(), MEMBERS_SHOWN);
        assert!(r.contains(&Row::AllMembers));
        info.all_members = true;
        let r = rows(&info, true);
        assert_eq!(r.iter().filter(|r| matches!(r, Row::Member(_))).count(), 12);
        assert!(r.contains(&Row::AllMembers), "says the list is cut");
        assert!(!rows(&info, false).contains(&Row::Members));
    }

    #[test]
    fn notes_tell_progress_emptiness_and_limits() {
        let mut info = Info::default();
        assert_eq!(note(&info), Some("Looking through the chat's history."));
        info.done = true;
        assert_eq!(note(&info), Some("No photos or videos."));
        info.scanned = MAX_SCAN;
        assert_eq!(note(&info), Some("No photos or videos in the recent history."));
        info.shelf = Shelf::Links;
        take(&mut info, &msg("$1", "https://a.example"));
        assert_eq!(note(&info), Some("Older history is not searched."));
        info.scanned = 10;
        assert_eq!(note(&info), None);
        assert_eq!(identifier("tel:+48123"), "+48123");
        assert_eq!(identifier("plain"), "plain");
    }
}
