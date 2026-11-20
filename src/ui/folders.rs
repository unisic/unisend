//! Chat folders as Signal has them: chips over the chat list. The built-in ones (All, Unread,
//! Direct, Groups, one per chat network, Low priority) are computed from the chats; the custom
//! ones are a name and a set of chats the user picks. Custom folders, the hidden built-ins and the
//! chosen chip live in `settings.json` on this device and nowhere else.

use super::panels::{Page, ROW_H, card, footnote, link, row, section};
use super::disappear::timer_row;
use super::*;
use crate::i18n::{t, tn};
use gpui_kit::component::switch::Switch;
use serde::{Deserialize, Serialize};

/// Custom folders kept. The chip row scrolls sideways and a longer one stops being a quick way to
/// a chat; the file stays small.
pub(crate) const MAX_FOLDERS: usize = 20;

/// Chats in one custom folder. Above what the biggest account holds (about 500 chats) so a folder
/// can take everything, yet `settings.json` is rewritten on each change and stays bounded.
pub(crate) const MAX_FOLDER_CHATS: usize = 600;

/// Characters of a folder's name; a chip wider than this is more than a phone's row can show.
pub(crate) const NAME_MAX: usize = 24;

/// How wide a chip cut by the row's edge fades into the list.
const EDGE_FADE: f32 = 24.;

/// What the list is narrowed to. Archive is a tab of its own and not one of these.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Pick {
    #[default]
    All,
    Unread,
    Direct,
    Groups,
    /// A chat network, by the name the chat row shows.
    Network(String),
    /// A custom folder, by its `id`.
    Custom(u32),
    LowPriority,
}

/// A custom folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Folder {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub rooms: Vec<OwnedRoomId>,
}

/// A built-in chip the user can hide in Settings. All stays: it is the way back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Builtin {
    Unread,
    Direct,
    Groups,
    /// The chips of the chat networks, as one switch.
    Networks,
    LowPriority,
}

impl Builtin {
    const ALL: [Self; 5] = [Self::Unread, Self::Direct, Self::Groups, Self::Networks, Self::LowPriority];

    fn label(self) -> &'static str {
        match self {
            Self::Unread => t!("Unread"),
            Self::Direct => t!("Direct"),
            Self::Groups => t!("Groups"),
            Self::Networks => t!("Networks"),
            Self::LowPriority => t!("Low priority"),
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Unread => "folder-show-unread",
            Self::Direct => "folder-show-direct",
            Self::Groups => "folder-show-groups",
            Self::Networks => "folder-show-networks",
            Self::LowPriority => "folder-show-low",
        }
    }
}

/// The network a chat is shown under; native Matrix chats have none and are "Matrix".
fn network_of(r: &RoomSummary) -> &str {
    r.network.as_deref().map_or("Matrix", short_network)
}

/// The networks of the chats the list shows, sorted.
fn networks(rooms: &[RoomSummary]) -> Vec<String> {
    let mut names: Vec<String> = rooms.iter().filter(|r| in_view(View::Inbox, r) && !replaced(rooms, r)).map(|r| network_of(r).to_owned()).collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Whether a chat is in what `pick` narrows the list to; the section (inbox, low priority) and the
/// search are the caller's.
fn matches(pick: &Pick, folders: &[Folder], r: &RoomSummary) -> bool {
    match pick {
        Pick::All | Pick::LowPriority => true,
        Pick::Unread => r.is_unread(),
        Pick::Direct => r.dm,
        Pick::Groups => !r.dm,
        Pick::Network(name) => network_of(r) == name,
        Pick::Custom(id) => folders.iter().find(|f| f.id == *id).is_some_and(|f| f.rooms.contains(&r.id)),
    }
}

/// The chips, in order: All, Unread, Direct, Groups, one per network (when there are two or more),
/// the custom folders, and Low priority last: it is where chats are put away, so it is the one
/// the user reaches for least and the row's end is where it costs the others least.
fn chips(settings: &crate::Settings, rooms: &[RoomSummary]) -> Vec<Pick> {
    let shown = |b: Builtin| !settings.folders_hidden.contains(&b);
    let mut out = vec![Pick::All];
    out.extend(shown(Builtin::Unread).then_some(Pick::Unread));
    out.extend(shown(Builtin::Direct).then_some(Pick::Direct));
    out.extend(shown(Builtin::Groups).then_some(Pick::Groups));
    if shown(Builtin::Networks) {
        let names = networks(rooms);
        if names.len() >= 2 {
            out.extend(names.into_iter().map(Pick::Network));
        }
    }
    out.extend(settings.folders.iter().map(|f| Pick::Custom(f.id)));
    out.extend(shown(Builtin::LowPriority).then_some(Pick::LowPriority));
    out
}

/// The chip in force: the stored one unless its folder is gone, hidden or (once the chats are
/// known) has no chats left to be a chip. Not written back, so a chat list still loading does not
/// lose the choice.
fn effective(pick: &Pick, settings: &crate::Settings, rooms: &[RoomSummary]) -> Pick {
    let known = match pick {
        Pick::Network(_) => rooms.is_empty() && !settings.folders_hidden.contains(&Builtin::Networks),
        _ => false,
    };
    if known || chips(settings, rooms).contains(pick) { pick.clone() } else { Pick::All }
}

fn label(pick: &Pick, folders: &[Folder]) -> SharedString {
    match pick {
        Pick::All => t!("All").into(),
        Pick::Unread => t!("Unread").into(),
        Pick::Direct => t!("Direct").into(),
        Pick::Groups => t!("Groups").into(),
        Pick::Network(name) => name.clone().into(),
        Pick::Custom(id) => folders.iter().find(|f| f.id == *id).map(|f| f.name.clone()).unwrap_or_default().into(),
        Pick::LowPriority => t!("Low priority").into(),
    }
}

fn clean(name: &str) -> Option<String> {
    let name: String = name.trim().chars().take(NAME_MAX).collect();
    let name = name.trim_end().to_owned();
    (!name.is_empty()).then_some(name)
}

/// Adds a folder; `None` when the name is empty or the cap is reached.
fn add(folders: &mut Vec<Folder>, name: &str) -> Option<u32> {
    let name = clean(name).filter(|_| folders.len() < MAX_FOLDERS)?;
    let id = folders.iter().map(|f| f.id).max().map_or(1, |max| max.saturating_add(1));
    folders.push(Folder { id, name, rooms: Vec::new() });
    Some(id)
}

fn rename(folders: &mut [Folder], id: u32, name: &str) -> bool {
    let (Some(name), Some(f)) = (clean(name), folders.iter_mut().find(|f| f.id == id)) else { return false };
    f.name = name;
    true
}

/// Moves a folder one place up or down; false at the end.
fn shift(folders: &mut [Folder], id: u32, down: bool) -> bool {
    let Some(at) = folders.iter().position(|f| f.id == id) else { return false };
    let to = if down { at + 1 } else { at.wrapping_sub(1) };
    if to >= folders.len() {
        return false;
    }
    folders.swap(at, to);
    true
}

/// Puts the chat in the folder, or takes it out when it is in. Adding to a full folder first lets
/// go of the chats that are gone (left or deleted), then refuses if it is still full.
fn toggle(folders: &mut [Folder], id: u32, room: &OwnedRoomId, exists: impl Fn(&OwnedRoomId) -> bool) -> bool {
    let Some(f) = folders.iter_mut().find(|f| f.id == id) else { return false };
    if let Some(at) = f.rooms.iter().position(|r| r == room) {
        f.rooms.remove(at);
        return true;
    }
    if f.rooms.len() >= MAX_FOLDER_CHATS {
        f.rooms.retain(|r| exists(r));
    }
    if f.rooms.len() >= MAX_FOLDER_CHATS {
        return false;
    }
    f.rooms.push(room.clone());
    true
}

pub(super) struct Ui {
    /// The chip in force, as stored; `effective` is what is shown.
    pub(super) pick: Pick,
    /// The name field of Settings > Chat folders, shared by new and rename.
    field: Entity<InputState>,
    /// What the field is for: `Some(None)` a new folder, `Some(Some(id))` a rename.
    edit: Option<Option<u32>>,
    /// The chat whose sheet started the new folder, put into it once it is added.
    with: Option<OwnedRoomId>,
    /// The chat sheet shows the folders to put the chat in.
    pub(super) picking: bool,
    /// The chosen chip is to be brought into view once the row has been measured: gliding after a
    /// tap (`Some(true)`), at once after a restart.
    reveal: Option<bool>,
    /// The row gliding to bring the chosen chip in: since when, from and to which offset.
    glide: Option<(Instant, Pixels, Pixels)>,
    scroll: ScrollHandle,
}

impl Ui {
    pub(super) fn new(pick: Pick, window: &mut Window, cx: &mut Context<BeeperApp>, subs: &mut Vec<Subscription>) -> Self {
        let field = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Folder name")));
        crate::ime::follow(&field, window, cx);
        subs.push(cx.subscribe_in(&field, window, |this, _, ev: &InputEvent, _, cx| match ev {
            InputEvent::PressEnter { shift: false, .. } => this.save_folder(cx),
            other => keyboard_follows(other, Keyboard::Text, cx),
        }));
        Self { pick, field, edit: None, with: None, picking: false, reveal: Some(false), glide: None, scroll: ScrollHandle::new() }
    }
}

impl BeeperApp {
    pub(super) fn effective_pick(&self) -> Pick {
        effective(&self.folders.pick, &self.settings, &self.rooms)
    }

    /// Whether `r` is in the chip in force, for `refilter`; the archive ignores the chips.
    pub(super) fn in_folder(&self, pick: &Pick, r: &RoomSummary) -> bool {
        matches(pick, &self.settings.folders, r)
    }

    pub(super) fn set_pick(&mut self, pick: Pick, cx: &mut Context<Self>) {
        self.folders.pick = pick.clone();
        // A chip only half in view is brought fully in.
        self.folders.reveal = Some(true);
        if self.settings.folder != pick {
            self.settings.folder = pick;
            self.settings.save(&self.dir);
        }
        self.refilter(cx);
    }

    fn save_settings(&mut self, cx: &mut Context<Self>) {
        self.settings.save(&self.dir);
        // A folder the chip points at may be gone, and the chats it holds may have changed.
        self.refilter(cx);
    }

    /// The chip row over the list. It scrolls sideways when it does not fit, apart from the list.
    pub(super) fn render_chips(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let p = Palette::of(cx);
        let current = self.effective_pick();
        // A forum is one row in the list, so one in the count however many of its topics wait.
        let mut forums = HashSet::new();
        let waiting = self.rooms.iter().filter(|r| super::accounts::waiting(r) && !replaced(&self.rooms, r)).filter(|r| r.forum.as_ref().is_none_or(|f| forums.insert(f))).count();
        let picks = chips(&self.settings, &self.rooms);
        let seg = press::Segments::new("filters", window, cx);
        let chosen = picks.iter().position(|pick| *pick == current);
        let slider = chosen.and_then(|ix| seg.pill("filters-pill", ix, p.raised, window, cx));
        let measured = slider.is_some();
        if let Some(glide) = self.folders.reveal
            && let Some(at) = chosen.and_then(|ix| seg.item(ix))
        {
            let view = self.folders.scroll.bounds();
            if view.size.width > px(0.) {
                let (offset, most) = (self.folders.scroll.offset(), self.folders.scroll.max_offset());
                let past = at.right() - view.right();
                let before = view.left() - at.left();
                let x = if past > px(0.) { offset.x - past - px(GUTTER) } else if before > px(0.) { offset.x + before + px(GUTTER) } else { offset.x };
                let x = x.clamp(-most.x, px(0.));
                // A tap glides, so the row does not jump under the pointer.
                if glide && x != offset.x && !cx.reduce_motion() {
                    self.folders.glide = Some((Instant::now(), offset.x, x));
                } else {
                    self.folders.scroll.set_offset(point(x, offset.y));
                }
                self.folders.reveal = None;
                window.request_animation_frame();
            }
        }
        if let Some((at, from, to)) = self.folders.glide {
            let t = (at.elapsed().as_secs_f32() / motion::ENTER.as_secs_f32()).min(1.);
            let x = from + (to - from) * motion::ease_out(t);
            self.folders.scroll.set_offset(point(x, self.folders.scroll.offset().y));
            if t < 1. {
                window.request_animation_frame();
            } else {
                self.folders.glide = None;
            }
        }
        // A chip cut by an edge fades into the list there: the cut reads as more chips that way.
        let (offset, most) = (self.folders.scroll.offset().x, self.folders.scroll.max_offset().x);
        let fade = |left: bool| {
            let stops = (linear_color_stop(p.ground, 0.), linear_color_stop(p.ground.opacity(0.), 1.));
            div()
                .absolute()
                .top_0()
                .bottom(px(4.))
                .w(px(EDGE_FADE))
                .map(|el| if left { el.left_0() } else { el.right_0() })
                .bg(linear_gradient(if left { 90. } else { 270. }, stops.0, stops.1))
        };
        let chips = picks.into_iter().enumerate().map(|(ix, pick)| {
            let on = chosen == Some(ix);
            let name = label(&pick, &self.settings.folders);
            let count = if pick == Pick::Unread { waiting } else { 0 };
            let press = (!on).then(|| press::area(("folder-chip-press", ix), Duration::ZERO, p.surface, window, cx).rounded_full());
            // The pill is slimmer than what a finger needs; the target around it is not.
            h_flex().id(("folder-chip", ix)).flex_none().h(px(TAP)).cursor_pointer().on_click(cx.listener(move |this, _, _, cx| this.set_pick(pick.clone(), cx))).child(
                h_flex()
                    .id("pill")
                    .relative()
                    .h(px(36.))
                    .px(px(if gpui_kit::is_mobile() { GAP } else { 10. }))
                    .gap(px(6.))
                    .rounded_full()
                    .text_size(fs(14.))
                    .text_color(if on { p.text } else { p.muted })
                    .when(on && !measured, |el| el.bg(p.raised))
                    .child(seg.probe(ix))
                    .children(press)
                    .child(name)
                    .when(count > 0, |el| el.child(div().text_color(p.faint).font_features(tabular()).child(count.to_string()))),
            )
        });
        let row = h_flex()
            .id("filter-row")
            .relative()
            .px(px(GUTTER))
            .pb(px(4.))
            .gap(px(4.))
            .overflow_x_scroll()
            .track_scroll(&self.folders.scroll)
            .child(seg.container())
            .children(slider)
            .children(chips);
        // The fades sit outside the scrolled row, which would carry them along.
        div()
            .id("filter-chips")
            .flex_none()
            .relative()
            .child(row)
            .when(offset < px(-0.5), |el| el.child(fade(true)))
            .when(offset > -most + px(0.5), |el| el.child(fade(false)))
    }

    /// The chip in force is a custom folder, whose empty list says how to fill it.
    pub(super) fn folder_is_custom(&self) -> bool {
        matches!(self.effective_pick(), Pick::Custom(_))
    }

    fn set_shown(&mut self, b: Builtin, on: bool, cx: &mut Context<Self>) {
        self.settings.folders_hidden.retain(|h| *h != b);
        if !on {
            self.settings.folders_hidden.push(b);
        }
        self.save_settings(cx);
    }

    fn start_edit(&mut self, target: Option<u32>, window: &mut Window, cx: &mut Context<Self>) {
        let name = target.and_then(|id| self.settings.folders.iter().find(|f| f.id == id)).map(|f| f.name.clone()).unwrap_or_default();
        self.folders.field.update(cx, |input, cx| {
            input.set_value(name, window, cx);
            input.focus(window, cx);
        });
        (self.folders.edit, self.folders.with) = (Some(target), None);
        cx.notify();
    }

    fn save_folder(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.folders.edit else { return };
        let name = self.folders.field.read(cx).value().to_string();
        let done = match target {
            None => add(&mut self.settings.folders, &name)
                .inspect(|&id| {
                    if let Some(room) = self.folders.with.take() {
                        let rooms = &self.rooms;
                        toggle(&mut self.settings.folders, id, &room, |r| rooms.iter().any(|s| &s.id == r));
                    }
                })
                .is_some(),
            Some(id) => rename(&mut self.settings.folders, id, &name),
        };
        if done {
            self.folders.edit = None;
            self.save_settings(cx);
        }
        cx.notify();
    }

    fn delete_folder(&mut self, id: u32, cx: &mut Context<Self>) {
        self.settings.folders.retain(|f| f.id != id);
        if self.folders.edit == Some(Some(id)) {
            self.folders.edit = None;
        }
        self.save_settings(cx);
    }

    fn move_folder(&mut self, id: u32, down: bool, cx: &mut Context<Self>) {
        if shift(&mut self.settings.folders, id, down) {
            self.save_settings(cx);
        }
    }

    pub(super) fn open_folders_page(&mut self, cx: &mut Context<Self>) {
        self.folders.edit = None;
        self.open_page(Page::Folders, cx);
    }

    /// Settings > Chats > Chat folders.
    pub(super) fn render_folders(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let hidden = self.settings.folders_hidden.clone();
        let built_in = card(cx).children(Builtin::ALL.map(|b| {
            let weak = cx.entity().downgrade();
            let switch = Switch::new(b.id()).checked(!hidden.contains(&b)).on_click(move |on, _, cx| {
                let _ = weak.update(cx, |this, cx| this.set_shown(b, *on, cx));
            });
            row(b.label(), "", cx).child(switch)
        }));
        let count = self.settings.folders.len();
        let tool = |id: (&'static str, u32), icon: IconName, on: bool, cx: &App| {
            let el = round_button(id, icon, cx);
            if on { el } else { el.opacity(0.4).cursor_default() }
        };
        let add = (self.folders.edit.is_none() && count < MAX_FOLDERS).then(|| {
            link("folder-new", t!("New folder"), SharedString::default(), Some(IconName::Plus), None, cx).on_click(cx.listener(|this, _, window, cx| this.start_edit(None, window, cx)))
        });
        let listed = count > 0 || add.is_some();
        let mine = card(cx).children(self.settings.folders.iter().enumerate().map(|(ix, f)| {
            let id = f.id;
            let detail = tn!(f.rooms.len(), "{n} chat", "{n} chats");
            h_flex()
                .min_h(px(ROW_H))
                .child(v_flex().flex_1().min_w_0().py(px(8.)).child(div().text_size(fs(16.)).line_height(fs(22.)).truncate().child(f.name.clone())).child(div().text_size(fs(13.)).line_height(fs(18.)).text_color(p.muted).child(detail)))
                .child(tool(("folder-up", id), IconName::ArrowUp, ix > 0, cx).when(ix > 0, |el| el.on_click(cx.listener(move |this, _, _, cx| this.move_folder(id, false, cx)))))
                .child(tool(("folder-down", id), IconName::ArrowDown, ix + 1 < count, cx).when(ix + 1 < count, |el| el.on_click(cx.listener(move |this, _, _, cx| this.move_folder(id, true, cx)))))
                .child(tool(("folder-rename", id), IconName::Pencil, true, cx).on_click(cx.listener(move |this, _, window, cx| this.start_edit(Some(id), window, cx))))
                .child(tool(("folder-delete", id), IconName::Trash, true, cx).on_click(cx.listener(move |this, _, _, cx| this.delete_folder(id, cx))))
        })).children(add);
        let editor = self.folders.edit.map(|target| {
            card(cx)
                .py(px(8.))
                .gap(px(8.))
                .child(div().text_size(fs(12.)).text_color(p.faint).child(t!("Name")))
                .child(Input::new(&self.folders.field))
                .child(crate::ime::marks(&self.folders.field))
                .child(
                    h_flex()
                        .pb(px(4.))
                        .gap(px(8.))
                        .child(Button::new("folder-save").primary().label(if target.is_some() { t!("Save") } else { t!("Add") }).on_click(cx.listener(|this, _, _, cx| this.save_folder(cx))))
                        .child(Button::new("folder-cancel").label(t!("Cancel")).on_click(cx.listener(|this, _, _, cx| {
                            this.folders.edit = None;
                            cx.notify();
                        }))),
                )
        });
        let note = if count > 0 {
            t!("Folders are kept on this device only.").to_owned()
        } else if gpui_kit::is_mobile() {
            t!("Hold a chat and choose \"{action}\".", action = t!("Add to folder"))
        } else {
            t!("Right-click a chat and choose \"{action}\".", action = t!("Add to folder"))
        };
        v_flex()
            .gap(px(GAP * 2.))
            .child(v_flex().gap(px(8.)).child(section(t!("Shown in the list"), cx).child(built_in)).child(footnote(t!("Hiding a folder only hides its chip. The chats stay where they are."), cx)))
            .child(
                v_flex()
                    .gap(px(8.))
                    .child(section(t!("Your folders"), cx).when(listed, |el| el.child(mine)))
                    .children(editor)
                    .child(footnote(&note, cx)),
            )
            .into_any_element()
    }

    /// The summary on the Chats page's row.
    pub(super) fn folders_summary(&self) -> String {
        match self.settings.folders.len() {
            0 => String::new(),
            n => tn!(n, "{n} folder", "{n} folders"),
        }
    }

    /// The chat sheet's row that opens the folders to put the chat in.
    pub(super) fn folder_entry(&self, live: bool, window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let row = sheet_row("add-to-folder", IconName::Folder, t!("Add to folder"), Palette::of(cx).text, window, cx);
        if !live {
            return row;
        }
        row.on_click(cx.listener(|this, _, _, cx| {
            this.folders.picking = true;
            cx.notify();
        }))
    }

    /// The sheet of folders: a tap puts the chat in one or takes it out; New folder goes to Settings.
    pub(super) fn folder_sheet(&self, room: &OwnedRoomId, live: bool, cx: &mut Context<Self>) -> (String, Vec<Stateful<Div>>) {
        let mut rows: Vec<_> = self
            .settings
            .folders
            .iter()
            .map(|f| {
                let (id, inside) = (f.id, f.rooms.contains(room));
                let row = timer_row(("folder-pick", id), inside.then_some(IconName::Check), f.name.clone(), cx);
                if !live {
                    return row;
                }
                let room = room.clone();
                row.on_click(cx.listener(move |this, _, _, cx| {
                    let rooms = &this.rooms;
                    if toggle(&mut this.settings.folders, id, &room, |r| rooms.iter().any(|s| &s.id == r)) {
                        this.save_settings(cx);
                    }
                    cx.notify();
                }))
            })
            .collect();
        if self.settings.folders.len() < MAX_FOLDERS {
            let row = timer_row("folder-pick-new", Some(IconName::Plus), t!("New folder"), cx);
            let room = room.clone();
            rows.push(if live {
                row.on_click(cx.listener(move |this, _, window, cx| {
                    (this.chat_sheet, this.folders.picking) = (None, false);
                    this.set_tab(Tab::Settings, window, cx);
                    this.open_page(Page::Chats, cx);
                    this.open_folders_page(cx);
                    this.start_edit(None, window, cx);
                    this.folders.with = Some(room.clone());
                }))
            } else {
                row
            });
        }
        (t!("Add to folder").to_owned(), rows)
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` attribute would shadow the standard one.
    use super::{Builtin, Folder, MAX_FOLDERS, MAX_FOLDER_CHATS, NAME_MAX, OwnedRoomId, Pick, RoomSummary, View, add, chips, effective, in_view, matches, rename, shift, toggle};
    use crate::Settings;

    fn room(id: &str, network: Option<&str>, dm: bool) -> RoomSummary {
        RoomSummary {
            id: id.try_into().unwrap(),
            name: String::new(),
            preview: None,
            preview_id: None,
            ts: 0,
            network: network.map(str::to_owned),
            parent: None,
            forum: None,
            unread: 0,
            mentions: 0,
            dm,
            note: false,
            pinned: false,
            low_priority: false,
            muted: false,
            marked_unread: false,
            archived: false,
            archive_mark: false,
            order: 0,
            dated: true,
            pending: None,
            moved_to: None,
            own_last: false,
            read_by: Vec::new(),
        }
    }

    fn ids(rooms: &[RoomSummary], pick: &Pick, folders: &[Folder]) -> Vec<String> {
        rooms.iter().filter(|r| in_view(View::Inbox, r) && matches(pick, folders, r)).map(|r| r.id.to_string()).collect()
    }

    fn sample() -> Vec<RoomSummary> {
        let mut unread = room("!c:x", Some("WhatsApp"), false);
        unread.unread = 2;
        let mut low = room("!e:x", Some("Signal"), true);
        low.low_priority = true;
        let mut gone = room("!f:x", Some("Signal"), true);
        gone.archived = true;
        vec![room("!a:x", Some("WhatsApp"), true), room("!b:x", None, true), unread, room("!d:x", Some("Facebook Messenger"), false), low, gone]
    }

    #[test]
    fn built_in_folders_show_their_chats_from_the_inbox_only() {
        let rooms = sample();
        let none: &[Folder] = &[];
        assert_eq!(ids(&rooms, &Pick::All, none), ["!a:x", "!b:x", "!c:x", "!d:x"]);
        assert_eq!(ids(&rooms, &Pick::Unread, none), ["!c:x"]);
        assert_eq!(ids(&rooms, &Pick::Direct, none), ["!a:x", "!b:x"]);
        assert_eq!(ids(&rooms, &Pick::Groups, none), ["!c:x", "!d:x"]);
        assert_eq!(ids(&rooms, &Pick::Network("WhatsApp".into()), none), ["!a:x", "!c:x"]);
        assert_eq!(ids(&rooms, &Pick::Network("Messenger".into()), none), ["!d:x"]);
        assert_eq!(ids(&rooms, &Pick::Network("Matrix".into()), none), ["!b:x"]);
    }

    #[test]
    fn a_custom_folder_shows_its_chats_and_an_unknown_one_none() {
        let rooms = sample();
        let folders = [Folder { id: 3, name: "Work".into(), rooms: vec!["!b:x".try_into().unwrap(), "!d:x".try_into().unwrap(), "!e:x".try_into().unwrap()] }];
        assert_eq!(ids(&rooms, &Pick::Custom(3), &folders), ["!b:x", "!d:x"]);
        assert!(ids(&rooms, &Pick::Custom(9), &folders).is_empty());
    }

    #[test]
    fn network_chips_need_two_networks_and_hiding_removes_them() {
        let mut settings = Settings::parse(b"{}").unwrap();
        let rooms = sample();
        assert_eq!(chips(&settings, &rooms[..1]), [Pick::All, Pick::Unread, Pick::Direct, Pick::Groups, Pick::LowPriority]);
        assert_eq!(
            chips(&settings, &rooms),
            [Pick::All, Pick::Unread, Pick::Direct, Pick::Groups, Pick::Network("Matrix".into()), Pick::Network("Messenger".into()), Pick::Network("WhatsApp".into()), Pick::LowPriority]
        );
        settings.folders_hidden = vec![Builtin::Networks, Builtin::Direct, Builtin::LowPriority];
        settings.folders = vec![Folder { id: 1, name: "A".into(), rooms: vec![] }];
        assert_eq!(chips(&settings, &rooms), [Pick::All, Pick::Unread, Pick::Groups, Pick::Custom(1)]);
    }

    #[test]
    fn a_pick_that_no_longer_has_a_chip_falls_back_to_all() {
        let mut settings = Settings::parse(b"{}").unwrap();
        let rooms = sample();
        let whatsapp = Pick::Network("WhatsApp".into());
        assert_eq!(effective(&whatsapp, &settings, &rooms), whatsapp);
        assert_eq!(effective(&whatsapp, &settings, &rooms[..1]), Pick::All);
        assert_eq!(effective(&whatsapp, &settings, &[]), whatsapp, "chats not loaded yet");
        assert_eq!(effective(&Pick::Custom(4), &settings, &rooms), Pick::All);
        settings.folders_hidden = vec![Builtin::Groups];
        assert_eq!(effective(&Pick::Groups, &settings, &rooms), Pick::All);
    }

    #[test]
    fn folders_are_capped_named_and_ordered() {
        let mut folders = Vec::new();
        assert_eq!(add(&mut folders, "  "), None);
        let long = "x".repeat(NAME_MAX + 10);
        assert_eq!(add(&mut folders, &long), Some(1));
        assert_eq!(folders[0].name.chars().count(), NAME_MAX);
        for n in 1..MAX_FOLDERS {
            assert_eq!(add(&mut folders, &format!("f{n}")), Some(n as u32 + 1));
        }
        assert_eq!(add(&mut folders, "one more"), None);
        assert_eq!(folders.len(), MAX_FOLDERS);
        folders.retain(|f| f.id != 2);
        assert_eq!(add(&mut folders, "again"), Some(MAX_FOLDERS as u32 + 1), "ids are not reused");
        assert!(rename(&mut folders, 1, " Work "));
        assert_eq!(folders[0].name, "Work");
        assert!(!rename(&mut folders, 1, ""));
        assert!(!shift(&mut folders, 1, false));
        assert!(shift(&mut folders, 1, true));
        assert_eq!((folders[0].id, folders[1].id), (3, 1));
        assert!(!shift(&mut folders, 99, true));
    }

    #[test]
    fn a_full_folder_lets_go_of_gone_chats_before_it_refuses() {
        let id = |n: usize| OwnedRoomId::try_from(format!("!r{n}:x")).unwrap();
        let mut folders = vec![Folder { id: 1, name: "All".into(), rooms: (0..MAX_FOLDER_CHATS).map(id).collect() }];
        assert!(!toggle(&mut folders, 1, &id(MAX_FOLDER_CHATS), |_| true));
        assert_eq!(folders[0].rooms.len(), MAX_FOLDER_CHATS);
        assert!(toggle(&mut folders, 1, &id(MAX_FOLDER_CHATS), |r| *r != id(0)));
        assert_eq!(folders[0].rooms.len(), MAX_FOLDER_CHATS);
        assert!(!folders[0].rooms.contains(&id(0)));
        assert!(toggle(&mut folders, 1, &id(MAX_FOLDER_CHATS), |_| true), "a second toggle takes it out");
        assert!(!folders[0].rooms.contains(&id(MAX_FOLDER_CHATS)));
    }

    #[test]
    fn old_settings_files_have_no_folders_and_new_ones_round_trip() {
        let old = Settings::parse(br#"{"theme":"Light"}"#).unwrap();
        assert_eq!((old.folder.clone(), old.folders.len(), old.folders_hidden.len()), (Pick::All, 0, 0));
        let mut changed = old.clone();
        changed.folder = Pick::Network("WhatsApp".into());
        changed.folders = vec![Folder { id: 2, name: "Family".into(), rooms: vec!["!a:x".try_into().unwrap()] }];
        changed.folders_hidden = vec![Builtin::Direct];
        assert_eq!(Settings::parse(&serde_json::to_vec(&changed).unwrap()).unwrap(), changed);
        assert!(!serde_json::to_string(&old).unwrap().contains("folders"), "empty lists stay out of the file");
    }
}
