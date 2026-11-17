//! The "Send to" screen for what other apps share into this one (see `crate::share`): a chat
//! picker, most recent first, with search; the shared files; an optional message; Send. Shared
//! text alone goes into the chosen chat's message box instead of being sent, as in Signal.

use std::path::Path;

use super::*;
use crate::i18n::{t, tn};

const ROW_H: f32 = 64.;
const ROW_AVATAR: f32 = 40.;
/// Widest the screen's column grows on a wide window.
const COLUMN_W: f32 = 560.;

pub(super) struct ShareIn {
    pub open: bool,
    target: Option<OwnedRoomId>,
    text: String,
    /// Copies in the cache with their sizes; deleted again when the share is cancelled.
    files: Vec<(PathBuf, u64)>,
    /// Files still being copied; 0 once the copy is done.
    copying: usize,
    over: usize,
    unreadable: usize,
    /// Bumped by every new or closed share, so a copy finishing late is thrown away.
    epoch: usize,
    /// The message field still has to take `text`: filling it needs the window.
    fill: bool,
    /// Text for the message box of the chat just opened, once the window is at hand.
    to_composer: Option<String>,
    /// Indices into `rooms` the list shows, most recent first.
    rows: Vec<usize>,
    /// The chats in the order they were first listed, so a message arriving does not move the
    /// row under a finger about to tap it.
    order: Vec<OwnedRoomId>,
    /// Chats last offered in other apps' share sheets.
    published: Vec<OwnedRoomId>,
    query: Entity<InputState>,
    message: Entity<TextareaState>,
}

impl ShareIn {
    pub(super) fn new(window: &mut Window, cx: &mut Context<BeeperApp>, subs: &mut Vec<Subscription>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Search chats")));
        let message = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 4).submit_on_enter(true).placeholder(t!("Add a message")));
        crate::ime::follow(&query, window, cx);
        crate::ime::follow(&message, window, cx);
        subs.push(cx.subscribe_in(&query, window, |_, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::Change => cx.notify(),
            other => keyboard_follows(other, Keyboard::Text, cx),
        }));
        subs.push(cx.subscribe_in(&message, window, |this, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::PressEnter { shift: false, .. } => this.share_send(cx),
            other => keyboard_follows(other, Keyboard::Text, cx),
        }));
        Self {
            open: false,
            target: None,
            text: String::new(),
            files: Vec::new(),
            copying: 0,
            over: 0,
            unreadable: 0,
            epoch: 0,
            fill: false,
            to_composer: None,
            rows: Vec::new(),
            order: Vec::new(),
            published: Vec::new(),
            query,
            message,
        }
    }

    fn text_only(&self) -> bool {
        self.files.is_empty() && self.copying == 0
    }
}

/// What the screen says about files that did not make it, or `None` when all did.
fn shortfall(over: usize, unreadable: usize) -> Option<String> {
    let (files, mb, each) = (crate::share::MAX_FILES, crate::share::MAX_BYTES >> 20, crate::backend::MAX_UPLOAD >> 20);
    let over = match over {
        0 => None,
        n => Some(tn!(
            n,
            "{n} file was left out: one share takes at most {files} files and {mb} MB, each file at most {each} MB.",
            "{n} files were left out: one share takes at most {files} files and {mb} MB, each file at most {each} MB.",
            files = files,
            mb = mb,
            each = each
        )),
    };
    let unreadable = match unreadable {
        0 => None,
        n => Some(tn!(n, "{n} file could not be read.", "{n} files could not be read.")),
    };
    match (over, unreadable) {
        (Some(a), Some(b)) => Some(format!("{a} {b}")),
        (a, b) => a.or(b),
    }
}

/// Chats as the picker lists them, filtered by name: most recent first, pinned or not, in the
/// order they had when first listed (`order`); chats that appear later go below.
fn pick_order(rooms: &[RoomSummary], query: &str, order: &mut Vec<OwnedRoomId>) -> Vec<usize> {
    let at: HashMap<&OwnedRoomId, usize> = rooms.iter().enumerate().map(|(ix, r)| (&r.id, ix)).collect();
    order.retain(|id| at.contains_key(id));
    let listed: HashSet<&OwnedRoomId> = order.iter().collect();
    let mut fresh: Vec<&RoomSummary> = rooms.iter().filter(|r| !listed.contains(&r.id)).collect();
    fresh.sort_by_key(|r| std::cmp::Reverse(r.ts));
    order.extend(fresh.into_iter().map(|r| r.id.clone()));
    let query = query.trim().to_lowercase();
    order.iter().map(|id| at[id]).filter(|&ix| query.is_empty() || rooms[ix].name.to_lowercase().contains(&query)).collect()
}

impl BeeperApp {
    /// Takes a share the app was opened or brought forward with, once the user is signed in.
    /// `window` is there unless the user just signed in, when nothing else can be open.
    pub(super) fn take_share(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        if self.screen != Screen::Main {
            return;
        }
        let Some(incoming) = crate::share::take() else { return };
        if incoming.uris.is_empty() && incoming.text.is_empty() {
            return;
        }
        self.close_share(cx);
        self.close_menu(cx);
        self.close_gallery(cx);
        self.close_viewer(cx);
        self.close_location(cx);
        self.close_emoji(cx);
        self.close_poll_form(cx);
        self.close_gifs(cx);
        self.close_confirm(cx);
        while self.forward_back(cx) {}
        if let Some(window) = window
            && self.preview.open()
        {
            self.close_preview(true, window, cx);
        }
        self.chat_sheet = None;
        self.gallery.menu = false;
        // A target of an account not shown, left from before a switch, gives way to the picker.
        let target = incoming.chat.and_then(|chat| {
            let (slot, room) = crate::accounts::split(&chat);
            let room = OwnedRoomId::try_from(room).map_err(|e| log::warn!("share target: {e}")).ok();
            room.filter(|_| slot == self.accounts.active())
        });
        self.share.text = incoming.text;
        // A Direct Share target with text alone needs no screen.
        if incoming.uris.is_empty()
            && let Some(room) = target
        {
            self.share_into_composer(room, cx);
            return;
        }
        let s = &mut self.share;
        (s.open, s.target, s.copying, s.fill) = (true, target, incoming.uris.len(), true);
        let epoch = s.epoch;
        if incoming.uris.is_empty() {
            return cx.notify();
        }
        cx.spawn(async move |this, cx| {
            let copied = cx
                .background_spawn(async move {
                    let copied = crate::share::copy(&incoming.uris);
                    let sized: Vec<(PathBuf, u64)> = copied.paths.iter().map(|p| (p.clone(), std::fs::metadata(p).map_or(0, |m| m.len()))).collect();
                    (copied, sized)
                })
                .await;
            let (copied, sized) = copied;
            let stale = this
                .update(cx, |this, cx| {
                    let s = &mut this.share;
                    if s.epoch != epoch {
                        return true;
                    }
                    (s.files, s.copying, s.over, s.unreadable) = (sized, 0, copied.over, copied.unreadable);
                    if s.files.is_empty() && s.text.is_empty() {
                        this.error = Some(t!("Nothing shared could be read.").into());
                        this.share.open = false;
                    }
                    cx.notify();
                    false
                })
                .unwrap_or(true);
            if stale {
                copied.paths.iter().for_each(|p| crate::share::discard(p));
            }
        })
        .detach();
        cx.notify();
    }

    /// Cancels the share: its copies are deleted.
    pub(super) fn close_share(&mut self, cx: &mut Context<Self>) {
        let s = &mut self.share;
        for (path, _) in s.files.drain(..) {
            crate::share::discard(&path);
        }
        (s.open, s.target, s.copying, s.over, s.unreadable, s.fill) = (false, None, 0, 0, 0, false);
        s.text.clear();
        s.order.clear();
        s.epoch += 1;
        cx.notify();
    }

    fn share_pick(&mut self, room: OwnedRoomId, cx: &mut Context<Self>) {
        if self.share.text_only() {
            self.share_into_composer(room, cx);
        } else {
            self.share.target = Some(room);
            cx.notify();
        }
    }

    fn enter_chat(&mut self, room: OwnedRoomId, cx: &mut Context<Self>) {
        self.panel = None;
        self.open_room(room, cx);
    }

    fn share_into_composer(&mut self, room: OwnedRoomId, cx: &mut Context<Self>) {
        let text = std::mem::take(&mut self.share.text);
        self.close_share(cx);
        self.enter_chat(room, cx);
        self.share.to_composer = Some(text);
    }

    fn share_send(&mut self, cx: &mut Context<Self>) {
        let s = &mut self.share;
        let Some(room) = s.target.clone().filter(|_| s.open && s.copying == 0 && !s.files.is_empty()) else { return };
        let files: Vec<PathBuf> = s.files.drain(..).map(|(path, _)| path).collect();
        let body = s.message.read(cx).value().trim().to_owned();
        self.close_share(cx);
        self.enter_chat(room.clone(), cx);
        // Before the files, which wait for their uploads.
        if !body.is_empty() {
            self.send(Cmd::Send { room: room.clone(), body, reply_to: None, edit: None, mentions: Vec::new(), preview: None }, cx);
        }
        self.uploads += files.len();
        self.queue_attach(room, files, None, cx);
    }

    /// Offers the most recent chats in other apps' share sheets, when they changed and the app is
    /// in front (the system limits how often an app in the background may change them).
    pub(super) fn publish_share_targets(&mut self) {
        if !crate::FOREGROUND.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let mut recent: Vec<&RoomSummary> = self.rooms.iter().filter(|r| !r.archived).collect();
        recent.sort_by_key(|r| std::cmp::Reverse(r.ts));
        recent.truncate(crate::share::MAX_TARGETS);
        let ids: Vec<OwnedRoomId> = recent.iter().map(|r| r.id.clone()).collect();
        if ids == self.share.published {
            return;
        }
        let chats: Vec<(String, String, Option<PathBuf>)> = recent
            .iter()
            .map(|r| {
                // The picture a notification of the chat left, when one did; see `notification_icon`.
                let file: String = r.id.as_str().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
                let icon = Some(self.account_dir().join("notify").join(file)).filter(|p| p.is_file());
                (self.chat_key(&r.id), r.name.clone(), icon)
            })
            .collect();
        crate::share::publish(&chats);
        self.share.published = ids;
    }

    pub(super) fn render_share(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if let Some(text) = self.share.to_composer.take() {
            self.composer.update(cx, |c, cx| {
                c.set_value(text, window, cx);
                c.focus(window, cx);
            });
        }
        if !self.share.open {
            return None;
        }
        if std::mem::take(&mut self.share.fill) {
            let (text, query) = (self.share.text.clone(), self.share.query.clone());
            self.share.message.update(cx, |m, cx| m.set_value(text, window, cx));
            query.update(cx, |q, cx| q.set_value("", window, cx));
        }
        let p = Palette::of(cx);
        let query = self.share.query.read(cx).value().to_string();
        self.share.rows = pick_order(&self.rooms, &query, &mut self.share.order);
        let s = &self.share;
        let text_only = s.text_only();

        let head = h_flex()
            .flex_none()
            .h(px(BAR_H))
            .pl(px(GUTTER - 10.))
            .pr(px(GUTTER))
            .gap(px(4.))
            .child(round_button("share-close", IconName::X, cx).on_click(cx.listener(|this, _, _, cx| this.close_share(cx))))
            .child(div().flex_1().min_w_0().child(screen_title(t!("Send to"))));

        let status = if s.copying > 0 {
            Some(match s.copying {
                1 => t!("Preparing the file…").to_owned(),
                n => tn!(n, "Preparing {n} file…", "Preparing {n} files…"),
            })
        } else {
            shortfall(s.over, s.unreadable)
        };
        let items = (!s.files.is_empty()).then(|| {
            h_flex().id("share-items").flex_none().w_full().overflow_x_scroll().px(px(GUTTER)).gap(px(8.)).children(s.files.iter().map(|(path, size)| {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                h_flex()
                    .flex_none()
                    .max_w(px(220.))
                    .h(px(TAP))
                    .pl(px(12.))
                    .pr(px(14.))
                    .gap(px(8.))
                    .rounded_full()
                    .bg(p.surface)
                    .child(Icon::new(file_icon(path)).size_4().flex_none().text_color(p.muted))
                    .child(div().min_w_0().truncate().text_size(fs(14.)).child(name))
                    .child(div().flex_none().text_size(fs(12.)).font_features(tabular()).text_color(p.faint).child(file_size(*size)))
            }))
        });
        let text_card = (text_only && !s.text.is_empty()).then(|| {
            v_flex()
                .flex_none()
                .mx(px(GUTTER))
                .px(px(GAP))
                .py(px(10.))
                .gap(px(4.))
                .rounded(px(RADIUS))
                .bg(p.surface)
                .child(div().text_size(fs(14.)).line_clamp(3).text_ellipsis().child(s.text.clone()))
                .child(div().text_size(fs(12.)).text_color(p.faint).child(t!("Goes into the message box of the chat you choose.")))
        });

        let search = h_flex()
            .flex_none()
            .mx(px(GUTTER))
            .h(px(TAP))
            .pl(px(GAP))
            .pr(px(GAP))
            .gap(px(8.))
            .rounded_full()
            .bg(p.surface)
            .child(Icon::new(IconName::Search).size_4().flex_none().text_color(p.faint))
            .child(div().flex_1().min_w_0().child(Input::new(&s.query).appearance(false)).child(crate::ime::marks(&s.query)));

        let empty = if self.rooms.is_empty() { t!("Syncing chats…") } else { t!("No chat matches") };
        let list = uniform_list(
            "share-chats",
            s.rows.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                this.want_share_avatars(range.clone(), cx);
                range
                    .map(|ix| {
                        let room = &this.rooms[this.share.rows[ix]];
                        let chosen = this.share.target.as_ref() == Some(&room.id);
                        let id = room.id.clone();
                        h_flex()
                            .id(SharedString::from(format!("share-{}", room.id)))
                            .w_full()
                            .h(px(ROW_H))
                            .px(px(GUTTER))
                            .gap(px(GAP))
                            .cursor_pointer()
                            .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(if chosen { p.raised } else { p.surface })))
                            .when(chosen, |el| el.bg(p.raised))
                            .active(|el| el.bg(p.raised))
                            .on_click(cx.listener(move |this, _, _, cx| this.share_pick(id.clone(), cx)))
                            .child(room_avatar(room, &this.avatars, ROW_AVATAR, cx))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().truncate().text_size(fs(16.)).line_height(fs(22.)).child(room.name.clone()))
                                    .children(room.network.as_deref().map(|n| {
                                        div().truncate().text_size(fs(12.)).text_color(p.faint).child(short_network(n).to_owned())
                                    })),
                            )
                            .when(chosen, |el| el.child(Icon::new(IconName::Check).size_5().flex_none().text_color(p.text)))
                    })
                    .collect()
            }),
        )
        .size_full();

        let ready = s.target.is_some() && s.copying == 0 && !s.files.is_empty();
        let bottom = (!text_only).then(|| {
            h_flex()
                .flex_none()
                .items_end()
                .gap(px(6.))
                .px(px(GUTTER))
                .py(px(8.))
                .border_t_1()
                .border_color(p.rule)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .min_h(px(TAP))
                        .px(px(6.))
                        .flex()
                        .items_center()
                        .rounded(px(TAP / 2.))
                        .overflow_hidden()
                        .bg(p.surface)
                        .child(Textarea::new(&s.message).appearance(false).text_size(fs(16.)))
                        .child(crate::ime::marks(&s.message)),
                )
                .child(
                    div()
                        .id("share-send")
                        .size(px(TAP))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(p.text)
                        .text_color(p.ground)
                        .map(|el| {
                            if ready {
                                el.cursor_pointer()
                                    .when(!gpui_kit::is_mobile(), |el| el.hover(|el| el.bg(p.muted)))
                                    .active(|el| el.bg(p.muted))
                                    .on_click(cx.listener(|this, _, _, cx| this.share_send(cx)))
                            } else {
                                el.opacity(0.4)
                            }
                        })
                        .child(Icon::new(IconName::ArrowUp).size_5()),
                )
        });

        let column = v_flex()
            .size_full()
            .max_w(px(COLUMN_W))
            .child(head)
            .children(items.map(|el| el.pb(px(8.))))
            .children(status.map(|text| div().flex_none().px(px(GUTTER)).pb(px(8.)).text_size(fs(12.)).text_color(p.faint).child(text)))
            .children(text_card.map(|el| el.mb(px(GAP))))
            .child(search.mb(px(8.)))
            .child(div().flex_1().min_h_0().child(if s.rows.is_empty() {
                div().px(px(GUTTER)).py(px(GAP)).text_size(fs(14.)).text_color(p.faint).child(empty).into_any_element()
            } else {
                list.into_any_element()
            }))
            .children(bottom);
        Some(h_flex().size_full().justify_center().child(column).into_any_element())
    }

    /// Asks once for the pictures of the chats the picker is drawing, leaving half the command
    /// channel to user actions as `fetch_rows` does.
    fn want_share_avatars(&mut self, range: std::ops::Range<usize>, cx: &mut App) {
        if self.cmd.capacity() < self.cmd.max_capacity() / 2 {
            return;
        }
        let wanted: Vec<OwnedRoomId> =
            self.share.rows[range].iter().map(|&ix| &self.rooms[ix].id).filter(|id| !self.avatars.contains_key(*id)).cloned().collect();
        if !wanted.is_empty() && self.cmd.try_send(Cmd::Avatars(wanted.clone())).is_ok() {
            self.asked_avatars(wanted, cx);
        }
    }
}

fn file_icon(path: &Path) -> IconName {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" | "avif" | "bmp" => IconName::Image,
        "mp4" | "mov" | "webm" | "mkv" | "3gp" | "m4v" => IconName::Video,
        "mp3" | "m4a" | "ogg" | "opus" | "wav" | "flac" | "aac" => IconName::Music,
        _ => IconName::File,
    }
}

#[cfg(test)]
mod tests {
    use super::{pick_order, shortfall};
    use crate::backend::RoomSummary;

    #[test]
    fn picker_lists_recent_first_and_filters_by_name() {
        let room = |id: &str, name: &str, ts: u64, pinned: bool| {
            let mut r: RoomSummary = serde_json::from_value(serde_json::json!({ "id": id, "name": name, "preview": null, "ts": ts })).unwrap();
            r.pinned = pinned;
            r
        };
        let rooms = [room("!a:x", "Anna", 10, true), room("!b:x", "Bob", 30, false), room("!c:x", "anneliese", 20, false)];
        let mut order = Vec::new();
        assert_eq!(pick_order(&rooms, "", &mut order), [1, 2, 0]);
        assert_eq!(pick_order(&rooms, " ANN ", &mut order), [2, 0]);
        // A new message keeps the rows in place; a new chat goes below; a gone one leaves.
        let mut later = vec![rooms[1].clone(), room("!c:x", "anneliese", 40, false), room("!d:x", "Dora", 50, false)];
        later[0].ts = 5;
        assert_eq!(pick_order(&later, "", &mut order), [0, 1, 2]);
    }

    #[test]
    fn says_what_was_left_out() {
        assert_eq!(shortfall(0, 0), None);
        assert_eq!(shortfall(1, 0).as_deref(), Some("1 file was left out: one share takes at most 20 files and 200 MB, each file at most 100 MB."));
        assert_eq!(shortfall(2, 1).as_deref(), Some("2 files were left out: one share takes at most 20 files and 200 MB, each file at most 100 MB. 1 file could not be read."));
    }
}
