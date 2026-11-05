//! Drawing the terminal wizard: boxed (a cleared screen per step, a heading box, a progress bar
//! and a menu on the bottom line) or plain (numbered lines that scroll, nothing redrawn, for
//! screen readers and small or non-terminal outputs).

use std::io::{self, BufRead, IsTerminal, Write};

/// The widest the boxes get, so lines stay readable on a wide window.
pub const MAX_WIDTH: usize = 78;
/// Below this size the boxed drawing does not fit, so plain mode is used.
pub const MIN_WIDTH: usize = 60;
pub const MIN_HEIGHT: usize = 20;

#[derive(Debug, Clone)]
pub struct Ui {
    pub plain: bool,
    pub color: bool,
    /// The window's width and height.
    pub cols: usize,
    pub rows: usize,
}

/// The heading box: the step's name and where it is in the workflow.
#[derive(Debug, Clone)]
pub struct Head {
    pub title: String,
    pub n: usize,
    pub total: usize,
}

impl Ui {
    /// Boxed when the output is a terminal of at least 60 by 20 that can draw; plain otherwise,
    /// or when asked (`--plain`, `REELIX_PLAIN=1`).
    pub fn detect(force_plain: bool) -> Self {
        let (cols, rows) = window_size();
        let tty = io::stdout().is_terminal() && io::stdin().is_terminal();
        let dumb = std::env::var("TERM").is_ok_and(|t| t == "dumb");
        let plain = force_plain || !tty || dumb || cols < MIN_WIDTH || rows < MIN_HEIGHT;
        let color = std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty());
        Self {
            plain,
            color,
            cols,
            rows,
        }
    }

    /// Reads the window size again (it can change between steps).
    pub fn refresh(&mut self) {
        let (cols, rows) = window_size();
        self.cols = cols;
        self.rows = rows;
    }

    /// The width of boxes and wrapped text.
    pub fn width(&self) -> usize {
        self.cols.clamp(MIN_WIDTH, MAX_WIDTH)
    }

    fn sgr(&self, code: &str, s: &str) -> String {
        if self.color && !self.plain {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_owned()
        }
    }

    pub fn bold(&self, s: &str) -> String {
        self.sgr("1", s)
    }

    pub fn dim(&self, s: &str) -> String {
        self.sgr("2", s)
    }

    pub fn accent(&self, s: &str) -> String {
        self.sgr("1;35", s)
    }

    pub fn danger(&self, s: &str) -> String {
        self.sgr("1;31", s)
    }

    /// The heading box and progress bar (boxed), or the heading line (plain).
    pub fn heading(&self, head: &Head) -> Vec<String> {
        let pct = head.n.saturating_sub(1) * 100 / head.total.max(1);
        if self.plain {
            return vec![format!(
                "Reelix, step {} of {}: {} ({pct}% done)",
                head.n, head.total, head.title
            )];
        }
        let w = self.width();
        let inner = w - 4;
        let right = format!("step {} of {}", head.n, head.total);
        let room = inner.saturating_sub(right.chars().count() + 1);
        let title = clip(&head.title, room.saturating_sub("Reelix | ".len()));
        let left = format!("{} {} {}", self.bold("Reelix"), self.dim("|"), title);
        let left_len = "Reelix | ".len() + title.chars().count();
        let gap = inner.saturating_sub(left_len + right.chars().count());
        let bar_w = w - 5;
        let filled = bar_w * pct / 100;
        vec![
            format!("┌{}┐", "─".repeat(w - 2)),
            format!("│ {left}{}{} │", " ".repeat(gap), self.dim(&right)),
            format!("└{}┘", "─".repeat(w - 2)),
            format!(
                "{}{} {:>3}%",
                self.accent(&"█".repeat(filled)),
                self.dim(&"░".repeat(bar_w - filled)),
                pct
            ),
        ]
    }

    /// A box with a title and lines inside, at the box's width.
    pub fn card(&self, title: &str, lines: &[String]) -> Vec<String> {
        let w = self.width();
        let inner = w - 4;
        let mut out = vec![format!(
            "┌─ {} {}┐",
            self.bold(title),
            "─".repeat(w.saturating_sub(5 + title.chars().count()))
        )];
        for l in lines {
            for part in wrap(l, inner) {
                let pad = inner.saturating_sub(visible_len(&part));
                out.push(format!("│ {part}{} │", " ".repeat(pad)));
            }
        }
        out.push(format!("└{}┘", "─".repeat(w - 2)));
        out
    }

    /// The menu on the bottom line: the keys that work here, and what Enter does on the right.
    pub fn menu_line(&self, keys: &str, enter: &str) -> String {
        let w = self.width();
        let right = format!("Enter: {enter}");
        let gap = w.saturating_sub(keys.chars().count() + right.chars().count());
        self.sgr("7", &format!("{keys}{}{right}", " ".repeat(gap)))
    }

    /// Clears the window and draws a whole step, with the menu on the bottom line, leaving the
    /// cursor after `prompt`. Plain mode never calls this.
    pub fn draw(&self, top: &[String], body: &[String], prompt: &str, menu: &str) {
        let mut out = String::from("\x1b[2J\x1b[H");
        let room = self.rows.saturating_sub(top.len() + 3);
        // Keep the end of the step (the question) in view on a short window.
        let start = body.len().saturating_sub(room);
        for line in top.iter().chain(&body[start..]) {
            out.push_str(line);
            out.push_str("\r\n");
        }
        out.push_str(&format!("\x1b7\x1b[{};1H{menu}\x1b8", self.rows));
        out.push_str(prompt);
        print!("{out}");
        let _ = io::stdout().flush();
    }

    /// The width of the box [`Ui::overlay`] draws.
    pub fn overlay_width(&self) -> usize {
        self.width().min(self.cols.saturating_sub(4)).max(40)
    }

    /// Draws `lines` dimmed with a box in the middle (help, or the feedback steps).
    pub fn overlay(&self, behind: &[String], title: &str, text: &[String], menu: &str) {
        let w = self.overlay_width();
        let inner = w - 4;
        let mut boxed = vec![format!(
            "┌─ {} {}┐",
            self.bold(title),
            "─".repeat(w.saturating_sub(5 + title.chars().count()))
        )];
        for l in text {
            for part in wrap(l, inner) {
                let pad = inner.saturating_sub(visible_len(&part));
                boxed.push(format!("│ {part}{} │", " ".repeat(pad)));
            }
        }
        boxed.push(format!("└{}┘", "─".repeat(w - 2)));
        let mut out = String::from("\x1b[2J\x1b[H");
        for line in behind.iter().take(self.rows.saturating_sub(1)) {
            out.push_str(&self.dim(&strip_ansi(line)));
            out.push_str("\r\n");
        }
        let top = self.rows.saturating_sub(boxed.len() + 1) / 2 + 1;
        let left = (self.cols.saturating_sub(w)) / 2 + 1;
        for (i, line) in boxed.iter().enumerate() {
            out.push_str(&format!("\x1b[{};{left}H{line}", top + i));
        }
        out.push_str(&format!("\x1b[{};1H\x1b[2K{menu}", self.rows));
        print!("{out}");
        let _ = io::stdout().flush();
    }

    /// Leaves the boxed drawing: clears the menu line and puts the cursor under the last line.
    pub fn release(&self) {
        if !self.plain {
            print!("\x1b[{};1H\x1b[2K\r\n", self.rows);
            let _ = io::stdout().flush();
        }
    }
}

/// Reads one line. `None` when input has ended (Ctrl+D, or a closed pipe).
pub fn read_line() -> Option<String> {
    let mut s = String::new();
    match io::stdin().lock().read_line(&mut s) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(s.trim_end_matches(['\r', '\n']).to_owned()),
    }
}

/// Reads a line without showing it. Falls back to a plain read when there is no terminal.
pub fn read_secret() -> Option<String> {
    let _ = io::stdout().flush();
    if io::stdin().is_terminal() {
        rpassword::read_password().ok()
    } else {
        read_line()
    }
}

fn window_size() -> (usize, usize) {
    if let Some((terminal_size::Width(w), terminal_size::Height(h))) = terminal_size::terminal_size() {
        return (w as usize, h as usize);
    }
    let env = |k: &str, d: usize| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    (env("COLUMNS", 80), env("LINES", 24))
}

/// Characters on screen, without colour codes.
pub fn visible_len(s: &str) -> usize {
    strip_ansi(s).chars().count()
}

pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Cuts `s` to `max` characters, ending with "…" when it was cut.
pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Wraps `text` at word boundaries to `width` characters. Lines with colour codes are kept whole.
/// Text from a server, without control characters (escape sequences among them), so it cannot
/// redraw the screen or fake a prompt. Line breaks and tabs stay.
pub fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if text.contains('\x1b') || text.chars().count() <= width {
        return vec![text.to_owned()];
    }
    let indent: String = text.chars().take_while(|c| *c == ' ').collect();
    let mut lines = Vec::new();
    let mut line = indent.clone();
    for word in text.split_whitespace() {
        let len = line.chars().count();
        if len > indent.len() && len + 1 + word.chars().count() > width {
            lines.push(std::mem::replace(&mut line, indent.clone()));
        }
        if line.chars().count() > indent.len() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if line.chars().count() > indent.len() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui(cols: usize) -> Ui {
        Ui {
            plain: false,
            color: false,
            cols,
            rows: 30,
        }
    }

    #[test]
    fn every_boxed_line_ends_at_the_same_column() {
        let u = ui(120);
        let head = u.heading(&Head {
            title: "Movie and series information".into(),
            n: 5,
            total: 14,
        });
        let card = u.card(
            "Done",
            &["Open Reelix on this computer: http://localhost:7700".into()],
        );
        for line in head.iter().take(3).chain(&card) {
            assert_eq!(visible_len(line), MAX_WIDTH, "{line}");
        }
        assert!(head[1].contains("Reelix | Movie and series information"));
        assert!(head[1].trim_end_matches(" │").ends_with("step 5 of 14"));
    }

    #[test]
    fn wrap_keeps_words_and_indent() {
        let lines = wrap("   one two three four five six", 12);
        assert!(
            lines
                .iter()
                .all(|l| l.starts_with("   ") && l.chars().count() <= 12)
        );
        assert_eq!(lines.join(" ").split_whitespace().count(), 6);
    }

    #[test]
    fn plain_heading_has_no_box_characters() {
        let mut u = ui(80);
        u.plain = true;
        let head = u.heading(&Head {
            title: "Quality".into(),
            n: 3,
            total: 4,
        });
        assert_eq!(head, vec!["Reelix, step 3 of 4: Quality (50% done)".to_owned()]);
    }
}
