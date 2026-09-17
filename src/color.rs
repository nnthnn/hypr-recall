use std::io::IsTerminal;

/// "hypr-recall" with orange→purple gradient, colorized when **stdout** is a
/// TTY. Use at `println!` sites.
pub fn hr() -> String {
    colorize("hypr-recall", std::io::stdout().is_terminal())
}

/// "hypr-recall" with gradient, colorized when **stderr** is a TTY. Use at
/// `eprintln!` sites so piping stdout doesn't strip color from a TTY stderr
/// (or add it when stderr is redirected).
pub fn hr_err() -> String {
    colorize("hypr-recall", std::io::stderr().is_terminal())
}

/// Gradient `text`, colorized when stdout is a TTY. Used for the clap
/// `bin_name`, which is rendered into help/usage written to stdout.
pub fn gradient(text: &str) -> String {
    colorize(text, std::io::stdout().is_terminal())
}

/// The version number, bold purple (#a855f7) so it picks up where the name
/// gradient ends, colorized when **stdout** is a TTY. clap renders `--version`
/// as `<display_name> <version>` with no styling of its own, so both halves
/// have to be pre-colored by us.
pub fn version(version: &str) -> String {
    if !std::io::stdout().is_terminal() {
        return version.to_owned();
    }
    format!("\x1b[1m\x1b[38;2;168;85;247m{version}\x1b[0m")
}

#[allow(
    clippy::cast_precision_loss,      // text is a short name — no real precision lost
    clippy::cast_possible_truncation, // color values are bounded 0-255 by construction
    clippy::cast_sign_loss            // color values are non-negative by construction
)]
fn colorize(text: &str, is_tty: bool) -> String {
    use std::fmt::Write as _;
    if !is_tty {
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
