//! The overlay's state glyph: a small view that draws bars or dots in the
//! accent colour. What it draws is set from outside each frame.
#![allow(unsafe_code)]

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSBezierPath, NSView};
use objc2_foundation::{NSPoint, NSRect, NSSize};

/// What the glyph shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Empty,
    /// Listening: bars centred on the middle line (a waveform).
    Wave([f64; 5]),
    /// Thinking (and other work): three dots, each value its opacity.
    Dots([f64; 3]),
    /// Speaking: equaliser bars rising from the baseline.
    Equaliser([f64; 5]),
}

pub struct Ivars {
    shape: RefCell<Shape>,
}

define_class!(
    // SAFETY:
    // - NSView has no subclassing requirements beyond calling its
    //   designated initialiser, which `new` does.
    // - `Glyph` does not implement `Drop`.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "GlanceOverlayGlyph"]
    #[ivars = Ivars]
    pub struct Glyph;

    impl Glyph {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }
    }
);

pub const WIDTH: f64 = 26.0;
pub const HEIGHT: f64 = 18.0;

impl Glyph {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            shape: RefCell::new(Shape::Empty),
        });
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, HEIGHT));
        // SAFETY: NSView's designated initialiser on a fresh allocation.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    pub fn set_shape(&self, shape: Shape) {
        let changed = *self.ivars().shape.borrow() != shape;
        if changed {
            *self.ivars().shape.borrow_mut() = shape;
            self.setNeedsDisplay(true);
        }
    }

    fn draw(&self) {
        let bounds = self.bounds();
        let (w, h) = (bounds.size.width, bounds.size.height);
        let accent = crate::theme::accent();
        let fill = |rect: NSRect, alpha: f64, round: bool| {
            accent.colorWithAlphaComponent(alpha).setFill();
            let r = rect.size.width.min(rect.size.height) / 2.0;
            let path = if round {
                NSBezierPath::bezierPathWithOvalInRect(rect)
            } else {
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, r, r)
            };
            path.fill();
        };
        match &*self.ivars().shape.borrow() {
            Shape::Empty => {}
            Shape::Wave(bars) | Shape::Equaliser(bars) => {
                let centred = matches!(&*self.ivars().shape.borrow(), Shape::Wave(_));
                let bar_w = 3.0;
                let gap = (w - 5.0 * bar_w) / 4.0;
                for (i, height) in bars.iter().enumerate() {
                    let bh = (height * h).max(bar_w);
                    let x = i as f64 * (bar_w + gap);
                    let y = if centred { (h - bh) / 2.0 } else { 0.0 };
                    fill(
                        NSRect::new(NSPoint::new(x, y), NSSize::new(bar_w, bh)),
                        1.0,
                        false,
                    );
                }
            }
            Shape::Dots(alphas) => {
                let d = 6.0;
                let gap = (w - 3.0 * d) / 2.0;
                for (i, alpha) in alphas.iter().enumerate() {
                    let x = i as f64 * (d + gap);
                    fill(
                        NSRect::new(NSPoint::new(x, (h - d) / 2.0), NSSize::new(d, d)),
                        *alpha,
                        true,
                    );
                }
            }
        }
    }
}
