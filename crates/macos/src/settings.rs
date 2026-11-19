//! The settings window (native AppKit): voice, answer length, think-harder
//! options, models and hotkeys. Everything else stays in `config.toml`.
//!
//! The window only shows and edits plain values; reading and saving them
//! is the app's [`SettingsBackend`](glance_orchestrator::settings::SettingsBackend).
//! How changes apply (maintainer, 2026-11-18):
//! - voice, speed, answer length, think-harder effort and idle time: live;
//! - hotkeys: live, but only if every new chord registers; otherwise the old
//!   chords stay and the window says why;
//! - models (and DEEP's thinking cap and auto-escalation): "Apply and
//!   restart".
//!
//! Main thread only. `unsafe` here: AppKit's target/action wiring (a control
//! keeps its target as a weak reference, so the window owns the target) and
//! `setReleasedWhenClosed`.
#![allow(unsafe_code)]

use std::cell::RefCell;

use dispatch2::DispatchQueue;
use glance_orchestrator::ResponseLength;
use glance_orchestrator::settings::{Applied, SettingsChange, SettingsChoices, SettingsValues};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSButton, NSColor, NSControl, NSControlStateValueOff,
    NSControlStateValueOn, NSFont, NSPanel, NSPopUpButton, NSSlider, NSTextField, NSView,
    NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use crate::hotkeys::{self, Chords};

const WIDTH: f64 = 540.0;
const HEIGHT: f64 = 900.0;
const LABEL_X: f64 = 24.0;
const LABEL_W: f64 = 160.0;
const FIELD_X: f64 = 190.0;
const FIELD_W: f64 = 326.0;
const ROW: f64 = 30.0;

const TAG_ENGINE: isize = 1;
const TAG_VOICE: isize = 2;
const TAG_SPEED: isize = 3;
const TAG_LENGTH: isize = 4;
const TAG_THINK: isize = 5;
const TAG_APPLY_MODELS: isize = 6;
const TAG_APPLY_HOTKEYS: isize = 7;
const TAG_MANAGE_MODELS: isize = 8;
const TAG_PERSONALITY: isize = 9;
const TAG_MATH_WORDS: isize = 10;

/// Preset names as saved, and as shown.
const PERSONALITIES: [(&str, &str); 8] = [
    ("neutral", "Neutral (as before)"),
    ("friendly", "Friendly"),
    ("direct", "Direct"),
    ("strict", "Strict"),
    ("pirate", "Pirate (fun)"),
    ("caveman", "Caveman (fun)"),
    ("shakespearean", "Shakespearean (fun)"),
    ("custom", "Custom"),
];

const ENGINES: [(&str, &str); 2] = [("kokoro", "Kokoro"), ("system", "macOS system voice")];
const EFFORTS: [&str; 4] = ["off", "low", "medium", "xhigh"];
const IDLE_MINS: [u64; 5] = [5, 10, 20, 30, 60];
const BUDGETS: [Option<u32>; 4] = [Some(64), Some(128), Some(256), None];
const LENGTHS: [ResponseLength; 3] = [
    ResponseLength::Short,
    ResponseLength::Medium,
    ResponseLength::Long,
];

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `Target` has no state and does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GlanceSettingsTarget"]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    impl Target {
        #[unsafe(method(changed:))]
        fn changed(&self, sender: &NSControl) {
            let tag = sender.tag();
            // Not inside the action: the backend may take a moment.
            DispatchQueue::main().exec_async(move || on_change(tag));
        }
    }
);

impl Target {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: plain `-[NSObject init]` on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}

struct Window {
    panel: Retained<NSPanel>,
    /// Controls hold their target weakly; the window keeps it alive.
    _target: Retained<Target>,
    engine: Retained<NSPopUpButton>,
    voice: Retained<NSPopUpButton>,
    speed: Retained<NSSlider>,
    speed_label: Retained<NSTextField>,
    math_words: Retained<NSButton>,
    length: Retained<NSSlider>,
    length_label: Retained<NSTextField>,
    personality: Retained<NSPopUpButton>,
    personality_custom: Retained<NSTextField>,
    effort: Retained<NSPopUpButton>,
    idle: Retained<NSPopUpButton>,
    fast: Retained<NSPopUpButton>,
    deep: Retained<NSPopUpButton>,
    budget: Retained<NSPopUpButton>,
    auto_escalate: Retained<NSButton>,
    hotkeys: [Retained<NSTextField>; 3],
    status: Retained<NSTextField>,
    choices: SettingsChoices,
    /// The voice for each engine, so switching engines keeps both.
    kokoro_voice: String,
    system_voice: Option<String>,
    /// The model lists as shown (configured ones first if not downloaded).
    fast_items: Vec<String>,
    deep_items: Vec<Option<String>>,
}

thread_local! {
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
}

/// Show the window (tray menu). Main thread.
pub fn open() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(backend) = crate::shell::settings_backend() else {
        return;
    };
    let (values, choices) = backend.current();
    WINDOW.with_borrow_mut(|window| {
        if window.is_none() {
            *window = Some(Window::build(mtm, choices.clone()));
        }
        let Some(w) = window.as_mut() else { return };
        w.fill(&values, choices);
        w.panel.center();
        w.panel.makeKeyAndOrderFront(None);
    });
    // An accessory app must activate for its window to take typing.
    NSApplication::sharedApplication(mtm).activate();
}

fn on_change(tag: isize) {
    let Some(backend) = crate::shell::settings_backend() else {
        return;
    };
    WINDOW.with_borrow_mut(|window| {
        let Some(w) = window.as_mut() else { return };
        let result = match tag {
            TAG_ENGINE => {
                w.fill_voices();
                Some(backend.apply(w.voice_change()))
            }
            TAG_VOICE | TAG_SPEED => {
                w.show_speed();
                Some(backend.apply(w.voice_change()))
            }
            TAG_LENGTH => {
                w.show_length();
                Some(backend.apply(SettingsChange::Length(w.length())))
            }
            TAG_MATH_WORDS => Some(backend.apply(SettingsChange::MathAsWords(
                w.math_words.state() == NSControlStateValueOn,
            ))),
            TAG_PERSONALITY => {
                let preset = PERSONALITIES[index(&w.personality, PERSONALITIES.len())].0;
                let custom = w.personality_custom.stringValue().to_string();
                w.personality_custom.setEnabled(preset == "custom");
                if preset == "custom" && custom.trim().is_empty() {
                    w.say("Type a style for Custom, then press Return.");
                    None
                } else {
                    Some(backend.apply(SettingsChange::Personality {
                        preset: preset.to_owned(),
                        custom,
                    }))
                }
            }
            TAG_THINK => Some(backend.apply(SettingsChange::Think {
                effort: EFFORTS[index(&w.effort, EFFORTS.len())].to_owned(),
                idle_mins: IDLE_MINS[index(&w.idle, IDLE_MINS.len())],
            })),
            TAG_APPLY_MODELS => {
                let change = SettingsChange::Models {
                    fast: w.fast_items[index(&w.fast, w.fast_items.len())].clone(),
                    deep: w.deep_items[index(&w.deep, w.deep_items.len())].clone(),
                    auto_escalate: w.auto_escalate.state() == NSControlStateValueOn,
                    thinking_budget: BUDGETS[index(&w.budget, BUDGETS.len())],
                };
                let result = backend.apply(change);
                if let Ok(Applied::NeedsRestart) = result {
                    w.say("Saved. Restarting glance…");
                    DispatchQueue::main().exec_async(crate::shell::relaunch);
                    return;
                }
                Some(result)
            }
            TAG_APPLY_HOTKEYS => {
                let text = |i: usize| w.hotkeys[i].stringValue().to_string().trim().to_owned();
                let chords = Chords {
                    talk: text(0),
                    look: text(1),
                    think: text(2),
                };
                match hotkeys::rebind(&chords) {
                    Ok(()) => Some(backend.apply(SettingsChange::Hotkeys([
                        chords.talk,
                        chords.look,
                        chords.think,
                    ]))),
                    Err(e) => {
                        // The old chords stay; show them again.
                        let labels = hotkeys::labels();
                        for (field, label) in
                            w.hotkeys
                                .iter()
                                .zip([labels.talk, labels.look, labels.think])
                        {
                            field.setStringValue(&NSString::from_str(&label));
                        }
                        Some(Err(e))
                    }
                }
            }
            TAG_MANAGE_MODELS => {
                DispatchQueue::main().exec_async(crate::models_window::open);
                None
            }
            _ => None,
        };
        match result {
            Some(Ok(Applied::Live)) => w.say("Saved and applied."),
            Some(Ok(Applied::NeedsRestart)) => {
                w.say("Saved. This takes effect when glance restarts.");
            }
            Some(Err(e)) => w.say(&e),
            None => {}
        }
    });
}

impl Window {
    fn build(mtm: MainThreadMarker, choices: SettingsChoices) -> Self {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, HEIGHT));
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            frame,
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setTitle(&NSString::from_str("glance settings"));
        panel.setHidesOnDeactivate(false);
        // SAFETY: the window is owned by `WINDOW` for the app's lifetime; it
        // must not free itself when closed.
        unsafe { panel.setReleasedWhenClosed(false) };
        let view = NSView::initWithFrame(NSView::alloc(mtm), frame);
        panel.setContentView(Some(&view));
        let target = Target::new(mtm);

        let mut y = HEIGHT - 44.0;
        let header = |text: &str, y: &mut f64| {
            *y -= 8.0;
            let l = label(mtm, text, LABEL_X, *y, FIELD_W + LABEL_W);
            l.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
            view.addSubview(&l);
            *y -= ROW;
        };
        let row = |text: &str, y: &mut f64| {
            view.addSubview(&label(mtm, text, LABEL_X, *y, LABEL_W));
            let at = *y;
            *y -= ROW;
            at
        };

        header("Voice", &mut y);
        let at = row("Engine", &mut y);
        let engine = popup(
            mtm,
            &ENGINES.map(|(_, l)| l.to_owned()),
            at,
            &target,
            TAG_ENGINE,
        );
        view.addSubview(&engine);
        let at = row("Voice", &mut y);
        let voice = popup(mtm, &[], at, &target, TAG_VOICE);
        view.addSubview(&voice);
        let at = row("Speed (Kokoro)", &mut y);
        let speed = slider(mtm, 0.5, 2.0, None, at, &target, TAG_SPEED);
        let speed_label = label(mtm, "", FIELD_X + FIELD_W - 50.0, at, 50.0);
        view.addSubview(&speed);
        view.addSubview(&speed_label);
        let at = row("Maths", &mut y);
        // SAFETY: no target yet; `connect` sets it under its rules.
        let math_words = unsafe {
            NSButton::checkboxWithTitle_target_action(
                &NSString::from_str(
                    "Say symbols as words: 2 × 2 = 4 is \u{201c}2 times 2 equals 4\u{201d}",
                ),
                None,
                None,
                mtm,
            )
        };
        math_words.setFrame(rect(FIELD_X, at, FIELD_W + 10.0));
        connect(&math_words, &target, TAG_MATH_WORDS);
        view.addSubview(&math_words);

        header("Answers", &mut y);
        let at = row("Length", &mut y);
        let length = slider(mtm, 0.0, 2.0, Some(3), at, &target, TAG_LENGTH);
        let length_label = label(mtm, "", FIELD_X + FIELD_W - 70.0, at, 70.0);
        view.addSubview(&length);
        view.addSubview(&length_label);
        view.addSubview(&note(
            mtm,
            "Short, medium or long; \"walk me through\" or \"in detail\" always gets the full answer.",
            &mut y,
        ));
        let at = row("Personality", &mut y);
        let personality = popup(
            mtm,
            &PERSONALITIES.map(|(_, label)| label.to_owned()),
            at,
            &target,
            TAG_PERSONALITY,
        );
        view.addSubview(&personality);
        let at = row("Custom style", &mut y);
        let personality_custom = NSTextField::textFieldWithString(&NSString::from_str(""), mtm);
        personality_custom.setPlaceholderString(Some(&NSString::from_str(
            "e.g. Calm and patient, like a good teacher",
        )));
        personality_custom.setFrame(rect(FIELD_X, at, FIELD_W));
        connect(&personality_custom, &target, TAG_PERSONALITY);
        view.addSubview(&personality_custom);
        view.addSubview(&note(
            mtm,
            "Only how answers sound, never how long. Custom: up to 300 characters, press Return.",
            &mut y,
        ));

        header("Think harder", &mut y);
        let at = row("Reasoning effort", &mut y);
        let effort = popup(mtm, &EFFORTS.map(str::to_owned), at, &target, TAG_THINK);
        view.addSubview(&effort);
        let at = row("Unload DEEP after", &mut y);
        let idle = popup(
            mtm,
            &IDLE_MINS.map(|m| format!("{m} minutes idle")),
            at,
            &target,
            TAG_THINK,
        );
        view.addSubview(&idle);

        header("Models (apply and restart)", &mut y);
        let at = row("FAST model", &mut y);
        let fast = popup(mtm, &[], at, &target, 0);
        view.addSubview(&fast);
        let at = row("DEEP model", &mut y);
        let deep = popup(mtm, &[], at, &target, 0);
        view.addSubview(&deep);
        let at = row("DEEP thinking cap", &mut y);
        let budget = popup(
            mtm,
            &BUDGETS.map(|b| b.map_or("No cap".to_owned(), |n| format!("{n} tokens"))),
            at,
            &target,
            0,
        );
        view.addSubview(&budget);
        let at = row("", &mut y);
        // SAFETY: `target` outlives the button (both owned by the window).
        let auto_escalate = unsafe {
            NSButton::checkboxWithTitle_target_action(
                &NSString::from_str("FAST may hand hard questions to DEEP by itself"),
                None,
                None,
                mtm,
            )
        };
        auto_escalate.setFrame(rect(FIELD_X, at, FIELD_W));
        view.addSubview(&auto_escalate);
        let at = row("", &mut y);
        let apply_models = button(mtm, "Apply and restart", at, &target, TAG_APPLY_MODELS);
        view.addSubview(&apply_models);
        let manage = button(mtm, "Manage models…", at, &target, TAG_MANAGE_MODELS);
        manage.setFrame(rect(FIELD_X + 210.0, at, 150.0));
        view.addSubview(&manage);

        header("Hotkeys", &mut y);
        let field = |name: &str, y: &mut f64| {
            let at = row(name, y);
            let f = NSTextField::textFieldWithString(&NSString::from_str(""), mtm);
            f.setFrame(rect(FIELD_X, at, 200.0));
            view.addSubview(&f);
            f
        };
        let hotkeys = [
            field("Ask (hold)", &mut y),
            field("Fresh look (hold)", &mut y),
            field("Think harder", &mut y),
        ];
        let at = row("", &mut y);
        let apply_hotkeys = button(mtm, "Apply hotkeys", at, &target, TAG_APPLY_HOTKEYS);
        view.addSubview(&apply_hotkeys);

        let status = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
        status.setTextColor(Some(&NSColor::secondaryLabelColor()));
        status.setFrame(NSRect::new(
            NSPoint::new(LABEL_X, 14.0),
            NSSize::new(WIDTH - 2.0 * LABEL_X, 36.0),
        ));
        view.addSubview(&status);

        Self {
            panel,
            _target: target,
            engine,
            voice,
            speed,
            speed_label,
            math_words,
            length,
            length_label,
            personality,
            personality_custom,
            effort,
            idle,
            fast,
            deep,
            budget,
            auto_escalate,
            hotkeys,
            status,
            choices,
            kokoro_voice: String::new(),
            system_voice: None,
            fast_items: Vec::new(),
            deep_items: Vec::new(),
        }
    }

    /// Put the current values into the controls.
    fn fill(&mut self, values: &SettingsValues, choices: SettingsChoices) {
        self.choices = choices;
        self.kokoro_voice = values.kokoro_voice.clone();
        self.system_voice = values.system_voice.clone();
        let engine = ENGINES
            .iter()
            .position(|(e, _)| *e == values.engine)
            .unwrap_or(0);
        self.engine.selectItemAtIndex(engine as isize);
        self.fill_voices();
        self.speed.setDoubleValue(f64::from(values.speed));
        self.show_speed();
        self.math_words.setState(if values.math_as_words {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
        let length = LENGTHS
            .iter()
            .position(|l| *l == values.length)
            .unwrap_or(1);
        self.length.setDoubleValue(length as f64);
        self.show_length();
        let personality = PERSONALITIES
            .iter()
            .position(|(p, _)| *p == values.personality)
            .unwrap_or(0);
        self.personality.selectItemAtIndex(personality as isize);
        self.personality_custom
            .setStringValue(&NSString::from_str(&values.personality_custom));
        self.personality_custom
            .setEnabled(values.personality == "custom");
        let effort = EFFORTS
            .iter()
            .position(|e| *e == values.deep_effort)
            .unwrap_or(2);
        self.effort.selectItemAtIndex(effort as isize);
        let idle = IDLE_MINS
            .iter()
            .position(|m| *m == values.deep_idle_mins)
            .unwrap_or(1);
        self.idle.selectItemAtIndex(idle as isize);

        self.fast_items = self.choices.models.clone();
        if !self.fast_items.contains(&values.fast_model) {
            self.fast_items.insert(0, values.fast_model.clone());
        }
        set_items(&self.fast, &self.fast_items);
        select(&self.fast, &self.fast_items, &values.fast_model);
        self.deep_items = std::iter::once(None)
            .chain(self.choices.models.iter().cloned().map(Some))
            .collect();
        if !self.deep_items.contains(&values.deep_model) {
            self.deep_items.insert(1, values.deep_model.clone());
        }
        let deep_titles: Vec<String> = self
            .deep_items
            .iter()
            .map(|d| {
                d.clone()
                    .unwrap_or_else(|| "None (no think harder)".to_owned())
            })
            .collect();
        set_items(&self.deep, &deep_titles);
        let deep = self
            .deep_items
            .iter()
            .position(|d| *d == values.deep_model)
            .unwrap_or(0);
        self.deep.selectItemAtIndex(deep as isize);
        let budget = BUDGETS
            .iter()
            .position(|b| *b == values.deep_thinking_budget)
            .unwrap_or(1);
        self.budget.selectItemAtIndex(budget as isize);
        self.auto_escalate.setState(if values.auto_escalate {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
        for (field, value) in self.hotkeys.iter().zip(&values.hotkeys) {
            field.setStringValue(&NSString::from_str(value));
        }
        self.say("");
    }

    fn is_kokoro(&self) -> bool {
        index(&self.engine, ENGINES.len()) == 0
    }

    /// The voice list for the selected engine, with its voice selected.
    fn fill_voices(&self) {
        if self.is_kokoro() {
            let mut items = self.choices.kokoro_voices.clone();
            if !items.contains(&self.kokoro_voice) {
                items.insert(0, self.kokoro_voice.clone());
            }
            set_items(&self.voice, &items);
            select(&self.voice, &items, &self.kokoro_voice);
        } else {
            let mut titles = vec!["System default".to_owned()];
            titles.extend(
                self.choices
                    .system_voices
                    .iter()
                    .map(|(_, label)| label.clone()),
            );
            set_items(&self.voice, &titles);
            let at = self.system_voice.as_ref().and_then(|wanted| {
                self.choices
                    .system_voices
                    .iter()
                    .position(|(id, _)| id == wanted)
            });
            self.voice
                .selectItemAtIndex(at.map_or(0, |i| i as isize + 1));
        }
        self.speed.setEnabled(self.is_kokoro());
    }

    fn voice_change(&mut self) -> SettingsChange {
        let engine = ENGINES[index(&self.engine, ENGINES.len())].0.to_owned();
        if self.is_kokoro() {
            if let Some(title) = self.voice.titleOfSelectedItem() {
                self.kokoro_voice = title.to_string();
            }
        } else {
            let i = self.voice.indexOfSelectedItem();
            self.system_voice = (i > 0)
                .then(|| self.choices.system_voices.get(i as usize - 1))
                .flatten()
                .map(|(id, _)| id.clone());
        }
        SettingsChange::Voice {
            engine,
            kokoro_voice: self.kokoro_voice.clone(),
            system_voice: self.system_voice.clone(),
            speed: self.speed_value(),
        }
    }

    fn speed_value(&self) -> f32 {
        ((self.speed.doubleValue() * 20.0).round() / 20.0) as f32
    }

    fn show_speed(&self) {
        self.speed_label
            .setStringValue(&NSString::from_str(&format!("{:.2}×", self.speed_value())));
    }

    fn length(&self) -> ResponseLength {
        LENGTHS[(self.length.doubleValue().round() as usize).min(2)]
    }

    fn show_length(&self) {
        let name = match self.length() {
            ResponseLength::Short => "Short",
            ResponseLength::Medium => "Medium",
            ResponseLength::Long => "Long",
        };
        self.length_label.setStringValue(&NSString::from_str(name));
    }

    fn say(&self, text: &str) {
        self.status.setStringValue(&NSString::from_str(text));
    }
}

fn rect(x: f64, y: f64, w: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, 24.0))
}

fn label(mtm: MainThreadMarker, text: &str, x: f64, y: f64, w: f64) -> Retained<NSTextField> {
    let l = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    l.setFrame(NSRect::new(NSPoint::new(x, y + 3.0), NSSize::new(w, 18.0)));
    l
}

fn note(mtm: MainThreadMarker, text: &str, y: &mut f64) -> Retained<NSTextField> {
    let l = NSTextField::wrappingLabelWithString(&NSString::from_str(text), mtm);
    l.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    l.setTextColor(Some(&NSColor::secondaryLabelColor()));
    l.setFrame(NSRect::new(
        NSPoint::new(FIELD_X, *y - 4.0),
        NSSize::new(FIELD_W, 28.0),
    ));
    *y -= ROW;
    l
}

/// Wire a control to the window's target with `tag` (0: no action).
fn connect(control: &NSControl, target: &Target, tag: isize) {
    if tag == 0 {
        return;
    }
    control.setTag(tag);
    let target: &AnyObject = target;
    // SAFETY: `target` is owned by the window, which also owns every
    // control, so it outlives them; `changed:` takes one object argument.
    unsafe {
        control.setTarget(Some(target));
        control.setAction(Some(sel!(changed:)));
    }
}

fn popup(
    mtm: MainThreadMarker,
    items: &[String],
    y: f64,
    target: &Target,
    tag: isize,
) -> Retained<NSPopUpButton> {
    let p = NSPopUpButton::initWithFrame_pullsDown(
        NSPopUpButton::alloc(mtm),
        rect(FIELD_X, y, FIELD_W),
        false,
    );
    set_items(&p, items);
    connect(&p, target, tag);
    p
}

fn slider(
    mtm: MainThreadMarker,
    min: f64,
    max: f64,
    ticks: Option<isize>,
    y: f64,
    target: &Target,
    tag: isize,
) -> Retained<NSSlider> {
    let width = FIELD_W - 80.0;
    let s = NSSlider::initWithFrame(NSSlider::alloc(mtm), rect(FIELD_X, y, width));
    s.setMinValue(min);
    s.setMaxValue(max);
    if let Some(ticks) = ticks {
        s.setNumberOfTickMarks(ticks);
        s.setAllowsTickMarkValuesOnly(true);
    }
    s.setContinuous(false);
    connect(&s, target, tag);
    s
}

fn button(
    mtm: MainThreadMarker,
    title: &str,
    y: f64,
    target: &Target,
    tag: isize,
) -> Retained<NSButton> {
    // SAFETY: the target is set by `connect` below, under its rules.
    let b = unsafe {
        NSButton::buttonWithTitle_target_action(&NSString::from_str(title), None, None, mtm)
    };
    b.setFrame(rect(FIELD_X, y, 200.0));
    connect(&b, target, tag);
    b
}

fn set_items(popup: &NSPopUpButton, items: &[String]) {
    popup.removeAllItems();
    for item in items {
        popup.addItemWithTitle(&NSString::from_str(item));
    }
}

fn select(popup: &NSPopUpButton, items: &[String], value: &str) {
    if let Some(i) = items.iter().position(|v| v == value) {
        popup.selectItemAtIndex(i as isize);
    }
}

/// The selected index, kept inside `len`.
fn index(popup: &NSPopUpButton, len: usize) -> usize {
    usize::try_from(popup.indexOfSelectedItem())
        .unwrap_or(0)
        .min(len.saturating_sub(1))
}
