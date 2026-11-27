//! The setup window's pages. Each shows plain data from the
//! `SetupSnapshot` (and the permission states), refreshed twice a second.
//! Every state is a symbol plus words, never colour alone.

use glance_orchestrator::settings::{DownloadIssue, DownloadPhase, FileProgress, FileState};
use glance_orchestrator::setup::{
    Access, PermissionAction, RamTier, SetupSnapshot, TEST_QUESTION, TestResult, works_line,
};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSButton, NSColor, NSControlSize, NSControlStateValueOff, NSControlStateValueOn, NSFont,
    NSImageView, NSProgressIndicator, NSProgressIndicatorStyle, NSStackView, NSTextField, NSView,
};
use objc2_foundation::NSString;

use super::perms::Which;
use super::{Ctx, Reply};
use crate::settings::ui::{self, Page, Ui};
use crate::signing::Signing;

/// Wrapping width of body text.
const TEXT: f64 = 520.0;

/// The README's signing section, opened in the browser only when the user
/// clicks (glance itself sends nothing).
const SIGNING_URL: &str = "https://github.com/Matty-PW/glance#keep-permissions-across-rebuilds";

pub(crate) trait StepPage {
    fn view(&self) -> Retained<NSView>;
    fn fill(&mut self, snap: &SetupSnapshot, env: &Env);
    fn on_change(&mut self, _id: isize, _ctx: &mut Ctx<'_>) -> Reply {
        Reply::Nothing
    }
    /// Snapshots: show the "Advanced" part open.
    fn open_advanced(&mut self) {}
}

/// What the platform knows, read on each refresh.
pub(crate) struct Env {
    pub screen: Access,
    pub mic: Access,
    pub signing: Signing,
}

fn body(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let l = NSTextField::wrappingLabelWithString(&NSString::from_str(text), mtm);
    l.setFont(Some(&NSFont::systemFontOfSize(13.0)));
    l.setPreferredMaxLayoutWidth(TEXT);
    l.setSelectable(false);
    l
}

fn set(label: &NSTextField, text: &str) {
    label.setStringValue(&NSString::from_str(text));
}

fn view_of<T: AsRef<NSView>>(v: &T) -> Retained<NSView> {
    use objc2::Message;
    v.as_ref().retain()
}

pub(crate) fn gb(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else {
        format!("{:.0} MB", (bytes as f64 / 1e6).max(1.0))
    }
}

/// "38 MB/s · about 2 min left".
pub(crate) fn speed_line(rate: Option<f64>, left: Option<std::time::Duration>) -> String {
    let mut parts = Vec::new();
    if let Some(rate) = rate {
        parts.push(format!("{:.0} MB/s", rate / 1e6));
    }
    if let Some(left) = left {
        let s = left.as_secs();
        parts.push(match s {
            0..60 => "less than a minute left".to_owned(),
            60..3600 => format!("about {} min left", s.div_ceil(60)),
            _ => format!("about {} h {} min left", s / 3600, (s % 3600) / 60),
        });
    }
    parts.join(" · ")
}

/// A symbol and words, as one line.
struct Badge {
    view: Retained<NSStackView>,
    icon: Retained<NSImageView>,
    text: Retained<NSTextField>,
}

impl Badge {
    fn new(mtm: MainThreadMarker) -> Self {
        let icon = NSImageView::new(mtm);
        ui::auto(&icon);
        ui::activate(&[
            icon.widthAnchor().constraintEqualToConstant(18.0),
            icon.heightAnchor().constraintEqualToConstant(18.0),
        ]);
        let text = body(mtm, "");
        text.setPreferredMaxLayoutWidth(TEXT - 30.0);
        let view = ui::stack(mtm, &[&icon, &text], false, 8.0);
        view.setAlignment(objc2_app_kit::NSLayoutAttribute::Top);
        Self { view, icon, text }
    }

    fn show(&self, symbol: &str, text: &str, accent: bool) {
        self.icon
            .setImage(crate::settings::symbol(symbol, text).as_deref());
        let tint = if accent {
            crate::theme::accent()
        } else {
            NSColor::secondaryLabelColor()
        };
        self.icon.setContentTintColor(Some(&tint));
        set(&self.text, text);
        self.view.setHidden(false);
    }

    fn hide(&self) {
        self.view.setHidden(true);
    }
}

/// Keycaps that are rebuilt only when the chord changes.
struct Caps {
    holder: Retained<NSStackView>,
    chord: String,
}

impl Caps {
    fn new(mtm: MainThreadMarker) -> Self {
        Self {
            holder: ui::stack(mtm, &[], false, 0.0),
            chord: String::new(),
        }
    }

    fn set(&mut self, mtm: MainThreadMarker, chord: &str) {
        if self.chord == chord {
            return;
        }
        self.chord = chord.to_owned();
        for old in self.holder.arrangedSubviews().iter() {
            self.holder.removeArrangedSubview(&old);
            old.removeFromSuperview();
        }
        let caps = ui::keycaps(mtm, chord);
        // As narrow as the keys, so the row puts them on the right.
        caps.setHuggingPriority_forOrientation(
            751.0,
            objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
        );
        self.holder.addArrangedSubview(&caps);
    }
}

// ---------------------------------------------------------------- Welcome

pub(crate) struct Welcome {
    page: Page,
}

impl Welcome {
    pub(crate) fn new(ui: &Ui<'_>) -> Self {
        let mtm = ui.mtm;
        let page = Page::new(mtm);
        page.group(
            mtm,
            "",
            &[view_of(&*body(
                mtm,
                "glance answers questions about what's on your screen. Hold a hotkey, ask out \
                 loud, and hear the answer.",
            ))],
        );
        let line = |symbol: &str, text: &str| {
            let badge = Badge::new(mtm);
            badge.show(symbol, text, true);
            view_of(&*badge.view)
        };
        page.group(
            mtm,
            "Privacy",
            &[
                line(
                    "lock",
                    "Everything runs on this Mac. The screenshot, your voice and the answers \
                     stay on it.",
                ),
                line(
                    "network.slash",
                    "Nothing is sent anywhere: no account, no telemetry, no update checks.",
                ),
                line(
                    "arrow.down.circle",
                    "The only download is the models, from Hugging Face, each checked against \
                     a pinned checksum.",
                ),
            ],
        );
        page.add(&ui::secondary(
            mtm,
            "Setup takes a few minutes; most of it is the download.",
            TEXT,
        ));
        Self { page }
    }
}

impl StepPage for Welcome {
    fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.page.root.clone())
    }

    fn fill(&mut self, _snap: &SetupSnapshot, _env: &Env) {}
}

// ---------------------------------------------------------------- Your Mac

const ADVANCED: isize = 2;
const FIRST_PRESET: isize = 10;

struct Card {
    id: String,
    radio: Retained<NSButton>,
    detail: Retained<NSTextField>,
    view: Retained<NSView>,
}

pub(crate) struct YourMac {
    page: Page,
    mtm: MainThreadMarker,
    chip: Retained<NSTextField>,
    ram: Retained<NSTextField>,
    disk: Retained<NSTextField>,
    llama: Retained<NSTextField>,
    llama_note: Retained<NSTextField>,
    warning: Badge,
    recommended: Retained<NSStackView>,
    others: Retained<NSStackView>,
    advanced_block: Retained<NSView>,
    advanced: Retained<NSButton>,
    cards: Vec<Card>,
    ui_target: Retained<objc2::runtime::AnyObject>,
    page_index: isize,
}

impl YourMac {
    pub(crate) fn new(ui: &Ui<'_>) -> Self {
        let mtm = ui.mtm;
        let page = Page::new(mtm);
        let value = || {
            let l = ui::label(mtm, "");
            l.setTextColor(Some(&NSColor::secondaryLabelColor()));
            l
        };
        let (chip, ram, disk, llama) = (value(), value(), value(), value());
        let llama_note = ui::secondary(mtm, "", ui::TEXT_W);
        let llama_text = ui::stack(
            mtm,
            &[&ui::label(mtm, "llama-server"), &llama_note],
            true,
            2.0,
        );
        page.group(
            mtm,
            "This Mac",
            &[
                ui::row(mtm, "Chip", None, false, &chip),
                ui::row(mtm, "Memory", None, false, &ram),
                ui::row(mtm, "Free disk space", None, false, &disk),
                ui::row_with(mtm, &llama_text, &llama),
            ],
        );
        let warning = Badge::new(mtm);
        page.add(&warning.view);
        let recommended = ui::stack(mtm, &[], true, 10.0);
        page.group(mtm, "Recommended", &[view_of(&*recommended)]);
        let advanced = ui.disclosure(ADVANCED);
        let adv_row = ui::stack(
            mtm,
            &[&advanced, &ui::label(mtm, "Advanced: other setups")],
            false,
            4.0,
        );
        page.add(&adv_row);
        let others = ui::stack(mtm, &[], true, 10.0);
        let before = page.stack.arrangedSubviews().count();
        page.group(mtm, "", &[view_of(&*others)]);
        let advanced_block = page.stack.arrangedSubviews().objectAtIndex(before);
        advanced_block.setHidden(true);
        Self {
            page,
            mtm,
            chip,
            ram,
            disk,
            llama,
            llama_note,
            warning,
            recommended,
            others,
            advanced_block,
            advanced,
            cards: Vec::new(),
            ui_target: objc2::Message::retain(ui.target),
            page_index: ui.page,
        }
    }

    fn build_cards(&mut self, snap: &SetupSnapshot) {
        for holder in [&self.recommended, &self.others] {
            for old in holder.arrangedSubviews().iter() {
                holder.removeArrangedSubview(&old);
                old.removeFromSuperview();
            }
        }
        self.cards.clear();
        let ui = Ui {
            mtm: self.mtm,
            target: &self.ui_target,
            page: self.page_index,
        };
        for (i, preset) in snap.presets.iter().enumerate() {
            let radio = ui.radio(&preset.name, FIRST_PRESET + i as isize);
            radio.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
            let detail = ui::secondary(self.mtm, "", TEXT - 20.0);
            let card = ui::stack(self.mtm, &[&radio, &detail], true, 3.0);
            let holder = if preset.id == snap.recommended {
                &self.recommended
            } else {
                &self.others
            };
            holder.addArrangedSubview(&card);
            self.cards.push(Card {
                id: preset.id.clone(),
                radio,
                detail,
                view: Retained::into_super(card),
            });
        }
    }
}

impl StepPage for YourMac {
    fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.page.root.clone())
    }

    fn fill(&mut self, snap: &SetupSnapshot, _env: &Env) {
        if self.cards.len() != snap.presets.len()
            || self
                .cards
                .iter()
                .zip(&snap.presets)
                .any(|(c, p)| c.id != p.id)
        {
            self.build_cards(snap);
        }
        let m = &snap.machine;
        set(&self.chip, m.chip.as_deref().unwrap_or("Unknown"));
        let gib = |b: u64| b / (1 << 30);
        set(
            &self.ram,
            &match (m.ram_bytes, snap.tier) {
                (Some(b), Some(tier)) => format!("{} GB ({})", gib(b), tier.label()),
                _ => "Unknown".to_owned(),
            },
        );
        set(
            &self.disk,
            &m.free_bytes
                .map_or("Unknown".into(), |b| format!("{} free", gb(b))),
        );
        match &m.llama_server {
            Ok(path) => {
                set(&self.llama, "Found");
                set(&self.llama_note, path);
            }
            Err(why) => {
                set(&self.llama, "Not found");
                set(&self.llama_note, why);
            }
        }
        let basics = snap.presets.iter().find(|p| p.id == snap.recommended);
        match snap.tier {
            Some(RamTier::BelowMinimum) => self.warning.show(
                "exclamationmark.triangle",
                &format!(
                    "This Mac has {} GB of memory. glance uses about 7 GB itself, which leaves \
                     too little for macOS and your apps. Macs under 16 GB are not supported: \
                     only the smallest setup is offered, and it may be slow or run out of memory.",
                    m.ram_bytes.map_or(0, gib)
                ),
                true,
            ),
            None => self.warning.show(
                "exclamationmark.triangle",
                "glance could not read how much memory this Mac has, so only the smallest setup \
                 is offered.",
                true,
            ),
            _ if basics.is_some_and(|b| !b.fits_disk) => self.warning.show(
                "exclamationmark.triangle",
                &format!(
                    "There is not enough free disk space for the recommended setup: it needs {} \
                     (including 2 GB kept free for macOS) and {} is free.",
                    gb(basics.map_or(0, |b| b.missing_bytes) + 2_000_000_000),
                    m.free_bytes.map_or("unknown".into(), gb)
                ),
                true,
            ),
            _ => self.warning.hide(),
        }
        if !snap.machine.llama_server.is_ok() && snap.tier.is_some() {
            // The llama-server row already says what to do.
        }
        for (card, preset) in self.cards.iter().zip(&snap.presets) {
            card.radio.setState(if preset.id == snap.chosen {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
            card.radio.setEnabled(preset.offered);
            let mut lines = vec![preset.summary.clone()];
            lines.extend(preset.includes.iter().map(|i| format!("• {i}")));
            lines.push(if preset.missing_bytes == 0 {
                format!("Download: {} (already downloaded)", gb(preset.total_bytes))
            } else if preset.missing_bytes < preset.total_bytes {
                format!(
                    "Download: {} in total, {} still to download",
                    gb(preset.total_bytes),
                    gb(preset.missing_bytes)
                )
            } else {
                format!("Download: {}", gb(preset.total_bytes))
            });
            lines.push(format!("Memory: {}. {}", preset.memory, preset.memory_note));
            if !preset.offered {
                lines.push("Not offered on this Mac: it needs more memory.".into());
            } else if !preset.fits_disk {
                lines.push("Not enough free disk space for this one.".into());
            }
            set(&card.detail, &lines.join("\n"));
            card.view.setHidden(false);
        }
    }

    fn open_advanced(&mut self) {
        self.advanced.setState(NSControlStateValueOn);
        self.advanced_block.setHidden(false);
    }

    fn on_change(&mut self, id: isize, ctx: &mut Ctx<'_>) -> Reply {
        if id == ADVANCED {
            let open = self.advanced.state() == NSControlStateValueOn;
            self.advanced_block.setHidden(!open);
            return Reply::Nothing;
        }
        let Some(card) = usize::try_from(id - FIRST_PRESET)
            .ok()
            .and_then(|i| self.cards.get(i))
        else {
            return Reply::Nothing;
        };
        ctx.backend.choose(&card.id);
        Reply::Refresh
    }
}

// ---------------------------------------------------------------- Permissions

const SCREEN_BUTTON: isize = 1;
const MIC_BUTTON: isize = 2;
const SIGNING_LINK: isize = 3;

pub(crate) struct PermissionsPage {
    page: Page,
    screen: Badge,
    mic: Badge,
    screen_button: Retained<NSButton>,
    mic_button: Retained<NSButton>,
    restart_note: Retained<NSTextField>,
    signing_block: Retained<NSView>,
    signing_text: Retained<NSTextField>,
    commands: Retained<NSTextField>,
    signing_link: Retained<NSButton>,
    actions: [Option<PermissionAction>; 2],
}

fn action_title(action: Option<PermissionAction>) -> &'static str {
    match action {
        Some(PermissionAction::Ask) => "Allow…",
        Some(PermissionAction::OpenSettings) => "Open System Settings",
        Some(PermissionAction::Restart) => "Restart glance",
        None => "",
    }
}

fn access_symbol(access: Access) -> (&'static str, bool) {
    match access {
        Access::Granted => ("checkmark.circle.fill", true),
        Access::NeedsRestart => ("arrow.clockwise.circle", true),
        Access::NotAsked => ("circle.dashed", false),
        Access::Denied | Access::Unknown => ("xmark.circle", false),
        Access::Restricted => ("lock.circle", false),
    }
}

impl PermissionsPage {
    pub(crate) fn new(ui: &Ui<'_>) -> Self {
        let mtm = ui.mtm;
        let page = Page::new(mtm);
        let (screen, mic) = (Badge::new(mtm), Badge::new(mtm));
        let screen_button = ui.button("", SCREEN_BUTTON, true);
        let mic_button = ui.button("", MIC_BUTTON, true);
        let column = |title: &str, badge: &Badge, reason: &str| {
            let t = ui::label(mtm, title);
            t.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
            ui::stack(
                mtm,
                &[
                    &t,
                    &badge.view,
                    &ui::secondary(mtm, reason, ui::TEXT_W + 40.0),
                ],
                true,
                3.0,
            )
        };
        let screen_col = column(
            "Screen Recording",
            &screen,
            "So glance can see the screen when you hold the hotkey, and only then.",
        );
        let mic_col = column(
            "Microphone",
            &mic,
            "So glance can hear your question while you hold the hotkey, and only then.",
        );
        page.group(
            mtm,
            "glance needs two permissions",
            &[
                ui::row_with(mtm, &screen_col, &screen_button),
                ui::row_with(mtm, &mic_col, &mic_button),
            ],
        );
        let restart_note = body(
            mtm,
            "macOS lets glance use Screen Recording only after it restarts. Restart glance, and \
             setup comes back to this page.",
        );
        page.add(&restart_note);
        let signing_text = body(mtm, "");
        let commands = body(mtm, "");
        commands.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(11.0, 0.0)));
        commands.setSelectable(true);
        let signing_link = ui.button("Signing instructions", SIGNING_LINK, false);
        let before = page.stack.arrangedSubviews().count();
        // One row, so a hidden part leaves no separator behind.
        let link_row = ui::stack(mtm, &[&signing_link], false, 0.0);
        let block = ui::stack(mtm, &[&signing_text, &commands, &link_row], true, 10.0);
        page.group(mtm, "This build", &[view_of(&*block)]);
        let signing_block = page.stack.arrangedSubviews().objectAtIndex(before);
        Self {
            page,
            screen,
            mic,
            screen_button,
            mic_button,
            restart_note,
            signing_block,
            signing_text,
            commands,
            signing_link,
            actions: [None, None],
        }
    }
}

impl StepPage for PermissionsPage {
    fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.page.root.clone())
    }

    fn fill(&mut self, _snap: &SetupSnapshot, env: &Env) {
        for (i, (access, badge, button)) in [
            (env.screen, &self.screen, &self.screen_button),
            (env.mic, &self.mic, &self.mic_button),
        ]
        .into_iter()
        .enumerate()
        {
            let (symbol, accent) = access_symbol(access);
            badge.show(symbol, access.label(), accent);
            let action = access.action();
            button.setTitle(&NSString::from_str(action_title(action)));
            button.setHidden(action.is_none());
            self.actions[i] = action;
        }
        self.restart_note
            .setHidden(env.screen != Access::NeedsRestart);
        match env.signing {
            Signing::AdHoc => {
                set(
                    &self.signing_text,
                    "This build is signed ad hoc, so macOS forgets these permissions after every \
                     rebuild. To keep them, sign glance with your own certificate:",
                );
                set(
                    &self.commands,
                    "security find-identity -v -p codesigning\nexport GLANCE_SIGN_IDENTITY=<its SHA-1>\ncargo xtask bundle",
                );
                self.commands.setHidden(false);
                self.signing_link.setHidden(false);
                self.signing_block.setHidden(false);
            }
            Signing::NotBundled => {
                set(
                    &self.signing_text,
                    "glance is running outside Glance.app, so macOS gives these permissions to \
                     the app that started it (your terminal). Build Glance.app with `cargo xtask \
                     bundle` to give them to glance itself.",
                );
                self.commands.setHidden(true);
                self.signing_link.setHidden(false);
                self.signing_block.setHidden(false);
            }
            Signing::Stable | Signing::Unknown => self.signing_block.setHidden(true),
        }
    }

    fn on_change(&mut self, id: isize, ctx: &mut Ctx<'_>) -> Reply {
        let (which, action) = match id {
            SCREEN_BUTTON => (Which::Screen, self.actions[0]),
            MIC_BUTTON => (Which::Mic, self.actions[1]),
            SIGNING_LINK => {
                // Fixed URL, opened in the user's browser on their click.
                if let Err(e) = std::process::Command::new("/usr/bin/open")
                    .arg(SIGNING_URL)
                    .spawn()
                {
                    return Reply::Say(format!("Could not open the browser: {e}"));
                }
                return Reply::Nothing;
            }
            _ => return Reply::Nothing,
        };
        let Some(action) = action else {
            return Reply::Nothing;
        };
        ctx.perms.act(which, action, ctx.backend);
        Reply::Refresh
    }
}

// ---------------------------------------------------------------- Download

const PRIMARY: isize = 1;
const CANCEL: isize = 2;

struct FileRow {
    key: String,
    name: Retained<NSTextField>,
    state: Retained<NSTextField>,
    bar: Retained<NSProgressIndicator>,
}

pub(crate) struct Download {
    page: Page,
    mtm: MainThreadMarker,
    summary: Retained<NSTextField>,
    overall: Retained<NSProgressIndicator>,
    overall_text: Retained<NSTextField>,
    problem: Badge,
    done: Badge,
    list: Retained<NSStackView>,
    rows: Vec<FileRow>,
    primary: Retained<NSButton>,
    cancel: Retained<NSButton>,
    /// The last refusal from the backend (not enough space, busy).
    refused: Option<DownloadIssue>,
}

/// A determinate bar; `width` fixes its width (inside a group, leave it to
/// the group's full-width rule, or the two fight and shrink the window).
fn bar(mtm: MainThreadMarker, width: Option<f64>, small: bool) -> Retained<NSProgressIndicator> {
    let b = NSProgressIndicator::new(mtm);
    b.setStyle(NSProgressIndicatorStyle::Bar);
    b.setIndeterminate(false);
    b.setMinValue(0.0);
    b.setMaxValue(1.0);
    if small {
        b.setControlSize(NSControlSize::Small);
    }
    ui::auto(&b);
    if let Some(width) = width {
        ui::activate(&[b.widthAnchor().constraintEqualToConstant(width)]);
    }
    b
}

/// One line per file, except models with many small files (the voices),
/// which are one line.
fn grouped(files: &[FileProgress]) -> Vec<(String, String, u64, u64, FileState)> {
    let mut out: Vec<(String, String, u64, u64, FileState)> = Vec::new();
    let count = |model: &str| files.iter().filter(|f| f.model == model).count();
    for f in files {
        if count(&f.model) > 4 {
            let key = f.model.clone();
            if let Some(row) = out.iter_mut().find(|r| r.0 == key) {
                row.2 += f.done;
                row.3 += f.total;
                row.4 = merge(row.4, f.state);
            } else {
                let name = format!("{} files of {}", count(&f.model), f.model);
                out.push((key, name, f.done, f.total, f.state));
            }
        } else {
            let file = f.path.rsplit('/').next().unwrap_or(&f.path);
            out.push((
                format!("{}/{}", f.model, f.path),
                file.to_owned(),
                f.done,
                f.total,
                f.state,
            ));
        }
    }
    out
}

/// The state of a group of files.
fn merge(a: FileState, b: FileState) -> FileState {
    use FileState::*;
    match (a, b) {
        (Downloading, _) | (_, Downloading) => Downloading,
        (Checking, _) | (_, Checking) => Checking,
        (Done, Done) => Done,
        (Done, Waiting) | (Waiting, Done) => Downloading,
        _ => Waiting,
    }
}

impl Download {
    pub(crate) fn new(ui: &Ui<'_>) -> Self {
        let mtm = ui.mtm;
        let page = Page::new(mtm);
        let summary = body(mtm, "");
        let overall = bar(mtm, None, false);
        let overall_text = ui::secondary(mtm, "", TEXT);
        let primary = ui.button("Download", PRIMARY, true);
        let cancel = ui.button("Cancel", CANCEL, false);
        let buttons = ui::stack(mtm, &[&primary, &cancel], false, 8.0);
        let problem = Badge::new(mtm);
        let done = Badge::new(mtm);
        // Hidden badges must not leave separator lines: one row holds them.
        let status = ui::stack(mtm, &[&problem.view, &done.view, &buttons], true, 10.0);
        let progress = ui::stack(mtm, &[&overall, &overall_text], true, 6.0);
        ui::full_width(&overall, &progress, 0.0);
        page.group(
            mtm,
            "",
            &[view_of(&*summary), view_of(&*progress), view_of(&*status)],
        );
        let list = ui::stack(mtm, &[], true, 8.0);
        page.group(mtm, "Files", &[view_of(&*list)]);
        page.add(&ui::secondary(
            mtm,
            "You can close this window: the download keeps going, or carries on where it \
             stopped the next time glance starts. Each file is checked against its pinned \
             SHA-256 before glance uses it.",
            TEXT,
        ));
        Self {
            page,
            mtm,
            summary,
            overall,
            overall_text,
            problem,
            done,
            list,
            rows: Vec::new(),
            primary,
            cancel,
            refused: None,
        }
    }

    fn sync_rows(&mut self, groups: &[(String, String, u64, u64, FileState)]) {
        let same = self.rows.len() == groups.len()
            && self.rows.iter().zip(groups).all(|(r, g)| r.key == g.0);
        if !same {
            for old in self.list.arrangedSubviews().iter() {
                self.list.removeArrangedSubview(&old);
                old.removeFromSuperview();
            }
            self.rows.clear();
            for g in groups {
                let name = ui::label(self.mtm, &g.1);
                name.setToolTip(Some(&NSString::from_str(&g.0)));
                let state = ui::label(self.mtm, "");
                state.setFont(Some(&NSFont::systemFontOfSize(11.0)));
                state.setTextColor(Some(&NSColor::secondaryLabelColor()));
                state.setAlignment(objc2_app_kit::NSTextAlignment::Right);
                ui::activate(&[state.widthAnchor().constraintEqualToConstant(64.0)]);
                let b = bar(self.mtm, Some(140.0), true);
                let right = ui::stack(self.mtm, &[&state, &b], false, 8.0);
                let row = ui::row_with(self.mtm, &name, &right);
                self.list.addArrangedSubview(&row);
                ui::full_width(&row, &self.list, 0.0);
                self.rows.push(FileRow {
                    key: g.0.clone(),
                    name,
                    state,
                    bar: b,
                });
            }
        }
        for (row, g) in self.rows.iter().zip(groups) {
            set(&row.name, &g.1);
            let fraction = g.2 as f64 / g.3.max(1) as f64;
            row.bar.setDoubleValue(fraction);
            set(
                &row.state,
                &match g.4 {
                    FileState::Waiting if g.2 > 0 => format!("{:.0}% kept", fraction * 100.0),
                    FileState::Waiting => "Waiting".to_owned(),
                    FileState::Downloading => format!("{:.0}%", fraction * 100.0),
                    FileState::Checking => "Checking".to_owned(),
                    FileState::Done => "✓ Done".to_owned(),
                },
            );
        }
    }
}

impl StepPage for Download {
    fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.page.root.clone())
    }

    fn fill(&mut self, snap: &SetupSnapshot, _env: &Env) {
        let chosen = snap.presets.iter().find(|p| p.id == snap.chosen);
        let under_way = snap.download.as_ref().is_some_and(|d| {
            !matches!(d.phase, DownloadPhase::Finished | DownloadPhase::Cancelled)
        });
        if let Some(p) = chosen {
            set(
                &self.summary,
                &if under_way {
                    format!("{}: {} from huggingface.co.", p.name, gb(p.total_bytes))
                } else if p.missing_bytes == 0 {
                    format!(
                        "{}: {} in total, all on this Mac.",
                        p.name,
                        gb(p.total_bytes)
                    )
                } else {
                    format!(
                        "{}: {} in total, {} still to download from huggingface.co.",
                        p.name,
                        gb(p.total_bytes),
                        gb(p.missing_bytes)
                    )
                },
            );
        }
        let phase = snap.download.as_ref().map(|d| d.phase.clone());
        let running = phase == Some(DownloadPhase::Running);
        match &snap.download {
            Some(d) => {
                self.overall
                    .setDoubleValue(d.done as f64 / d.total.max(1) as f64);
                let speed = speed_line(d.bytes_per_sec, d.remaining);
                let mut line = format!("{} of {}", gb(d.done), gb(d.total));
                match &d.phase {
                    DownloadPhase::Running if !speed.is_empty() => line += &format!(" · {speed}"),
                    DownloadPhase::Paused => line += " · paused",
                    DownloadPhase::Cancelled => line += " · cancelled",
                    _ => {}
                }
                set(&self.overall_text, &line);
                self.sync_rows(&grouped(&d.files));
            }
            None => {
                let (done, total) =
                    chosen.map_or((0, 0), |p| (p.total_bytes - p.missing_bytes, p.total_bytes));
                self.overall
                    .setDoubleValue(done as f64 / total.max(1) as f64);
                set(
                    &self.overall_text,
                    &format!("{} of {} on this Mac", gb(done), gb(total)),
                );
                self.sync_rows(&[]);
            }
        }
        // Problems: a failed download, a refusal, or no room for it.
        let issue = match &phase {
            Some(DownloadPhase::Failed(issue)) => Some(issue.clone()),
            _ if running => None,
            _ => self.refused.clone().or_else(|| {
                chosen
                    .filter(|p| !p.fits_disk && !snap.models_ready)
                    .map(|p| DownloadIssue::NotEnoughSpace {
                        needed: p.missing_bytes + 2_000_000_000,
                        free: snap.machine.free_bytes.unwrap_or(0),
                    })
            }),
        };
        match &issue {
            Some(issue) => self
                .problem
                .show("exclamationmark.triangle", &issue.plain(), true),
            None => self.problem.hide(),
        }
        if snap.models_ready {
            self.done.show(
                "checkmark.seal",
                "All models are downloaded and checked against their pins.",
                true,
            );
            self.primary.setHidden(true);
            self.cancel.setHidden(true);
            self.refused = None;
            return;
        }
        self.done.hide();
        let (title, cancel) = match &phase {
            Some(DownloadPhase::Running) => ("Pause", true),
            Some(DownloadPhase::Paused) => ("Resume", true),
            Some(DownloadPhase::Failed(_)) => ("Try again", true),
            _ => ("Download", false),
        };
        self.primary.setTitle(&NSString::from_str(title));
        self.primary.setHidden(false);
        self.primary
            .setEnabled(running || !matches!(issue, Some(DownloadIssue::NotEnoughSpace { .. })));
        self.cancel.setHidden(!cancel);
    }

    fn on_change(&mut self, id: isize, ctx: &mut Ctx<'_>) -> Reply {
        let phase = ctx.snap.download.as_ref().map(|d| d.phase.clone());
        let result = match (id, phase) {
            (PRIMARY, Some(DownloadPhase::Running)) => {
                ctx.backend.pause();
                Ok(())
            }
            (PRIMARY, Some(DownloadPhase::Paused | DownloadPhase::Failed(_))) => {
                ctx.backend.resume()
            }
            (PRIMARY, _) => ctx.backend.download(),
            (CANCEL, _) => {
                if super::super::settings::confirm(
                    self.mtm,
                    "Cancel the download?",
                    "What has been downloaded so far is deleted. Pause keeps it instead.",
                    "Cancel download",
                ) {
                    ctx.backend.cancel();
                }
                Ok(())
            }
            _ => Ok(()),
        };
        self.refused = result.err();
        Reply::Refresh
    }
}

// ---------------------------------------------------------------- Try it

const TEST: isize = 1;

pub(crate) struct TryIt {
    page: Page,
    mtm: MainThreadMarker,
    caps: [Caps; 3],
    test: Retained<NSButton>,
    result: Badge,
    listening: bool,
}

impl TryIt {
    pub(crate) fn new(ui: &Ui<'_>) -> Self {
        let mtm = ui.mtm;
        let page = Page::new(mtm);
        let caps = [Caps::new(mtm), Caps::new(mtm), Caps::new(mtm)];
        let names = [
            ("Ask about the screen", "Hold, speak, let go."),
            (
                "Take a fresh look and ask",
                "Like Ask, with a new screenshot.",
            ),
            (
                "Think harder",
                "Asks the bigger model (with the think harder setup).",
            ),
        ];
        let rows: Vec<Retained<NSView>> = names
            .iter()
            .zip(&caps)
            .map(|((name, detail), caps)| ui::row(mtm, name, Some(detail), false, &caps.holder))
            .collect();
        page.group(mtm, "Hotkeys", &rows);
        page.add(&body(
            mtm,
            "Push to talk: hold the keys while you speak, and let go to hear the answer.",
        ));
        let test = ui.button("Run a test question", TEST, true);
        let text = ui::stack(
            mtm,
            &[
                &ui::label(mtm, "Test question"),
                &ui::secondary(
                    mtm,
                    "Uses a built-in picture of a chart, not your screen. You read a question \
                     aloud; glance answers out loud.",
                    ui::TEXT_W + 60.0,
                ),
            ],
            true,
            2.0,
        );
        let result = Badge::new(mtm);
        page.group(
            mtm,
            "Try it",
            &[ui::row_with(mtm, &text, &test), view_of(&*result.view)],
        );
        Self {
            page,
            mtm,
            caps,
            test,
            result,
            listening: false,
        }
    }
}

impl StepPage for TryIt {
    fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.page.root.clone())
    }

    fn fill(&mut self, snap: &SetupSnapshot, _env: &Env) {
        for (caps, chord) in self.caps.iter_mut().zip(&snap.hotkeys) {
            caps.set(self.mtm, chord);
        }
        self.listening = snap.test == TestResult::Listening;
        let (button, enabled) = match &snap.test {
            TestResult::Idle => ("Run a test question", true),
            TestResult::Listening => ("Stop", true),
            TestResult::Starting | TestResult::Working => ("Run a test question", false),
            TestResult::Works { .. } => ("Run it again", true),
            TestResult::Failed { .. } => ("Try again", true),
        };
        self.test.setTitle(&NSString::from_str(button));
        self.test.setEnabled(enabled);
        match &snap.test {
            TestResult::Idle => self.result.hide(),
            TestResult::Starting => self.result.show("hourglass", "Starting the models…", false),
            TestResult::Listening => self.result.show(
                "mic.fill",
                &format!("Listening. Say “{TEST_QUESTION}”, then press Stop."),
                true,
            ),
            TestResult::Working => self.result.show("ellipsis.circle", "Thinking…", false),
            TestResult::Works { first_word } => {
                self.result
                    .show("checkmark.circle.fill", &works_line(*first_word), true)
            }
            TestResult::Failed { stage, hint } => self.result.show(
                "xmark.octagon",
                &format!("{} did not work. {hint}", stage.label()),
                true,
            ),
        }
    }

    fn on_change(&mut self, id: isize, ctx: &mut Ctx<'_>) -> Reply {
        if id != TEST {
            return Reply::Nothing;
        }
        if self.listening {
            ctx.backend.stop_test();
            return Reply::Refresh;
        }
        match ctx.backend.start_test() {
            Ok(()) => Reply::Refresh,
            Err(e) => Reply::Say(e),
        }
    }
}

// ---------------------------------------------------------------- Done

pub(crate) struct Done {
    page: Page,
    mtm: MainThreadMarker,
    setup: Retained<NSTextField>,
    screen: Retained<NSTextField>,
    mic: Retained<NSTextField>,
    voice: Retained<NSTextField>,
    caps: Caps,
    note: Badge,
}

impl Done {
    pub(crate) fn new(ui: &Ui<'_>) -> Self {
        let mtm = ui.mtm;
        let page = Page::new(mtm);
        let value = || {
            let l = ui::label(mtm, "");
            l.setTextColor(Some(&NSColor::secondaryLabelColor()));
            l
        };
        let (setup, screen, mic, voice) = (value(), value(), value(), value());
        let caps = Caps::new(mtm);
        page.group(
            mtm,
            "Summary",
            &[
                ui::row(mtm, "Setup", None, false, &setup),
                ui::row(mtm, "Screen Recording", None, false, &screen),
                ui::row(mtm, "Microphone", None, false, &mic),
                ui::row(mtm, "Voice", None, false, &voice),
                ui::row(mtm, "Ask about the screen", None, false, &caps.holder),
            ],
        );
        page.add(&body(
            mtm,
            "glance lives in the menu bar. Hold the hotkey any time to ask about your screen. \
             Settings has the voice, answer length, hotkeys and models.",
        ));
        let note = Badge::new(mtm);
        page.add(&note.view);
        Self {
            page,
            mtm,
            setup,
            screen,
            mic,
            voice,
            caps,
            note,
        }
    }
}

impl StepPage for Done {
    fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.page.root.clone())
    }

    fn fill(&mut self, snap: &SetupSnapshot, env: &Env) {
        let name = snap
            .presets
            .iter()
            .find(|p| p.id == snap.chosen)
            .map_or(snap.chosen.as_str(), |p| p.name.as_str());
        set(&self.setup, name);
        set(&self.screen, env.screen.label());
        set(&self.mic, env.mic.label());
        set(&self.voice, &snap.voice);
        self.caps.set(self.mtm, &snap.hotkeys[0]);
        if !snap.models_ready {
            self.note.show(
                "exclamationmark.triangle",
                "The models are not all downloaded yet. Go back to Download models first.",
                true,
            );
        } else if snap.needs_restart {
            self.note.show(
                "arrow.clockwise.circle",
                "glance restarts to use the models you chose.",
                false,
            );
        } else {
            self.note.hide();
        }
    }
}

// ---------------------------------------------------------------- All set

pub(crate) struct AllSet {
    page: Page,
    mtm: MainThreadMarker,
    caps: Caps,
}

impl AllSet {
    pub(crate) fn new(ui: &Ui<'_>) -> Self {
        let mtm = ui.mtm;
        let page = Page::new(mtm);
        let badge = Badge::new(mtm);
        badge.show(
            "checkmark.seal",
            "Everything glance needs is already here: Screen Recording and Microphone are \
             allowed, and the models are downloaded.",
            true,
        );
        page.group(mtm, "", &[view_of(&*badge.view)]);
        let caps = Caps::new(mtm);
        page.group(
            mtm,
            "",
            &[ui::row(
                mtm,
                "Ask about the screen",
                Some("Hold, speak, let go."),
                false,
                &caps.holder,
            )],
        );
        Self { page, mtm, caps }
    }
}

impl StepPage for AllSet {
    fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.page.root.clone())
    }

    fn fill(&mut self, snap: &SetupSnapshot, _env: &Env) {
        self.caps.set(self.mtm, &snap.hotkeys[0]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_and_time_read_naturally() {
        use std::time::Duration;
        assert_eq!(speed_line(None, None), "");
        assert_eq!(
            speed_line(Some(38_400_000.0), Some(Duration::from_secs(95))),
            "38 MB/s · about 2 min left"
        );
        assert_eq!(
            speed_line(Some(1e6), Some(Duration::from_secs(20))),
            "1 MB/s · less than a minute left"
        );
        assert_eq!(
            speed_line(None, Some(Duration::from_secs(4000))),
            "about 1 h 6 min left"
        );
    }

    #[test]
    fn many_small_files_are_one_line() {
        let file = |model: &str, path: &str, done, state| FileProgress {
            model: model.into(),
            path: path.into(),
            done,
            total: 10,
            state,
        };
        let mut files = vec![file("fast", "w.gguf", 4, FileState::Downloading)];
        for i in 0..6 {
            let state = if i < 2 {
                FileState::Done
            } else {
                FileState::Waiting
            };
            files.push(file(
                "voices",
                &format!("voices/v{i}.bin"),
                if i < 2 { 10 } else { 0 },
                state,
            ));
        }
        let rows = grouped(&files);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].1, "w.gguf");
        assert_eq!(rows[1].1, "6 files of voices");
        assert_eq!((rows[1].2, rows[1].3), (20, 60));
        assert_eq!(rows[1].4, FileState::Downloading);
    }
}
