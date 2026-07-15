//! Shared lipgloss styling for the TUI: a Charm-style true-colour palette (aubergine ground,
//! pink/violet/cyan accents), a gradient wordmark, solid title chips, toggle switches, and the
//! rounded-border colours. Colours are 24-bit hex; lipgloss downsamples on limited terminals.

use lipgloss::{join_vertical, Color, Style, LEFT};

// ---- palette (24-bit) ----
const INK: &str = "#17121f"; // ground: dark foreground on chips/selected
const DIM: &str = "#6b6280";
const EXTRA_DIM: &str = "#453e58";
const PINK: &str = "#FF6AC1";
const VIOLET: &str = "#A78BFA";
const BORDER_VIOLET: &str = "#8f7bff";
const CYAN: &str = "#5DE4C7";
const AMBER: &str = "#FDBB4E";
const GREEN: &str = "#56D364";
const CORAL: &str = "#FF6188";

/// Gradient stops for the wordmark ramp: pink -> violet -> cyan.
const STOPS: [&str; 3] = [PINK, VIOLET, CYAN];

/// The ANSI-Shadow block wordmark, coloured left-to-right with the gradient ramp.
const WORDMARK: [&str; 6] = [
    "\u{2588}\u{2588}\u{2557}  \u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2588}\u{2557}   \u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2557}  \u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2557}",
    "\u{2588}\u{2588}\u{2551} \u{2588}\u{2588}\u{2554}\u{255d} \u{2588}\u{2588}\u{2588}\u{2588}\u{2557}  \u{2588}\u{2588}\u{2551} \u{2588}\u{2588}\u{2551} \u{255a}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2554}\u{255d} \u{2588}\u{2588}\u{2551}",
    "\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2554}\u{255d}  \u{2588}\u{2588}\u{2554}\u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2551} \u{2588}\u{2588}\u{2551}  \u{255a}\u{2588}\u{2588}\u{2588}\u{2554}\u{255d}  \u{2588}\u{2588}\u{2551}",
    "\u{2588}\u{2588}\u{2554}\u{2550}\u{2588}\u{2588}\u{2557}  \u{2588}\u{2588}\u{2551}\u{255a}\u{2588}\u{2588}\u{2557}\u{2588}\u{2588}\u{2551} \u{2588}\u{2588}\u{2551}  \u{2588}\u{2588}\u{2554}\u{2588}\u{2588}\u{2557}  \u{2588}\u{2588}\u{2551}",
    "\u{2588}\u{2588}\u{2551}  \u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2551} \u{255a}\u{2588}\u{2588}\u{2588}\u{2588}\u{2551} \u{2588}\u{2588}\u{2551} \u{2588}\u{2588}\u{2554}\u{255d} \u{2588}\u{2588}\u{2557} \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2557}",
    "\u{255a}\u{2550}\u{255d}  \u{255a}\u{2550}\u{255d} \u{255a}\u{2550}\u{255d}  \u{255a}\u{2550}\u{2550}\u{2550}\u{255d} \u{255a}\u{2550}\u{255d} \u{255a}\u{2550}\u{255d}  \u{255a}\u{2550}\u{255d} \u{255a}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{2550}\u{255d}",
];

pub fn color(code: &str) -> Color {
    Color(code.to_string())
}

pub fn accent() -> Style {
    Style::new().foreground(color(PINK))
}

pub fn dim() -> Style {
    Style::new().foreground(color(DIM))
}

pub fn amber() -> Style {
    Style::new().foreground(color(AMBER))
}

pub fn good() -> Style {
    Style::new().foreground(color(GREEN))
}

pub fn bad() -> Style {
    Style::new().foreground(color(CORAL))
}

/// The focused/selected element: violet background, ink foreground.
pub fn selected() -> Style {
    Style::new().foreground(color(INK)).background(color(VIOLET)).bold(true)
}

/// A title chip: solid pink background with ink text, e.g. ` install `.
pub fn chip(label: &str) -> String {
    Style::new().foreground(color(INK)).background(color(PINK)).bold(true).render(label)
}

/// Rounded-border colour: pink when the panel is focused, violet otherwise.
pub fn border(focused: bool) -> Color {
    color(if focused { PINK } else { BORDER_VIOLET })
}

/// A boolean toggle switch: `(\u{25cf} )` off, `( \u{25cf})` on. The track is extra-dim; the
/// knob is dim when off and green when on.
pub fn toggle(on: bool) -> String {
    let track = Style::new().foreground(color(EXTRA_DIM));
    let knob = Style::new().foreground(color(if on { GREEN } else { DIM }));
    if on {
        format!("{}{}{}", track.render("( "), knob.render("\u{25cf}"), track.render(")"))
    } else {
        format!("{}{}{}", track.render("("), knob.render("\u{25cf}"), track.render(" )"))
    }
}

/// The gradient wordmark, six rows of block art with a per-column pink->violet->cyan ramp.
pub fn wordmark() -> String {
    let rows: Vec<String> = WORDMARK.iter().map(|r| gradient_fg(r)).collect();
    join_vertical(LEFT, &rows.iter().map(String::as_str).collect::<Vec<_>>())
}

/// Colour each character of `text` along the pink->violet->cyan ramp (left to right).
fn gradient_fg(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = String::new();
    for (i, ch) in chars.into_iter().enumerate() {
        let t = if n <= 1 { 0.0 } else { i as f32 / (n - 1) as f32 };
        let hex = ramp(t);
        out.push_str(&Style::new().foreground(color(&hex)).bold(true).render(&ch.to_string()));
    }
    out
}

/// Interpolate the three-stop ramp at `t` in [0, 1], returning a `#rrggbb` string.
fn ramp(t: f32) -> String {
    let t = t.clamp(0.0, 1.0);
    let (a, b, local) =
        if t < 0.5 { (STOPS[0], STOPS[1], t * 2.0) } else { (STOPS[1], STOPS[2], (t - 0.5) * 2.0) };
    lerp_hex(a, b, local)
}

fn lerp_hex(a: &str, b: &str, t: f32) -> String {
    let (ar, ag, ab) = rgb(a);
    let (br, bg, bb) = rgb(b);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    format!("#{:02x}{:02x}{:02x}", mix(ar, br), mix(ag, bg), mix(ab, bb))
}

fn rgb(hex: &str) -> (u8, u8, u8) {
    let h = hex.trim_start_matches('#');
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0);
    (p(0), p(2), p(4))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramp_runs_pink_to_cyan_through_violet() {
        assert_eq!(ramp(0.0), PINK.to_lowercase());
        assert_eq!(ramp(0.5), VIOLET.to_lowercase());
        assert_eq!(ramp(1.0), CYAN.to_lowercase());
    }

    #[test]
    fn toggle_knob_slides_with_state() {
        let off = toggle(false);
        let on = toggle(true);
        assert!(off.contains("\u{25cf}"));
        assert!(on.contains("\u{25cf}"));
        assert_ne!(off, on, "the switch renders differently on and off");
    }

    #[test]
    fn wordmark_has_six_rows() {
        assert_eq!(wordmark().lines().count(), 6);
    }
}
