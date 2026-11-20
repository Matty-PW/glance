//! The overlay's motion as plain functions of time, so it can be tested
//! without AppKit: easing, the state glyphs' bar and dot sizes, fades.

/// Ease-out cubic, `t` in 0..=1.
pub fn ease_out(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// A value moving from `from` to `to` over `duration` seconds, `elapsed`
/// seconds in.
pub fn tween(from: f64, to: f64, elapsed: f64, duration: f64) -> f64 {
    if duration <= 0.0 {
        return to;
    }
    from + (to - from) * ease_out(elapsed / duration)
}

/// Listening: five bars mirrored about the middle, from the recent mic
/// levels (newest last). Heights are 0.15..=1 of the glyph.
pub fn listening_bars(levels: &[f32; 5]) -> [f64; 5] {
    // Speech RMS sits around 0.01..0.2; a square root spreads it out.
    let h = |level: f32| (f64::from(level).max(0.0) * 12.0).sqrt().clamp(0.15, 1.0);
    // The newest level in the middle, older ones further out.
    [
        h(levels[0]),
        h(levels[2]),
        h(levels[4]),
        h(levels[3]),
        h(levels[1]),
    ]
}

/// Thinking: three dots pulsing in turn; each value is the dot's opacity
/// (0.3..=1). `t` in seconds.
pub fn thinking_dots(t: f64) -> [f64; 3] {
    let dot = |i: f64| {
        let phase = (t * 1.4 - i * 0.22).fract();
        // A short bump per cycle.
        let bump = (phase * std::f64::consts::PI).sin().max(0.0).powi(2);
        0.3 + 0.7 * bump
    };
    [dot(0.0), dot(1.0), dot(2.0)]
}

/// Speaking: five equaliser bars rising from the baseline, moving calmly
/// (0.25..=0.9 of the glyph). `t` in seconds.
pub fn speaking_bars(t: f64) -> [f64; 5] {
    let bar = |i: f64| {
        let a = (t * 2.1 + i * 1.3).sin();
        let b = (t * 3.3 + i * 0.7).sin();
        0.575 + 0.325 * (0.6 * a + 0.4 * b)
    };
    [bar(0.0), bar(1.0), bar(2.0), bar(3.0), bar(4.0)]
}

/// The still pictures used with Reduce Motion (each state keeps its own
/// shape, so it never depends on movement or colour).
pub const STILL_LISTENING: [f64; 5] = [0.35, 0.7, 1.0, 0.7, 0.35];
pub const STILL_THINKING: [f64; 3] = [1.0, 0.6, 0.3];
pub const STILL_SPEAKING: [f64; 5] = [0.45, 0.8, 0.6, 0.9, 0.5];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_starts_and_ends_where_it_should() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert_eq!(ease_out(2.0), 1.0);
        assert!(ease_out(0.5) > 0.5, "ease-out is ahead at half time");
        assert_eq!(tween(10.0, 20.0, 0.0, 0.2), 10.0);
        assert_eq!(tween(10.0, 20.0, 0.2, 0.2), 20.0);
        assert_eq!(tween(10.0, 20.0, 0.0, 0.0), 20.0);
    }

    #[test]
    fn glyph_values_stay_in_range() {
        for step in 0..400 {
            let t = f64::from(step) * 0.037;
            assert!(thinking_dots(t).iter().all(|v| (0.3..=1.0).contains(v)));
            assert!(speaking_bars(t).iter().all(|v| (0.25..=0.9).contains(v)));
        }
        let quiet = listening_bars(&[0.0; 5]);
        assert!(quiet.iter().all(|h| (*h - 0.15).abs() < 1e-9));
        let loud = listening_bars(&[1.0; 5]);
        assert!(loud.iter().all(|h| (*h - 1.0).abs() < 1e-9));
    }

    #[test]
    fn the_newest_level_is_in_the_middle_and_louder_is_taller() {
        let bars = listening_bars(&[0.0, 0.0, 0.0, 0.0, 0.05]);
        assert!(bars[2] > bars[0] && bars[2] > bars[4]);
        let soft = listening_bars(&[0.01; 5])[2];
        let loud = listening_bars(&[0.06; 5])[2];
        assert!(loud > soft);
    }

    #[test]
    fn the_dots_take_turns() {
        // At some moment each dot is the brightest.
        let mut brightest = [false; 3];
        for step in 0..100 {
            let dots = thinking_dots(f64::from(step) * 0.01);
            let i = (0..3).max_by(|a, b| dots[*a].total_cmp(&dots[*b])).unwrap();
            brightest[i] = true;
        }
        assert_eq!(brightest, [true; 3]);
    }
}
