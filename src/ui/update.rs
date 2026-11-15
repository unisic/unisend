//! Updates outside an app store: the Settings section, the notice that one is ready, and the
//! steps from a tap on Update to Android's installer. Nothing installs without that tap.

use gpui_kit::component::switch::Switch;

use super::panels::{card, footnote, row};
use super::*;
use crate::backend::update::{self, Prefs, Release, Step};
use crate::i18n::t;

#[derive(Default)]
pub(super) enum Status {
    #[default]
    Idle,
    Checking,
    Latest,
    CheckFailed(String),
    Found(Release),
    /// Waiting for the user to allow installing apps in Android's settings.
    Allowing(Release),
    Downloading(Release, u8),
    Installing(Release),
    Failed(Release, String),
}

impl Status {
    fn release(&self) -> Option<&Release> {
        match self {
            Status::Found(r) | Status::Allowing(r) | Status::Downloading(r, _) | Status::Installing(r) | Status::Failed(r, _) => Some(r),
            _ => None,
        }
    }

    fn busy(&self) -> bool {
        matches!(self, Status::Checking | Status::Downloading(..))
    }
}

pub(super) struct Updates {
    prefs: Prefs,
    status: Status,
    /// The notice was put away with Later, until the next start.
    later: bool,
}

impl Updates {
    pub(super) fn load(dir: &std::path::Path) -> Self {
        Self { prefs: Prefs::load(dir), status: Status::Idle, later: false }
    }
}

impl BeeperApp {
    /// On start and on every return to the app: checks when automatic checks are on and a day has passed.
    pub(super) fn check_update_if_due(&mut self, cx: &mut Context<Self>) {
        let now = update::now();
        if !update::enabled() || self.updates.status.busy() || self.updates.status.release().is_some() || !self.updates.prefs.due(now) {
            return;
        }
        // Quietly: a full queue just means the next return tries again.
        if self.cmd.try_send(Cmd::UpdateCheck).is_ok() {
            self.updates.prefs.last_check = now;
            self.updates.prefs.save(&self.dir);
            self.updates.status = Status::Checking;
            cx.notify();
        }
    }

    /// Back from Android's settings: the update goes on once installing is allowed.
    pub(super) fn update_returned(&mut self, cx: &mut Context<Self>) {
        if let Status::Allowing(release) = &self.updates.status
            && update::can_install()
        {
            let release = release.clone();
            self.install_update(release, cx);
        }
    }

    fn check_update_now(&mut self, cx: &mut Context<Self>) {
        if self.updates.status.busy() {
            return;
        }
        if self.send(Cmd::UpdateCheck, cx) {
            self.updates.prefs.last_check = update::now();
            self.updates.prefs.save(&self.dir);
            self.updates.status = Status::Checking;
        }
    }

    fn set_auto_update(&mut self, on: bool, cx: &mut Context<Self>) {
        self.updates.prefs.auto = on;
        self.updates.prefs.save(&self.dir);
        cx.notify();
    }

    fn install_update(&mut self, release: Release, cx: &mut Context<Self>) {
        if !update::can_install() {
            update::allow_install();
            self.updates.status = Status::Allowing(release);
        } else if self.send(Cmd::UpdateInstall(release.clone()), cx) {
            self.updates.status = Status::Downloading(release, 0);
        }
        cx.notify();
    }

    pub(super) fn on_update(&mut self, step: Step) {
        let status = std::mem::take(&mut self.updates.status);
        self.updates.status = match (step, status) {
            (Step::Latest, _) => Status::Latest,
            (Step::Found(release), _) => Status::Found(release),
            (Step::CheckFailed(e), _) => Status::CheckFailed(e),
            (Step::Progress(p), Status::Downloading(r, _)) => Status::Downloading(r, p),
            (Step::Installing, Status::Downloading(r, _)) => Status::Installing(r),
            (Step::Failed(e), status) => match status.release() {
                Some(r) => Status::Failed(r.clone(), e),
                None => Status::CheckFailed(e),
            },
            // A late progress or a step for an update no longer shown.
            (_, status) => status,
        };
    }

    fn status_line(&self) -> String {
        match &self.updates.status {
            Status::Idle => String::new(),
            Status::Checking => t!("Checking…").to_owned(),
            Status::Latest => t!("You have the latest version.").to_owned(),
            Status::CheckFailed(e) => t!("Could not check: {error}", error = e),
            Status::Found(r) => r.notes.clone(),
            Status::Allowing(_) => t!("Allow Unisend to install apps in the settings that opened, then come back.").to_owned(),
            Status::Downloading(_, p) => t!("Downloading… {percent}%", percent = p),
            Status::Installing(_) => t!("Installing. Android may ask you to confirm.").to_owned(),
            Status::Failed(_, e) => t!("Not updated: {error}", error = e),
        }
    }

    /// The pill that moves the update on, when there is a step for the user to take.
    fn update_action(&self, id: &'static str, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let (label, release) = match &self.updates.status {
            Status::Found(r) => (t!("Update|install"), r.clone()),
            // Installing stays here when Android could not show its confirmation.
            Status::Failed(r, _) | Status::Installing(r) => (t!("Try again"), r.clone()),
            Status::Allowing(r) => (t!("Allow"), r.clone()),
            _ => return None,
        };
        let p = Palette::of(cx);
        Some(
            h_flex()
                .id(id)
                .h(px(TAP))
                .px(px(GAP))
                .flex_none()
                .rounded_full()
                .bg(p.raised)
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.install_update(release.clone(), cx)))
                .child(label),
        )
    }

    /// What the settings list says of updates; `None` in a build that has no manifest URL.
    pub(super) fn updates_summary(&self) -> Option<String> {
        if !update::enabled() {
            return None;
        }
        Some(match self.updates.status.release() {
            Some(r) => t!("Update to {version}", version = r.version_name),
            None if self.updates.prefs.auto => t!("Checked once a day").to_owned(),
            None => t!("Checked when you ask").to_owned(),
        })
    }

    /// The Updates page of Settings; `None` in a build that has no manifest URL.
    pub(super) fn render_updates(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        if !update::enabled() {
            return None;
        }
        let p = Palette::of(cx);
        let weak = cx.entity().downgrade();
        let auto = Switch::new("update-auto").checked(self.updates.prefs.auto).on_click(move |on, _, cx| {
            let _ = weak.update(cx, |this, cx| this.set_auto_update(*on, cx));
        });
        let host = update::host();
        let line = self.status_line();
        let found = self.updates.status.release().map(|r| t!("Update to {version}", version = r.version_name));
        let check = h_flex()
            .id("update-check")
            .h(px(TAP))
            .px(px(GAP))
            .flex_none()
            .rounded_full()
            .bg(p.raised)
            .when(self.updates.status.busy(), |el| el.opacity(0.5))
            .cursor_pointer()
            .on_click(cx.listener(|this, _, _, cx| this.check_update_now(cx)))
            .child(t!("Check"));
        let action = self.update_action("update-install", cx);
        Some(
            v_flex()
                .gap(px(8.))
                .child(card(cx).child(row(t!("Check for updates automatically"), "", cx).child(auto)).child(match found {
                    Some(label) => row(&label, &line, cx).children(action),
                    None => row(t!("Check now"), &line, cx).child(check),
                }))
                .child(footnote(&t!("Once a day Unisend asks {host} for the newest version. It sends nothing about you or this phone.", host = host), cx)),
        )
    }

    /// A quiet notice above the screen while an update waits for the user, until Later.
    pub(super) fn render_update_notice(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        let release = self.updates.status.release()?;
        if self.updates.later {
            return None;
        }
        let p = Palette::of(cx);
        let title = t!("Unisend {version} is available.", version = release.version_name);
        let line = self.status_line();
        let action = self.update_action("update-notice-install", cx);
        let waiting = matches!(self.updates.status, Status::Found(_) | Status::Failed(..));
        Some(
            v_flex()
                .mx(px(GUTTER))
                .mt(px(8.))
                .p(px(GAP))
                .gap(px(6.))
                .rounded(px(RADIUS))
                .bg(p.surface)
                .text_size(fs(14.))
                .line_height(fs(20.))
                .child(div().child(title))
                .when(!line.is_empty(), |el| el.child(div().text_color(p.muted).line_clamp(3).child(line)))
                .child(
                    h_flex()
                        .gap(px(8.))
                        .justify_end()
                        .when(waiting, |el| {
                            el.child(
                                h_flex()
                                    .id("update-later")
                                    .h(px(TAP))
                                    .px(px(GAP))
                                    .rounded_full()
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.updates.later = true;
                                        cx.notify();
                                    }))
                                    .child(t!("Later")),
                            )
                        })
                        .children(action),
                ),
        )
    }
}
