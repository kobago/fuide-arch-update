//! A small line-oriented terminal emulator for the console panel.
//!
//! Enough of VT100 for `arch-update`, `pacman`, `paru` and `flatpak`: SGR colours, `\r`
//! overwrite, erase-in-line, cursor up / down / column moves (pacman's parallel download
//! bars) and erase-below. There is no scroll region and no wrapping: lines grow as needed and
//! the panel wraps them visually. Unknown sequences are dropped.

/// Basic colour of a cell, from the 8 / 16 ANSI colours. RGB / 256-colour SGR values are
/// mapped onto the nearest of these (the palette has few colours by design).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Hue {
    #[default]
    Default,
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style {
    pub hue: Hue,
    pub bold: bool,
    pub dim: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cell {
    ch: char,
    style: Style,
}

/// One run of equally styled text within a line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub style: Style,
}

#[derive(Debug)]
enum State {
    Ground,
    Esc,
    /// `ESC [` … parameters accumulate until the final byte.
    Csi(String),
    /// `ESC ]` … until BEL or `ESC \`.
    Osc(bool),
    /// `ESC (` / `ESC )` charset designation: one more byte.
    Charset,
}

pub struct Terminal {
    lines: Vec<Vec<Cell>>,
    row: usize,
    col: usize,
    cur: Style,
    state: State,
    /// Incomplete UTF-8 sequence carried over between `feed` calls.
    partial: Vec<u8>,
    /// Lines dropped from the front so far (so callers can keep stable indices).
    pub dropped: usize,
    max_lines: usize,
}

impl Default for Terminal {
    fn default() -> Self {
        Self::new()
    }
}

impl Terminal {
    pub fn new() -> Self {
        Self {
            lines: vec![Vec::new()],
            row: 0,
            col: 0,
            cur: Style::default(),
            state: State::Ground,
            partial: Vec::new(),
            dropped: 0,
            max_lines: 20_000,
        }
    }

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    /// Row the cursor is on (the line a prompt would be waiting on).
    pub fn cursor_row(&self) -> usize {
        self.row
    }

    /// Plain text of line `i` (trailing spaces trimmed).
    pub fn text(&self, i: usize) -> String {
        let s: String = self
            .lines
            .get(i)
            .map(|l| l.iter().map(|c| c.ch).collect())
            .unwrap_or_default();
        s.trim_end().to_string()
    }

    /// Text of the cursor line: what a program waiting for input has just printed.
    pub fn cursor_text(&self) -> String {
        self.text(self.row)
    }

    /// The last `n` lines as plain text, oldest first (for prompt context such as numbered lists).
    pub fn tail(&self, n: usize) -> Vec<String> {
        let start = self.lines.len().saturating_sub(n);
        (start..self.lines.len()).map(|i| self.text(i)).collect()
    }

    /// Styled runs of line `i`.
    pub fn runs(&self, i: usize) -> Vec<Run> {
        let mut out: Vec<Run> = Vec::new();
        let Some(line) = self.lines.get(i) else {
            return out;
        };
        // trailing spaces carry no information; drop them (keeps wrapped labels tidy)
        let end = line
            .iter()
            .rposition(|c| c.ch != ' ')
            .map(|p| p + 1)
            .unwrap_or(0);
        for c in &line[..end] {
            match out.last_mut() {
                Some(r) if r.style == c.style => r.text.push(c.ch),
                _ => out.push(Run {
                    text: c.ch.to_string(),
                    style: c.style,
                }),
            }
        }
        out
    }

    /// Feed raw bytes from the pty.
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut buf = std::mem::take(&mut self.partial);
        buf.extend_from_slice(bytes);
        let (valid, rest) = match std::str::from_utf8(&buf) {
            Ok(s) => (s.to_string(), Vec::new()),
            Err(e) => {
                let good = e.valid_up_to();
                let keep = match e.error_len() {
                    // truncated sequence at the end: wait for more bytes
                    None => buf[good..].to_vec(),
                    Some(_) => Vec::new(),
                };
                let mut s = String::from_utf8_lossy(&buf[..good]).into_owned();
                if e.error_len().is_some() {
                    // a genuinely bad byte: replace it and carry on
                    s.push('\u{FFFD}');
                    let skip = good + e.error_len().unwrap_or(1);
                    let tail = String::from_utf8_lossy(&buf[skip..]).into_owned();
                    s.push_str(&tail);
                }
                (s, keep)
            }
        };
        self.partial = rest;
        for ch in valid.chars() {
            self.put(ch);
        }
        self.trim();
    }

    fn put(&mut self, ch: char) {
        match &mut self.state {
            State::Ground => match ch {
                '\x1b' => self.state = State::Esc,
                '\r' => self.col = 0,
                '\n' => self.newline(),
                '\t' => self.col = (self.col / 8 + 1) * 8,
                '\x08' => self.col = self.col.saturating_sub(1),
                '\x07' => {}
                c if (c as u32) < 0x20 => {}
                c => self.write_char(c),
            },
            State::Esc => match ch {
                '[' => self.state = State::Csi(String::new()),
                ']' => self.state = State::Osc(false),
                '(' | ')' => self.state = State::Charset,
                // ESC 7 / ESC 8 (save / restore cursor), ESC = / ESC > (keypad), ESC c (reset)...
                _ => self.state = State::Ground,
            },
            State::Csi(params) => {
                if ('\x40'..='\x7e').contains(&ch) {
                    let params = std::mem::take(params);
                    self.state = State::Ground;
                    self.csi(&params, ch);
                } else {
                    params.push(ch);
                }
            }
            State::Osc(esc) => match ch {
                '\x07' => self.state = State::Ground,
                '\x1b' => *esc = true,
                '\\' if *esc => self.state = State::Ground,
                _ => *esc = false,
            },
            State::Charset => self.state = State::Ground,
        }
    }

    fn newline(&mut self) {
        self.row += 1;
        self.col = 0;
        if self.row >= self.lines.len() {
            self.lines.push(Vec::new());
        }
    }

    fn write_char(&mut self, ch: char) {
        while self.row >= self.lines.len() {
            self.lines.push(Vec::new());
        }
        let line = &mut self.lines[self.row];
        while line.len() < self.col {
            line.push(Cell {
                ch: ' ',
                style: Style::default(),
            });
        }
        let cell = Cell {
            ch,
            style: self.cur,
        };
        if self.col < line.len() {
            line[self.col] = cell;
        } else {
            line.push(cell);
        }
        self.col += 1;
    }

    fn csi(&mut self, params: &str, fin: char) {
        let private = params.starts_with('?');
        let nums: Vec<usize> = params
            .trim_start_matches('?')
            .split(';')
            .map(|p| p.parse().unwrap_or(0))
            .collect();
        let n1 = |d: usize| -> usize {
            let v = nums.first().copied().unwrap_or(0);
            if v == 0 {
                d
            } else {
                v
            }
        };
        match fin {
            'm' if !private => self.sgr(&nums),
            'A' => self.row = self.row.saturating_sub(n1(1)),
            'B' => {
                self.row += n1(1);
                while self.row >= self.lines.len() {
                    self.lines.push(Vec::new());
                }
            }
            'C' => self.col += n1(1),
            'D' => self.col = self.col.saturating_sub(n1(1)),
            'G' => self.col = n1(1) - 1,
            'H' | 'f' => {
                // absolute positioning has no meaning without a screen; treat as "column"
                let c = nums.get(1).copied().unwrap_or(1).max(1);
                self.col = c - 1;
            }
            'K' => {
                let mode = nums.first().copied().unwrap_or(0);
                if let Some(line) = self.lines.get_mut(self.row) {
                    match mode {
                        0 => line.truncate(self.col),
                        1 => {
                            for c in line.iter_mut().take(self.col + 1) {
                                c.ch = ' ';
                            }
                        }
                        _ => line.clear(),
                    }
                }
            }
            'J' => {
                let mode = nums.first().copied().unwrap_or(0);
                match mode {
                    0 => {
                        if let Some(line) = self.lines.get_mut(self.row) {
                            line.truncate(self.col);
                        }
                        self.lines.truncate(self.row + 1);
                    }
                    _ => {
                        // clear screen: keep the history, start a fresh line
                        if !self.lines.last().is_some_and(|l| l.is_empty()) {
                            self.lines.push(Vec::new());
                        }
                        self.row = self.lines.len() - 1;
                        self.col = 0;
                    }
                }
            }
            // cursor show / hide, bracketed paste, alternate screen, save / restore: ignored
            _ => {}
        }
    }

    fn sgr(&mut self, nums: &[usize]) {
        if nums.is_empty() {
            self.cur = Style::default();
            return;
        }
        let mut i = 0;
        while i < nums.len() {
            match nums[i] {
                0 => self.cur = Style::default(),
                1 => self.cur.bold = true,
                2 => self.cur.dim = true,
                22 => {
                    self.cur.bold = false;
                    self.cur.dim = false;
                }
                30 => self.cur.hue = Hue::Black,
                31 | 91 => self.cur.hue = Hue::Red,
                32 | 92 => self.cur.hue = Hue::Green,
                33 | 93 => self.cur.hue = Hue::Yellow,
                34 | 94 => self.cur.hue = Hue::Blue,
                35 | 95 => self.cur.hue = Hue::Magenta,
                36 | 96 => self.cur.hue = Hue::Cyan,
                37 | 97 => self.cur.hue = Hue::White,
                90 => self.cur.dim = true,
                39 => self.cur.hue = Hue::Default,
                38 | 48 => {
                    // extended colour: 38;5;n or 38;2;r;g;b — consume, approximate foreground
                    let fg = nums[i] == 38;
                    match nums.get(i + 1) {
                        Some(5) => {
                            if fg {
                                self.cur.hue = hue_256(nums.get(i + 2).copied().unwrap_or(7));
                            }
                            i += 2;
                        }
                        Some(2) => {
                            if fg {
                                let r = nums.get(i + 2).copied().unwrap_or(0);
                                let g = nums.get(i + 3).copied().unwrap_or(0);
                                let b = nums.get(i + 4).copied().unwrap_or(0);
                                self.cur.hue = hue_rgb(r, g, b);
                            }
                            i += 4;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn trim(&mut self) {
        if self.lines.len() > self.max_lines {
            let drop = self.lines.len() - self.max_lines + self.max_lines / 10;
            let drop = drop.min(self.row); // never drop the cursor line
            self.lines.drain(..drop);
            self.row -= drop;
            self.dropped += drop;
        }
    }
}

fn hue_256(n: usize) -> Hue {
    match n {
        0 | 8 => Hue::Black,
        1 | 9 => Hue::Red,
        2 | 10 => Hue::Green,
        3 | 11 => Hue::Yellow,
        4 | 12 => Hue::Blue,
        5 | 13 => Hue::Magenta,
        6 | 14 => Hue::Cyan,
        7 | 15 => Hue::White,
        16..=231 => {
            let n = n - 16;
            let (r, g, b) = (n / 36, (n / 6) % 6, n % 6);
            hue_rgb(r * 51, g * 51, b * 51)
        }
        _ => Hue::White,
    }
}

fn hue_rgb(r: usize, g: usize, b: usize) -> Hue {
    let max = r.max(g).max(b);
    if max < 40 {
        return Hue::Black;
    }
    let hi = |v: usize| v * 3 >= max * 2;
    match (hi(r), hi(g), hi(b)) {
        (true, true, true) => Hue::White,
        (true, false, false) => Hue::Red,
        (false, true, false) => Hue::Green,
        (true, true, false) => Hue::Yellow,
        (false, false, true) => Hue::Blue,
        (true, false, true) => Hue::Magenta,
        (false, true, true) => Hue::Cyan,
        _ => Hue::Default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(t: &Terminal) -> Vec<String> {
        (0..t.len()).map(|i| t.text(i)).collect()
    }

    #[test]
    fn plain_lines_and_crlf() {
        let mut t = Terminal::new();
        t.feed(b"hello\r\nworld\r\n");
        assert_eq!(lines(&t), vec!["hello", "world", ""]);
        assert_eq!(t.cursor_row(), 2);
        assert_eq!(t.cursor_text(), "");
    }

    #[test]
    fn carriage_return_overwrites() {
        let mut t = Terminal::new();
        t.feed(b"progress 10%\rprogress 20%\rdone\x1b[K");
        assert_eq!(lines(&t), vec!["done"]);
    }

    #[test]
    fn colours_become_runs() {
        let mut t = Terminal::new();
        t.feed(b"\x1b[1m\x1b[34m==>\x1b[0m\x1b[1m Packages:\x1b[0m\r\n");
        let runs = t.runs(0);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "==>");
        assert_eq!(runs[0].style.hue, Hue::Blue);
        assert!(runs[0].style.bold);
        assert_eq!(runs[1].text, " Packages:");
        assert_eq!(runs[1].style.hue, Hue::Default);
        assert!(runs[1].style.bold);
    }

    #[test]
    fn cursor_up_rewrites_earlier_line() {
        // pacman-style parallel download bars: move up two lines and repaint
        let mut t = Terminal::new();
        t.feed(b"a 10%\r\nb 10%\r\n\x1b[2A\ra 50%\x1b[K\r\n\x1b[K\rb 60%\r\n");
        assert_eq!(lines(&t), vec!["a 50%", "b 60%", ""]);
    }

    #[test]
    fn prompt_stays_on_cursor_line() {
        let mut t = Terminal::new();
        t.feed(b"\x1b[1m\x1b[34m->\x1b[0m\x1b[1m Proceed with update? [Y/n]\x1b[0m ");
        assert_eq!(t.cursor_text(), "-> Proceed with update? [Y/n]");
        t.feed(b"y\r\n");
        assert_eq!(t.text(0), "-> Proceed with update? [Y/n] y");
        assert_eq!(t.cursor_text(), "");
    }

    #[test]
    fn split_utf8_and_escape_sequences_survive_chunk_boundaries() {
        let mut t = Terminal::new();
        let s = "日本語\x1b[32mok\x1b[0m".as_bytes();
        for b in s {
            t.feed(std::slice::from_ref(b));
        }
        assert_eq!(t.text(0), "日本語ok");
        assert_eq!(t.runs(0)[1].style.hue, Hue::Green);
    }

    #[test]
    fn osc_title_and_private_modes_are_ignored() {
        let mut t = Terminal::new();
        t.feed(b"\x1b]0;title\x07\x1b[?25lvisible\x1b[?25h\x1b(B");
        assert_eq!(t.text(0), "visible");
    }

    #[test]
    fn erase_below_truncates_history() {
        let mut t = Terminal::new();
        t.feed(b"one\r\ntwo\r\nthree\x1b[2A\r\x1b[Jnew");
        assert_eq!(lines(&t), vec!["new"]);
    }

    #[test]
    fn tail_gives_context_lines() {
        let mut t = Terminal::new();
        t.feed(b"1 - a\r\n2 - b\r\n\r\n-> Select:");
        assert_eq!(t.tail(3), vec!["2 - b", "", "-> Select:"]);
    }

    #[test]
    fn history_is_capped_without_losing_the_cursor_line() {
        let mut t = Terminal::new();
        t.max_lines = 100;
        for i in 0..250 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        assert!(t.len() <= 100);
        assert_eq!(t.cursor_text(), "");
        assert!(t.dropped > 0);
        assert_eq!(t.text(t.len() - 2), "line 249");
    }
}
