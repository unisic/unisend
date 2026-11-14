//! Disappearing messages, as Signal and WhatsApp have them: the timer under the chat's name, its
//! setting in the chat's sheet, and a timer on a message that will disappear. The bridge deletes
//! the expired messages; what this file shows is read in `backend/disappear.rs`.

use super::*;
use crate::backend::{Disappearing, disappear_label};

#[derive(Default)]
pub(super) struct Disappear {
    /// The open chat's timer, once read.
    chat: Option<(OwnedRoomId, Disappearing)>,
    /// The chat's sheet lists the timers instead of the chat's actions.
    pub picking: bool,
}

impl BeeperApp {
    pub(super) fn on_disappear(&mut self, room: OwnedRoomId, setting: Disappearing) {
        if self.open.as_ref() == Some(&room) {
            self.disappear.chat = Some((room, setting));
            // Its row on the info screen appears or changes; the list measured it before.
            if self.info_open() {
                self.info_rows();
            }
        }
    }

    fn disappearing(&self, room: &OwnedRoomId) -> Option<&Disappearing> {
        self.disappear.chat.as_ref().filter(|(id, _)| id == room).map(|(_, setting)| setting)
    }

    /// The line under the chat's name: its network, and the timer while it is on.
    pub(super) fn header_subline(&self, network: Option<String>, cx: &App) -> Option<Div> {
        let p = Palette::of(cx);
        let timer = self.open.as_ref().and_then(|room| self.disappearing(room)).map(|d| d.ms).filter(|&ms| ms > 0);
        if network.is_none() && timer.is_none() {
            return None;
        }
        Some(
            h_flex()
                .min_w_0()
                .gap(px(4.))
                .text_size(fs(12.))
                .line_height(fs(16.))
                .text_color(p.faint)
                .children(network.map(|n| div().min_w_0().truncate().child(n)))
                .children(timer.map(|ms| {
                    h_flex()
                        .id("header-timer")
                        .flex_none()
                        .gap(px(3.))
                        .child(Icon::new(IconName::Timer).size_3p5().text_color(p.faint))
                        .child(disappear_label(ms))
                })),
        )
    }

    /// Whether the message at `ix` will disappear: its own content says so, or for the user's
    /// message, the bridge's word on it does.
    pub(super) fn disappears(&self, ix: usize) -> bool {
        let m = &self.msgs[ix];
        m.disappear > 0
            || (m.own && self.msgs[ix + 1..].iter().any(|row| row.disappear > 0 && matches!(&row.kind, Kind::Status { target, .. } if *target == m.id)))
    }

    /// The row of a chat's sheet that leads to its timer: shown for the open chat once its timer
    /// is known, when the timer is on or can be set. Usable on a chat info screen as well.
    pub(super) fn disappear_entry(&self, room: &OwnedRoomId, live: bool, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let setting = self.disappearing(room)?;
        if setting.ms == 0 && !setting.settable() {
            return None;
        }
        let p = Palette::of(cx);
        let row = timer_row("disappear-entry", Some(IconName::Timer), "Disappearing messages", cx)
            .child(div().flex_1())
            .child(div().text_size(fs(14.)).text_color(p.faint).child(disappear_label(setting.ms)));
        if !live {
            return Some(row);
        }
        let room = room.clone();
        Some(row.on_click(cx.listener(move |this, _, _, cx| {
            // From the info screen there is no sheet open yet to show the timers in.
            this.chat_sheet = Some(room.clone());
            this.disappear.picking = true;
            cx.notify();
        })))
    }

    /// The timer's sheet: Off and the timers the bridge takes, the current one checked; where it
    /// cannot be changed from here, the timer and why.
    pub(super) fn disappear_sheet(&self, room: &OwnedRoomId, live: bool, cx: &mut Context<Self>) -> Option<(String, Vec<Stateful<Div>>)> {
        let setting = self.disappearing(room)?;
        let title = "Disappearing messages".to_owned();
        if !setting.settable() {
            let network = self.rooms.iter().find(|r| &r.id == room).and_then(|r| r.network.clone());
            let why = match (&setting.choices, network) {
                (Some(_), _) => "Only the chat's admins can change this.".to_owned(),
                (None, Some(network)) => format!("This chat's bridge does not take this change from Murmur. Change it in {network}."),
                (None, None) => "This chat does not take this change from Murmur.".to_owned(),
            };
            let p = Palette::of(cx);
            let current = timer_row("disappear-current", Some(IconName::Timer), disappear_label(setting.ms), cx).cursor_default();
            let note = h_flex()
                .id("disappear-note")
                .h(px(SHEET_ROW_H))
                .px(px(GUTTER + 4.))
                .text_size(fs(13.))
                .line_height(fs(18.))
                .text_color(p.faint)
                .child(div().flex_1().min_w_0().child(why));
            return Some((title, vec![current, note]));
        }
        let mut timers = vec![0];
        timers.extend(setting.choices.iter().flat_map(|c| c.timers.iter().copied()));
        // A timer set elsewhere that this list lacks is still the one in force.
        if !timers.contains(&setting.ms) {
            timers.push(setting.ms);
            timers[1..].sort_unstable();
        }
        let rows = timers
            .into_iter()
            .map(|ms| {
                let current = ms == setting.ms;
                let row = timer_row(("disappear-timer", ms as usize), current.then_some(IconName::Check), disappear_label(ms), cx);
                if !live {
                    return row;
                }
                let room = room.clone();
                row.on_click(cx.listener(move |this, _, _, cx| {
                    if !current {
                        this.send(Cmd::SetDisappear { room: room.clone(), ms }, cx);
                    }
                    this.chat_sheet = None;
                    cx.notify();
                }))
            })
            .collect();
        Some((title, rows))
    }
}

/// A sheet row with a label made at run time; without an icon, its place stays empty so labels line up.
fn timer_row(id: impl Into<ElementId>, icon: Option<IconName>, label: impl Into<SharedString>, cx: &App) -> Stateful<Div> {
    let p = Palette::of(cx);
    h_flex()
        .id(id)
        .h(px(SHEET_ROW_H))
        .active(|el| el.bg(p.raised))
        .px(px(GUTTER + 4.))
        .gap(px(GAP))
        .text_size(fs(16.))
        .text_color(p.text)
        .cursor_pointer()
        .child(match icon {
            Some(icon) => Icon::new(icon).size_5().into_any_element(),
            None => div().size_5().flex_none().into_any_element(),
        })
        .child(label.into())
}
