//! Acting on a group from its info screen: a member's sheet (message, mention, remove), editing
//! the group's name, picture and description, and adding people. What may be done comes from
//! `backend::Can`; the backend says when the server took a change.

use super::*;
use crate::backend::group::Change;
use crate::backend::rooms::{Ask, Target};
use crate::backend::Member;
use gpui_kit::component::input::InlineToken;
use crate::i18n::t;

/// People listed on the adding page at once; typing narrows the rest down.
const SHOWN: usize = 50;
const FACE: f32 = 56.;

pub(super) enum Page {
    Edit { name: Entity<InputState>, topic: Entity<InputState>, _subs: [Subscription; 2] },
    Add { query: Entity<InputState>, people: Option<Vec<(OwnedUserId, String)>>, added: Vec<OwnedUserId>, _sub: Subscription },
}

/// The member whose sheet is up: who, how their network knows them, and whether the sheet asks to
/// confirm removing them.
pub(super) struct Picked {
    member: Member,
    ids: Vec<String>,
    removing: bool,
}

fn text_field(placeholder: &'static str, window: &mut Window, cx: &mut Context<BeeperApp>) -> (Entity<InputState>, Subscription) {
    let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
    crate::ime::follow(&input, window, cx);
    let sub = cx.subscribe_in(&input, window, |_, _, ev: &InputEvent, _, cx| keyboard_follows(ev, Keyboard::Text, cx));
    (input, sub)
}

impl BeeperApp {
    fn info_room(&self) -> Option<OwnedRoomId> {
        self.info.room.clone()
    }

    pub(super) fn open_member(&mut self, member: Member, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(room) = self.info_room() else { return };
        let bridged = self.rooms.iter().any(|r| r.id == room && r.network.is_some());
        if bridged {
            self.send(Cmd::MemberIds(room.clone(), member.id.clone()), cx);
        }
        self.info.member = Some(Picked { member, ids: Vec::new(), removing: false });
        self.chat_sheet = Some(room);
        self.sheet_at = Some(at);
        self.disappear.picking = false;
        cx.notify();
    }

    pub(super) fn member_ids(&mut self, user: OwnedUserId, ids: Vec<String>, cx: &mut Context<Self>) {
        if let Some(picked) = self.info.member.as_mut().filter(|p| p.member.id == user) {
            picked.ids = ids;
            cx.notify();
        }
    }

    /// The member sheet's title and rows, while one is picked.
    pub(super) fn member_sheet(&self, live: bool, window: &mut Window, cx: &mut Context<Self>) -> Option<(usize, Vec<Stateful<Div>>)> {
        let picked = self.info.member.as_ref()?;
        let details = self.info.details.as_ref()?;
        let room = self.info_room()?;
        let p = Palette::of(cx);
        let m = picked.member.clone();
        let close = |this: &mut Self, cx: &mut Context<Self>| {
            this.chat_sheet = None;
            cx.notify();
        };
        if picked.removing {
            let question = h_flex()
                .id("member-remove-question")
                .min_h(px(SHEET_ROW_H))
                .px(px(GUTTER + 4.))
                .text_size(fs(16.))
                .line_height(fs(22.))
                .child(div().flex_1().min_w_0().child(t!("Remove {name} from the group?", name = m.name)));
            let mut yes = sheet_row("member-remove-yes", IconName::UserMinus, t!("Remove"), p.negative, window, cx);
            let mut no = sheet_row("member-remove-no", IconName::X, t!("Cancel"), p.text, window, cx);
            if live {
                let user = m.id.clone();
                yes = yes.on_click(cx.listener(move |this, _, _, cx| {
                    this.send(Cmd::Group { room: room.clone(), change: Change::Remove(user.clone()) }, cx);
                    close(this, cx);
                }));
                no = no.on_click(cx.listener(move |this, _, _, cx| close(this, cx)));
            }
            return Some((3, vec![question, yes, no]));
        }
        let line = match picked.ids.first() {
            Some(id) => id.split_once(':').map_or(id.as_str(), |(_, rest)| rest).to_owned(),
            None if details.bridge.is_some() => String::new(),
            None => m.id.to_string(),
        };
        let head = h_flex()
            .id("member-head")
            .px(px(GUTTER + 4.))
            .py(px(8.))
            .gap(px(GAP))
            .child(avatar(&m.name, self.faces.get(&m.id), FACE, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_size(fs(18.)).line_height(fs(24.)).font_weight(FontWeight::SEMIBOLD).truncate().child(m.name.clone()))
                    .when(!line.is_empty(), |el| el.child(div().text_size(fs(14.)).line_height(fs(20.)).text_color(p.faint).truncate().child(line))),
            );
        let mut rows = vec![head];
        let target = match &details.bridge {
            Some((bridge, login)) => Target::Bridge { bridge: bridge.clone(), login: login.clone() },
            None => Target::Matrix,
        };
        let message = sheet_row("member-message", IconName::MessageCircle, t!("Send message"), p.text, window, cx);
        let mention = sheet_row("member-mention", IconName::AtSign, t!("Mention"), p.text, window, cx);
        let remove = (details.can.remove && m.removable).then(|| sheet_row("member-remove", IconName::UserMinus, t!("Remove from group"), p.negative, window, cx));
        let network = self.rooms.iter().find(|r| r.id == room).and_then(|r| r.network.clone());
        let block = (self.user.as_ref() != Some(&m.id)).then(|| self.block_row(m.id.clone(), m.name.clone(), network, live, window, cx));
        if !live {
            rows.extend([message, mention]);
            rows.extend(block);
            rows.extend(remove);
            return Some((2, rows));
        }
        let who = m.id.to_string();
        rows.push(message.on_click(cx.listener(move |this, _, _, cx| {
            this.send(Cmd::People(Ask::Start { target: target.clone(), who: who.clone() }), cx);
            close(this, cx);
        })));
        let (user, name) = (m.id.clone(), m.name.clone());
        rows.push(mention.on_click(cx.listener(move |this, _, window, cx| {
            close(this, cx);
            this.close_info(cx);
            this.mention_member(user.clone(), name.clone(), window, cx);
        })));
        rows.extend(block);
        rows.extend(remove.map(|row| {
            row.on_click(cx.listener(|this, _, _, cx| {
                if let Some(picked) = this.info.member.as_mut() {
                    picked.removing = true;
                }
                cx.notify();
            }))
        }));
        Some((2, rows))
    }

    pub(super) fn open_group_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (name, name_sub) = text_field(t!("Group name"), window, cx);
        let (topic, topic_sub) = text_field(t!("Description"), window, cx);
        let current = self.info_room().and_then(|id| self.rooms.iter().find(|r| r.id == id)).map(|r| r.name.clone()).unwrap_or_default();
        let about = self.info.details.as_ref().and_then(|d| d.topic.clone()).unwrap_or_default();
        name.update(cx, |input, cx| input.set_value(current, window, cx));
        topic.update(cx, |input, cx| input.set_value(about, window, cx));
        self.info.page = Some(Page::Edit { name, topic, _subs: [name_sub, topic_sub] });
        cx.notify();
    }

    pub(super) fn open_group_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(room) = self.info_room() else { return };
        let (query, _) = text_field(t!("Name or Matrix ID"), window, cx);
        let sub = cx.subscribe_in(&query, window, |_, _, ev: &InputEvent, _, cx| {
            keyboard_follows(ev, Keyboard::Text, cx);
            cx.notify();
        });
        self.send(Cmd::Candidates(room), cx);
        self.info.page = Some(Page::Add { query, people: None, added: Vec::new(), _sub: sub });
        cx.notify();
    }

    pub(super) fn candidates_arrived(&mut self, room: OwnedRoomId, list: Vec<(OwnedUserId, String)>, cx: &mut Context<Self>) {
        if self.info.room.as_ref() == Some(&room)
            && let Some(Page::Add { people, .. }) = &mut self.info.page
        {
            *people = Some(list);
            cx.notify();
        }
    }

    /// The server took a change: the screen shows it before the sync brings it.
    pub(super) fn group_changed(&mut self, room: OwnedRoomId, change: Change, cx: &mut Context<Self>) {
        if self.info.room.as_ref() != Some(&room) {
            return;
        }
        let Some(d) = self.info.details.as_mut() else { return };
        match change {
            Change::Topic(topic) => d.topic = Some(topic.trim().to_owned()).filter(|t| !t.is_empty()),
            Change::Remove(user) => {
                let before = d.members.len();
                d.members.retain(|m| m.id != user);
                d.joined = d.joined.saturating_sub((before - d.members.len()) as u64);
            }
            Change::Invite(user) => {
                if let Some(Page::Add { added, .. }) = &mut self.info.page
                    && !added.contains(&user)
                {
                    added.push(user);
                }
            }
            Change::Name(_) | Change::Photo(_) => {}
        }
        self.info_rows();
        cx.notify();
    }

    fn save_group(&mut self, cx: &mut Context<Self>) {
        let (Some(room), Some(Page::Edit { name, topic, .. }), Some(d)) = (self.info_room(), &self.info.page, &self.info.details) else { return };
        let (name, topic) = (name.read(cx).value().trim().to_owned(), topic.read(cx).value().trim().to_owned());
        let current = self.rooms.iter().find(|r| r.id == room).map(|r| r.name.clone()).unwrap_or_default();
        let mut changes = Vec::new();
        if d.can.name && !name.is_empty() && name != current {
            changes.push(Change::Name(name));
        }
        if d.can.topic && topic != d.topic.clone().unwrap_or_default() {
            changes.push(Change::Topic(topic));
        }
        for change in changes {
            self.send(Cmd::Group { room: room.clone(), change }, cx);
        }
        self.info.page = None;
        cx.notify();
    }

    fn choose_group_photo(&mut self, cx: &mut Context<Self>) {
        let Some(room) = self.info_room() else { return };
        let picked = crate::pick_file(cx);
        cx.spawn(async move |this, cx| {
            let Some(path) = picked.await else { return };
            let _ = this.update(cx, |this, cx| {
                this.send(Cmd::Group { room, change: Change::Photo(path) }, cx);
            });
        })
        .detach();
    }

    /// Puts `@name` for this member into the composer, where the cursor is.
    fn mention_member(&mut self, user: OwnedUserId, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let text = format!("@{name}");
        self.composer.update(cx, |c, cx| {
            let at = c.cursor();
            if c.replace_range_with_token(at..at, InlineToken::new(user.to_string(), text.clone()), window, cx).is_ok() {
                c.insert(" ", window, cx);
            } else {
                c.replace(format!("{text} "), window, cx);
            }
        });
        let focus = self.composer.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    pub(super) fn group_page_title(&self) -> Option<&'static str> {
        match self.info.page.as_ref()? {
            Page::Edit { .. } => Some(t!("Edit group")),
            Page::Add { .. } => Some(t!("Add members")),
        }
    }

    pub(super) fn render_group_page(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = Palette::of(cx);
        let d = self.info.details.clone()?;
        let room = self.info.room.as_ref().and_then(|id| self.rooms.iter().find(|r| &r.id == id)).cloned()?;
        let label = |text: &'static str| div().text_size(fs(12.)).text_color(p.faint).child(text);
        let body = v_flex().id("group-page").flex_1().min_h_0().overflow_y_scroll().px(px(GUTTER)).pt(px(8.)).pb(px(GAP * 2.)).gap(px(GAP));
        Some(match self.info.page.as_ref()? {
            Page::Edit { name, topic, .. } => body
                .child(
                    v_flex()
                        .items_center()
                        .gap(px(GAP))
                        .child(avatar(&room.name, self.avatars.get(&room.id), 96., cx))
                        .when(d.can.photo, |el| {
                            el.child(Button::new("group-photo").label(t!("Change photo")).on_click(cx.listener(|this, _, _, cx| this.choose_group_photo(cx))))
                        }),
                )
                .when(d.can.name, |el| el.child(v_flex().gap(px(4.)).child(label(t!("Group name"))).child(Input::new(name)).child(crate::ime::marks(name))))
                .when(d.can.topic, |el| el.child(v_flex().gap(px(4.)).child(label(t!("Description"))).child(Input::new(topic)).child(crate::ime::marks(topic))))
                .when_some(room.network.clone(), |el, network| {
                    el.child(div().text_size(fs(13.)).line_height(fs(18.)).text_color(p.muted).child(t!("The bridge makes these changes in {network} too.", network = network)))
                })
                .when(d.can.name || d.can.topic, |el| {
                    el.child(h_flex().child(Button::new("group-save").primary().label(t!("Save")).on_click(cx.listener(|this, _, _, cx| this.save_group(cx)))))
                })
                .into_any_element(),
            Page::Add { query, people, added, .. } => {
                let typed = query.read(cx).value().trim().to_owned();
                let needle = typed.to_lowercase();
                let found: Vec<(OwnedUserId, String)> = people
                    .iter()
                    .flatten()
                    .filter(|(id, name)| needle.is_empty() || name.to_lowercase().contains(&needle) || id.as_str().to_lowercase().contains(&needle))
                    .take(SHOWN)
                    .cloned()
                    .collect();
                // A full Matrix ID typed or pasted as a link is offered as it is, unless it is listed or already in.
                let typed_id = crate::backend::rooms::matrix_id(&typed)
                    .filter(|u| room.network.is_none() && !found.iter().any(|(id, _)| id == u) && !d.members.iter().any(|m| &m.id == u));
                let note = match people {
                    None => Some(t!("Loading…")),
                    Some(list) if list.is_empty() && typed_id.is_none() => {
                        Some(if room.network.is_some() { t!("No one else from your chats on this network.") } else { t!("Type a full Matrix ID, like @name:matrix.org.") })
                    }
                    _ => None,
                };
                let added = added.clone();
                let rows = typed_id.map(|u| (u.clone(), u.to_string())).into_iter().chain(found).map(|(user, name)| {
                    let done = added.contains(&user);
                    let room_id = room.id.clone();
                    h_flex()
                        .id(SharedString::from(format!("add-{user}")))
                        .w_full()
                        .h(px(56.))
                        .gap(px(12.))
                        .when(!done, |el| el.cursor_pointer().active(|el| el.bg(p.surface)))
                        .child(avatar(&name, None, 40., cx))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_size(fs(16.)).truncate().child(name.clone()))
                                .when(room.network.is_none(), |el| el.child(div().text_size(fs(13.)).text_color(p.faint).truncate().child(user.to_string()))),
                        )
                        .child(if done {
                            div().flex_none().text_size(fs(13.)).text_color(p.faint).child(t!("Invited")).into_any_element()
                        } else {
                            Icon::new(IconName::UserPlus).size_5().flex_none().text_color(p.muted).into_any_element()
                        })
                        .when(!done, |el| {
                            el.on_click(cx.listener(move |this, _, _, cx| {
                                this.send(Cmd::Group { room: room_id.clone(), change: Change::Invite(user.clone()) }, cx);
                            }))
                        })
                });
                body.child(Input::new(query))
                    .child(crate::ime::marks(query))
                    .children(note.map(|n| div().pt(px(8.)).text_size(fs(14.)).text_color(p.faint).child(n)))
                    .children(rows)
                    .into_any_element()
            }
        })
    }
}

