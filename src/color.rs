use std::io::IsTerminal;

/// "hypr-recall" with orange→purple gradient when stdout is a TTY, plain otherwise.
pub fn hr() -> String {
    gradient("hypr-recall")
}

#[allow(
    clippy::cast_precision_loss,      // text is a short name — no real precision lost
    clippy::cast_possible_truncation, // color values are bounded 0-255 by construction
    clippy::cast_sign_loss            // color values are non-negative by construction
)]
pub fn gradient(text: &str) -> String {
    use std::fmt::Write as _;
    if !std::io::stdout().is_terminal() {
        return text.to_owned();
    }
    let from = (249.0_f32, 115.0_f32, 22.0_f32); // orange #f97316
    let to = (168.0_f32, 85.0_f32, 247.0_f32); // purple #a855f7
    let chars: Vec<char> = text.chars().collect();
    let steps = chars.len().saturating_sub(1).max(1) as f32;
    let mut out = String::from("\x1b[1m");
    for (idx, ch) in chars.iter().enumerate() {
        let frac = idx as f32 / steps;
        let red = (from.0 + (to.0 - from.0) * frac) as u8;
        let green = (from.1 + (to.1 - from.1) * frac) as u8;
        let blue = (from.2 + (to.2 - from.2) * frac) as u8;
        write!(out, "\x1b[38;2;{red};{green};{blue}m{ch}").ok();
    }
    out.push_str("\x1b[0m");
    out
}
