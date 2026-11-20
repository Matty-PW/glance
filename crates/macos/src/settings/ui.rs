//! Building blocks for the settings pages, all Auto Layout so nothing
//! overlaps when the window is resized or a name is long: a page (vertical
//! stack), a caption + rounded group, a row (title and optional secondary
//! text on the left, a control on the right), keycap chips, and controls
//! wired to the window's target.
//!
//! Colours: semantic system colours only, plus `theme::accent()`.
#![allow(unsafe_code)]

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBox, NSBoxType, NSButton, NSColor, NSControl, NSControlStateValueOff, NSControlStateValueOn,
    NSFont, NSLayoutAttribute, NSLayoutConstraint, NSLayoutConstraintOrientation, NSLineBreakMode,
    NSPopUpButton, NSSlider, NSStackView, NSStackViewDistribution, NSStackViewGravity, NSSwitch,
    NSTextField, NSTitlePosition, NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{NSArray, NSEdgeInsets, NSSize, NSString};

use crate::theme;

/// Width of the control column, so long names truncate instead of pushing
/// the layout around.
pub const CONTROL_W: f64 = 260.0;
/// Secondary text wraps at this width: what the text column has left at
/// the minimum window size, so a line never clips.
pub const TEXT_W: f64 = 200.0;

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `Target` has no state and does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GlanceSettingsWindowTarget"]
    pub struct Target;

    unsafe impl NSObjectProtocol for Target {}

    impl Target {
        #[unsafe(method(changed:))]
        fn changed(&self, sender: &NSControl) {
            let tag = sender.tag();
            // Not inside the action: alerts and the backend run after it.
            dispatch2::DispatchQueue::main().exec_async(move || super::on_change(tag));
        }
    }
);

impl Target {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: plain `-[NSObject init]` on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    // SAFETY: NSView subclass that only reports itself flipped (content
    // laid out from the top, as a scrolling page should be); no Drop.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "GlanceFlippedView"]
    pub struct Flipped;

    impl Flipped {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

impl Flipped {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSView's plain initialiser.
        unsafe { msg_send![super(this), init] }
    }
}

/// Builds controls wired to one target. `tag` = page × 100 + control.
pub struct Ui<'a> {
    pub mtm: MainThreadMarker,
    pub target: &'a Target,
    pub page: isize,
}

pub fn activate(constraints: &[Retained<NSLayoutConstraint>]) {
    for c in constraints {
        c.setActive(true);
    }
}

pub fn auto(view: &NSView) {
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
}

/// Pin `view` to all edges of `container` with `inset`.
pub fn pin(view: &NSView, container: &NSView, inset: f64) {
    auto(view);
    activate(&[
        view.leadingAnchor()
            .constraintEqualToAnchor_constant(&container.leadingAnchor(), inset),
        view.trailingAnchor()
            .constraintEqualToAnchor_constant(&container.trailingAnchor(), -inset),
        view.topAnchor()
            .constraintEqualToAnchor_constant(&container.topAnchor(), inset),
        view.bottomAnchor()
            .constraintEqualToAnchor_constant(&container.bottomAnchor(), -inset),
    ]);
}

pub fn stack(
    mtm: MainThreadMarker,
    views: &[&NSView],
    vertical: bool,
    spacing: f64,
) -> Retained<NSStackView> {
    let s = NSStackView::stackViewWithViews(&NSArray::from_slice(views), mtm);
    s.setOrientation(if vertical {
        NSUserInterfaceLayoutOrientation::Vertical
    } else {
        NSUserInterfaceLayoutOrientation::Horizontal
    });
    s.setSpacing(spacing);
    s.setAlignment(if vertical {
        NSLayoutAttribute::Leading
    } else {
        NSLayoutAttribute::CenterY
    });
    s.setDetachesHiddenViews(true);
    auto(&s);
    s
}

/// Make `child` as wide as `parent` (both inside a vertical stack).
pub fn full_width(child: &NSView, parent: &NSView, inset: f64) {
    auto(child);
    activate(&[child
        .widthAnchor()
        .constraintEqualToAnchor_constant(&parent.widthAnchor(), -2.0 * inset)]);
}

pub fn label(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let l = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    l.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    // Let the label shrink (truncate) before a control does.
    l.setContentCompressionResistancePriority_forOrientation(
        249.0,
        NSLayoutConstraintOrientation::Horizontal,
    );
    l
}

pub fn secondary(mtm: MainThreadMarker, text: &str, width: f64) -> Retained<NSTextField> {
    let l = NSTextField::wrappingLabelWithString(&NSString::from_str(text), mtm);
    l.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    l.setTextColor(Some(&NSColor::secondaryLabelColor()));
    l.setPreferredMaxLayoutWidth(width);
    l.setSelectable(false);
    l
}

pub fn title(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let l = label(mtm, text);
    l.setFont(Some(&NSFont::boldSystemFontOfSize(20.0)));
    l
}

/// A small caption above a group.
pub fn caption(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let l = label(mtm, text);
    l.setFont(Some(&NSFont::boldSystemFontOfSize(11.0)));
    l.setTextColor(Some(&NSColor::secondaryLabelColor()));
    l
}

/// The page: a vertical stack of captioned groups, inside a flipped view so
/// it starts at the top when scrolled.
pub struct Page {
    pub root: Retained<Flipped>,
    pub stack: Retained<NSStackView>,
}

impl Page {
    pub fn new(mtm: MainThreadMarker) -> Self {
        let root = Flipped::new(mtm);
        let stack = stack(mtm, &[], true, theme::GROUP_GAP);
        stack.setEdgeInsets(NSEdgeInsets {
            top: 4.0,
            left: theme::MARGIN,
            bottom: theme::MARGIN,
            right: theme::MARGIN,
        });
        root.addSubview(&stack);
        pin(&stack, &root, 0.0);
        Self { root, stack }
    }

    /// A caption and a rounded box holding `rows`, separated by hairlines.
    pub fn group(&self, mtm: MainThreadMarker, caption_text: &str, rows: &[Retained<NSView>]) {
        let block = stack(mtm, &[], true, 6.0);
        if !caption_text.is_empty() {
            let c = caption(mtm, caption_text);
            block.addArrangedSubview(&c);
        }
        let group = NSBox::new(mtm);
        group.setBoxType(NSBoxType::Custom);
        group.setTitlePosition(NSTitlePosition::NoTitle);
        group.setCornerRadius(theme::GROUP_RADIUS);
        group.setBorderWidth(1.0);
        group.setBorderColor(&NSColor::separatorColor());
        group.setFillColor(&NSColor::controlBackgroundColor());
        group.setContentViewMargins(NSSize::new(0.0, 0.0));
        let inner = stack(mtm, &[], true, 10.0);
        inner.setEdgeInsets(NSEdgeInsets {
            top: 10.0,
            left: theme::GROUP_PADDING,
            bottom: 10.0,
            right: theme::GROUP_PADDING,
        });
        for (i, row) in rows.iter().enumerate() {
            if i > 0 {
                let line = NSBox::new(mtm);
                line.setBoxType(NSBoxType::Separator);
                inner.addArrangedSubview(&line);
                full_width(&line, &inner, theme::GROUP_PADDING);
            }
            inner.addArrangedSubview(row);
            full_width(row, &inner, theme::GROUP_PADDING);
        }
        if let Some(content) = group.contentView() {
            content.addSubview(&inner);
            pin(&inner, &content, 0.0);
        }
        block.addArrangedSubview(&group);
        full_width(&group, &block, 0.0);
        self.stack.addArrangedSubview(&block);
        full_width(&block, &self.stack, theme::MARGIN);
    }

    /// Plain views (text, buttons) between groups, full width.
    pub fn add(&self, view: &NSView) {
        self.stack.addArrangedSubview(view);
        full_width(view, &self.stack, theme::MARGIN);
    }
}

/// A settings row: title (and secondary text, and a "Restart to apply"
/// note) on the left, `control` on the right.
pub fn row(
    mtm: MainThreadMarker,
    title_text: &str,
    detail: Option<&str>,
    restart: bool,
    control: &NSView,
) -> Retained<NSView> {
    let text = stack(mtm, &[], true, 2.0);
    let t = label(mtm, title_text);
    // Titles take the room they need, up to the control.
    t.setContentCompressionResistancePriority_forOrientation(
        500.0,
        NSLayoutConstraintOrientation::Horizontal,
    );
    text.addArrangedSubview(&t);
    if let Some(detail) = detail {
        text.addArrangedSubview(&secondary(mtm, detail, TEXT_W));
    }
    if restart {
        text.addArrangedSubview(&restart_note(mtm));
    }
    row_with(mtm, &text, control)
}

/// A row from any text column and control: text left, control right.
pub fn row_with(mtm: MainThreadMarker, text: &NSView, control: &NSView) -> Retained<NSView> {
    let row = NSStackView::new(mtm);
    row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    row.setAlignment(NSLayoutAttribute::CenterY);
    row.setDistribution(NSStackViewDistribution::GravityAreas);
    row.setSpacing(16.0);
    auto(&row);
    row.addView_inGravity(text, NSStackViewGravity::Leading);
    row.addView_inGravity(control, NSStackViewGravity::Trailing);
    text.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
    text.setContentCompressionResistancePriority_forOrientation(
        249.0,
        NSLayoutConstraintOrientation::Horizontal,
    );
    control.setContentCompressionResistancePriority_forOrientation(
        751.0,
        NSLayoutConstraintOrientation::Horizontal,
    );
    // A stack as the control (keycaps, buttons) is only as wide as what it
    // shows, so it sits against the right edge.
    if let Some(s) = control.downcast_ref::<NSStackView>() {
        s.setHuggingPriority_forOrientation(751.0, NSLayoutConstraintOrientation::Horizontal);
    }
    Retained::into_super(row)
}

/// "Restart to apply" under a row whose change needs a restart.
pub fn restart_note(mtm: MainThreadMarker) -> Retained<NSTextField> {
    let l = label(mtm, "↻ Restart to apply");
    l.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    l.setTextColor(Some(&NSColor::secondaryLabelColor()));
    l
}

impl Ui<'_> {
    fn wire(&self, control: &NSControl, id: isize) {
        control.setTag(self.page * 100 + id);
        let target: &AnyObject = self.target;
        // SAFETY: the target is owned by the window, which also owns every
        // control, so it outlives them; `changed:` takes one object.
        unsafe {
            control.setTarget(Some(target));
            control.setAction(Some(sel!(changed:)));
        }
    }

    fn width(view: &NSView, w: f64) {
        auto(view);
        activate(&[view.widthAnchor().constraintEqualToConstant(w)]);
    }

    pub fn popup(&self, items: &[String], id: isize) -> Retained<NSPopUpButton> {
        let p = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(self.mtm),
            objc2_foundation::NSRect::ZERO,
            false,
        );
        set_items(&p, items);
        self.wire(&p, id);
        Self::width(&p, CONTROL_W);
        p
    }

    pub fn slider(
        &self,
        min: f64,
        max: f64,
        ticks: Option<isize>,
        id: isize,
    ) -> Retained<NSSlider> {
        let s = NSSlider::new(self.mtm);
        s.setMinValue(min);
        s.setMaxValue(max);
        if let Some(ticks) = ticks {
            s.setNumberOfTickMarks(ticks);
            s.setAllowsTickMarkValuesOnly(true);
        }
        s.setContinuous(false);
        s.setTrackFillColor(Some(&theme::accent()));
        self.wire(&s, id);
        Self::width(&s, CONTROL_W - 70.0);
        s
    }

    pub fn switch(&self, id: isize) -> Retained<NSSwitch> {
        let s = NSSwitch::new(self.mtm);
        self.wire(&s, id);
        auto(&s);
        s
    }

    pub fn button(&self, text: &str, id: isize, primary: bool) -> Retained<NSButton> {
        // SAFETY: no target yet; `wire` sets it under its rules.
        let b = unsafe {
            NSButton::buttonWithTitle_target_action(&NSString::from_str(text), None, None, self.mtm)
        };
        if primary {
            b.setBezelColor(Some(&theme::accent()));
        }
        self.wire(&b, id);
        auto(&b);
        b
    }

    pub fn text_field(&self, placeholder: &str, id: isize, width: f64) -> Retained<NSTextField> {
        let f = NSTextField::textFieldWithString(&NSString::from_str(""), self.mtm);
        f.setPlaceholderString(Some(&NSString::from_str(placeholder)));
        self.wire(&f, id);
        Self::width(&f, width);
        f
    }

    /// A value shown next to a slider ("1.00×", "Medium").
    pub fn value_label(&self, width: f64) -> Retained<NSTextField> {
        let l = label(self.mtm, "");
        l.setTextColor(Some(&NSColor::secondaryLabelColor()));
        Self::width(&l, width);
        l
    }
}

/// Keycaps for a chord such as "Ctrl+Opt+Shift+G": ⌃ ⌥ ⇧ G.
pub fn keycap_labels(chord: &str) -> Vec<String> {
    chord
        .split('+')
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(|k| {
            match k.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => "⌃",
                "opt" | "option" | "alt" => "⌥",
                "shift" => "⇧",
                "cmd" | "command" | "super" => "⌘",
                "space" => "Space",
                _ => return k.to_uppercase(),
            }
            .to_owned()
        })
        .collect()
}

pub fn keycaps(mtm: MainThreadMarker, chord: &str) -> Retained<NSStackView> {
    let row = stack(mtm, &[], false, 4.0);
    for key in keycap_labels(chord) {
        let cap = NSBox::new(mtm);
        cap.setBoxType(NSBoxType::Custom);
        cap.setTitlePosition(NSTitlePosition::NoTitle);
        cap.setCornerRadius(5.0);
        cap.setBorderWidth(1.0);
        cap.setBorderColor(&NSColor::separatorColor());
        cap.setFillColor(&NSColor::windowBackgroundColor());
        cap.setContentViewMargins(NSSize::new(0.0, 0.0));
        let l = label(mtm, &key);
        l.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(12.0, 0.0)));
        l.setAlignment(objc2_app_kit::NSTextAlignment::Center);
        if let Some(content) = cap.contentView() {
            content.addSubview(&l);
            pin(&l, &content, 3.0);
        }
        auto(&cap);
        activate(&[cap
            .widthAnchor()
            .constraintGreaterThanOrEqualToConstant(24.0)]);
        row.addArrangedSubview(&cap);
    }
    row
}

pub fn set_items(popup: &NSPopUpButton, items: &[String]) {
    popup.removeAllItems();
    for item in items {
        popup.addItemWithTitle(&NSString::from_str(item));
    }
}

pub fn select(popup: &NSPopUpButton, items: &[String], value: &str) {
    if let Some(i) = items.iter().position(|v| v == value) {
        popup.selectItemAtIndex(i as isize);
    }
}

/// The selected index, kept inside `len`.
pub fn index(popup: &NSPopUpButton, len: usize) -> usize {
    usize::try_from(popup.indexOfSelectedItem())
        .unwrap_or(0)
        .min(len.saturating_sub(1))
}

pub fn on_off(on: bool) -> isize {
    if on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    }
}

#[cfg(test)]
mod tests {
    use super::keycap_labels;

    #[test]
    fn chords_become_keycaps() {
        assert_eq!(keycap_labels("Ctrl+Opt+G"), ["⌃", "⌥", "G"]);
        assert_eq!(keycap_labels("Ctrl+Opt+Shift+g"), ["⌃", "⌥", "⇧", "G"]);
        assert_eq!(keycap_labels("cmd + space"), ["⌘", "Space"]);
        assert_eq!(keycap_labels(""), Vec::<String>::new());
    }
}
