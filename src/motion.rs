//! Price color fades and refresh activity. Data never waits for an effect.
use std::time::Duration;

use ratatui::style::{Color, Style};

pub const PRICE_FLASH: Duration = Duration::from_millis(900);
pub const FRAME: Duration = Duration::from_millis(34);
pub const SPINNER_FRAME: Duration = Duration::from_millis(125);

/// Fade a signal back to the original foreground. Only explicit RGB
/// endpoints are interpolated: ANSI colors and Reset belong to the user's
/// terminal palette, whose actual shades and background we do not know.
pub fn pulse(base: Style, signal: Color, elapsed: Duration, duration: Duration) -> Style {
    if elapsed >= duration {
        return base;
    }
    let progress = elapsed.as_secs_f64() / duration.as_secs_f64();
    if let (Color::Rgb(r, g, b), Some(Color::Rgb(x, y, z))) = (signal, base.fg) {
        // Ease out: a clear initial tick, followed by a quiet tail.
        let weight = (1.0 - progress).powi(2);
        let mix = |a: u8, b: u8| (b as f64 + (a as f64 - b as f64) * weight).round() as u8;
        base.fg(Color::Rgb(mix(r, x), mix(g, y), mix(b, z)))
    } else if progress < 0.7 {
        base.fg(signal)
    } else {
        base
    }
}

pub fn spinner(elapsed: Duration) -> char {
    ['|', '/', '-', '\\'][(elapsed.as_millis() / SPINNER_FRAME.as_millis() % 4) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_fades_to_the_exact_original_style() {
        let base = Style::new().fg(Color::Rgb(100, 100, 100));
        let signal = Color::Rgb(200, 0, 0);
        assert_eq!(
            pulse(base, signal, Duration::ZERO, PRICE_FLASH).fg,
            Some(signal)
        );
        assert_eq!(
            pulse(base, signal, PRICE_FLASH / 2, PRICE_FLASH).fg,
            Some(Color::Rgb(125, 75, 75))
        );
        assert_eq!(pulse(base, signal, PRICE_FLASH, PRICE_FLASH), base);
    }

    #[test]
    fn ansi_and_unknown_foregrounds_never_turn_into_rgb() {
        for fg in [Color::Reset, Color::White, Color::Indexed(7)] {
            let base = Style::new().fg(fg);
            let first = pulse(base, Color::Green, Duration::ZERO, PRICE_FLASH);
            assert_eq!(first.fg, Some(Color::Green));
            let middle = pulse(base, Color::Green, PRICE_FLASH / 2, PRICE_FLASH);
            assert_eq!(middle.fg, Some(Color::Green));
            assert_eq!(
                pulse(base, Color::Green, PRICE_FLASH * 4 / 5, PRICE_FLASH),
                base
            );
        }
    }

    #[test]
    fn price_fades_preserve_font_and_background() {
        use ratatui::style::Modifier;
        for fg in [Color::Reset, Color::White, Color::Rgb(200, 200, 200)] {
            for modifier in [Modifier::empty(), Modifier::BOLD] {
                let base = Style::new().fg(fg).bg(Color::Blue).add_modifier(modifier);
                for step in 0..=10 {
                    let style = pulse(
                        base,
                        Color::Rgb(0, 255, 0),
                        PRICE_FLASH * step / 10,
                        PRICE_FLASH,
                    );
                    assert_eq!(style.bg, base.bg);
                    assert_eq!(style.add_modifier, base.add_modifier);
                    assert_eq!(style.sub_modifier, base.sub_modifier);
                }
            }
        }
    }
}
