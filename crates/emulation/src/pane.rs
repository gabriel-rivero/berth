//! Pane: the emulation atom. Wraps one `alacritty_terminal::Term` grid plus one `portable-pty`
//! master. A reader thread forwards raw PTY bytes to an internal channel; the UI calls `pump()`
//! on its side to drain into the grid and drive redraws. UI never touches the PtyPair directly.
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use berth_core::config::Shell;
use berth_core::layout::PaneId;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

/// Expand a leading `~`/`~/` against HOME (USERPROFILE on Windows); other paths
/// pass through unchanged.
fn expand_tilde(path: &str) -> PathBuf {
    let home = || std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    if path == "~"
        && let Some(home) = home()
    {
        return PathBuf::from(home);
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = home()
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

/// Events the pane pushes to the UI loop (extend incrementally: titles, exit status, OSC cwd...).
#[derive(Debug, Clone)]
pub enum PaneEvent {
    /// PTY produced bytes: pump() pending and redraw.
    Output(PaneId),
    /// child process exited (or the read side broke).
    Closed(PaneId),
    /// OSC 0/2 title change from the shell (pane's window/tab label).
    Title(PaneId, String),
    /// OSC 112 or ESC c cleared the title.
    TitleReset(PaneId),
    /// OSC 7 cwd report from the shell (file://host/path, percent-decoded abs path).
    /// Only emitted by shells with an integration hook (precmd/PROMPT_COMMAND).
    Cwd(PaneId, String),
    /// Terminal-query reply the parser generated (DSR cursor report, DA1, ...)
    /// that must be written back to the PTY.
    PtyWrite(PaneId, Vec<u8>),
    /// Terminal bell (BEL / OSC 0x07); marks the owning tab for attention.
    Bell(PaneId),
    /// Lifecycle log line (spawn/reader/writer state) for triage; goes to the
    /// app log file unconditionally (BERTH_DEBUG not required).
    Log(String),
}

/// Callback the pane uses to wake the UI event loop.
pub type Notifier = Arc<dyn Fn(PaneEvent) + Send + Sync>;

/// Forwards relevant Term events to the UI loop (VoidListener discards them).
pub struct TermListener {
    pane: PaneId,
    notify: Notifier,
}

impl TermListener {
    pub fn new(pane: PaneId, notify: Notifier) -> Self {
        Self { pane, notify }
    }
}

impl EventListener for TermListener {
    fn send_event(&self, event: alacritty_terminal::event::Event) {
        match event {
            alacritty_terminal::event::Event::Title(title) => {
                (self.notify)(PaneEvent::Title(self.pane, title));
            }
            alacritty_terminal::event::Event::ResetTitle => {
                (self.notify)(PaneEvent::TitleReset(self.pane));
            }
            alacritty_terminal::event::Event::Bell => {
                (self.notify)(PaneEvent::Bell(self.pane));
            }
            // Terminal-query replies (DSR 6n cursor report, DA1/DA2, DECRPM, ...).
            // ConPTY's INHERIT_CURSOR gates cmd.exe startup on the 6n reply.
            alacritty_terminal::event::Event::PtyWrite(text) => {
                (self.notify)(PaneEvent::PtyWrite(self.pane, text.into_bytes()));
            }
            _ => {}
        }
    }
}

/// One search hit inside the grid: absolute line (negative = history), inclusive
/// start column, exclusive end column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridMatch {
    pub line: i32,
    pub start: usize,
    pub end: usize,
}

/// Case-insensitive per-line substring search over scrollback + screen.
/// Cross-line matches are intentionally not supported (keeps offsets stable).
/// A clickable link on one row of the visible grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSpan {
    /// Viewport row (0 = top of the visible grid).
    pub row: usize,
    /// Start/end columns (end exclusive).
    pub start: usize,
    pub end: usize,
    pub uri: String,
}

/// URL schemes we are willing to hand to the OS opener. OSC 8 URIs are
/// terminal-controlled, so anything else (javascript:, custom handlers) is
/// deliberately not clickable.
pub fn is_safe_uri(uri: &str) -> bool {
    let lower = uri.to_ascii_lowercase();
    ["https://", "http://", "file://", "mailto:"]
        .iter()
        .any(|s| lower.starts_with(s))
}

/// Locate the URL spanning `col` in one row of characters (no regex dependency).
/// Scans for known schemes, consumes until whitespace/quote, trims trailing
/// punctuation, and returns the (start, end) range only if `col` is inside it.
pub fn url_at(chars: &[char], col: usize) -> Option<(usize, usize)> {
    const SCHEMES: [&str; 4] = ["https://", "http://", "file://", "mailto:"];
    let lower: Vec<char> = chars.iter().map(|c| c.to_ascii_lowercase()).collect();
    let mut best: Option<(usize, usize)> = None;
    for scheme in SCHEMES {
        let needle: Vec<char> = scheme.chars().collect();
        if needle.len() > lower.len() {
            continue;
        }
        for start in 0..=lower.len() - needle.len() {
            if lower[start..start + needle.len()] != needle[..] {
                continue;
            }
            let mut end = start + needle.len();
            while end < chars.len() && !chars[end].is_whitespace() && !"<>\"'`".contains(chars[end])
            {
                end += 1;
            }
            // Trailing sentence punctuation is not part of the URL.
            while end > start && ".,;:!?)]}".contains(chars[end - 1]) {
                end -= 1;
            }
            if col >= start && col < end {
                // Prefer the earliest/longest enclosing match.
                best = match best {
                    Some((s, e)) if s <= start && e >= end => Some((s, e)),
                    _ => Some((start, end)),
                };
            }
        }
    }
    best
}

fn find_in_term(term: &Term<TermListener>, query: &str) -> Vec<GridMatch> {
    if query.is_empty() {
        return Vec::new();
    }
    let needle: Vec<char> = query.chars().collect();
    let eq_ci = |a: char, b: char| a.to_lowercase().eq(b.to_lowercase());
    let grid = term.grid();
    let cols = grid.columns();
    let history = term.history_size() as i32;
    let rows = grid.screen_lines() as i32;
    let mut hits = Vec::new();
    for line in -history..rows {
        let row = &grid[Line(line)];
        let chars: Vec<char> = (0..cols).map(|col| row[Column(col)].c).collect();
        if chars.len() < needle.len() {
            continue;
        }
        for start in 0..=chars.len() - needle.len() {
            if chars[start..start + needle.len()]
                .iter()
                .zip(&needle)
                .all(|(a, b)| eq_ci(*a, *b))
            {
                hits.push(GridMatch {
                    line,
                    start,
                    end: start + needle.len(),
                });
            }
        }
    }
    hits
}

pub struct Pane {
    term: Term<TermListener>,
    parser: Processor,
    /// PTY output queued by the reader thread (empty vec = EOF sentinel).
    pending: Receiver<Vec<u8>>,
    master: Box<dyn MasterPty + Send>,
    /// Outgoing PTY-input channel; the writer thread drains it (keeps the UI thread free).
    writer_jobs: Sender<Vec<u8>>,
    child: Box<dyn Child + Send>,
    /// geometry currently applied to the grid (kept so resize can rebuild it).
    size: PaneSize,
}

/// Terminal-space point (Line = viewport row minus display offset).
pub type TermPoint = Point<alacritty_terminal::index::Line>;

/// Selection granularity for a press (click count).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    /// Freeform drag selection.
    Simple,
    /// Whole word under the pointer.
    Word,
    /// Whole line (triple click).
    Line,
}

/// Cell geometry for the alacritty grid; lives next to its Term (Term borrows dims only at new()).
///
/// NOTE: Dimensions::total_lines includes scrollback per grid semantics.
struct PaneSize {
    cols: u16,
    rows: u16,
    scrollback: usize,
}

impl Dimensions for PaneSize {
    fn columns(&self) -> usize {
        self.cols as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn total_lines(&self) -> usize {
        self.screen_lines() + self.scrollback
    }
}

impl Pane {
    /// Spawn a pane running the default user shell at 80x24 cells; `notify` is called from the
    /// reader thread whenever fresh output lands (call `pump()` afterwards). Pane ids come from
    /// `Layout::alloc_pane_id` (global counter) and tag every emitted event.
    pub fn spawn(
        id: PaneId,
        cwd: Option<&str>,
        shell: &Shell,
        scrollback: usize,
        kitty_keyboard: bool,
        notify: Notifier,
    ) -> anyhow::Result<Self> {
        match Self::spawn_inner(id, cwd, shell, scrollback, kitty_keyboard, &notify) {
            Ok(pane) => Ok(pane),
            Err(e) => {
                notify(PaneEvent::Log(format!("pty: spawn FAILED: {e:#}")));
                Err(e)
            }
        }
    }

    fn spawn_inner(
        id: PaneId,
        cwd: Option<&str>,
        shell: &Shell,
        scrollback: usize,
        kitty_keyboard: bool,
        notify: &Notifier,
    ) -> anyhow::Result<Self> {
        let pty_system = native_pty_system();

        // NOTE: rows/cols/scrollback must stay in sync with `size` below.
        let pair = pty_system.openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let master = pair.master;

        let mut cmd = match shell.program.as_deref().map(str::trim) {
            Some(prog) if !prog.is_empty() => CommandBuilder::new(prog),
            _ => CommandBuilder::new_default_prog(),
        };
        if !shell.args.is_empty() {
            cmd.args(shell.args.iter());
        }
        // Full-capability terminfo: most children inherit a minimal/dumb TERM
        // from the PTY session, which degrades TUI palettes (claude/vim...) to
        // one color. Only what we actually implement goes here.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "berth");
        // User env comes last so it can override the defaults above.
        for (key, value) in &shell.env {
            cmd.env(key, value);
        }
        // Explicit `[shell] working_dir` wins over the tab's inherited cwd.
        if let Some(dir) = shell.working_dir.as_deref().or(cwd) {
            cmd.cwd(expand_tilde(dir));
        }
        let child = pair.slave.spawn_command(cmd)?;
        let writer = master.take_writer()?;
        let reader = master.try_clone_reader()?;
        // SAFETY-reference: notify is an Arc — clone into the thread body below so
        // borrows never escape this fn (E0521: spawn_inner takes &Notifier but
        // threads need 'static).
        let notify = notify.clone();
        notify(PaneEvent::Log(format!(
            "pty: spawned child (pid {:?}), {}x{}",
            child.process_id(),
            80,
            24
        )));

        // Writer thread: PTY input jobs (keeps write_all/flush off the UI thread —
        // a stalled child must never block the event loop).
        let (wtx, wrx) = channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name("berth-pane-writer".into())
            .spawn(move || {
                let mut writer = writer;
                for bytes in wrx {
                    if let Err(_e) = writer.write_all(&bytes).and_then(|_| writer.flush()) {
                        // Child is gone or wedged; UI will observe Closed via the reader.
                        break;
                    }
                }
            })?;

        let scrollback = scrollback.min(1_000_000);
        let size = PaneSize {
            cols: 80,
            rows: 24,
            scrollback,
        };
        let term_cfg = TermConfig {
            scrolling_history: scrollback,
            // Opt-in only: advertising kitty support makes opted-in apps expect
            // CSI-u key reports, which not every PTY layer (ConPTY) decodes.
            kitty_keyboard,
            ..TermConfig::default()
        };
        let term = Term::new(term_cfg, &size, TermListener::new(id, notify.clone()));

        // Reader thread: PTY -> bytes channel; never touches Term (grid lives on the UI side).
        let (tx, rx) = channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name("berth-pane-reader".into())
            .spawn(move || {
                let notify = notify;
                let mut reader = reader;
                let mut buf = [0u8; 8192];
                let mut first = true;
                // OSC 7 (cwd report) extractor state: carries a trailing partial
                // `\x1b]7;...` fragment across chunk boundaries.
                let mut osc_tail: Vec<u8> = Vec::new();
                notify(PaneEvent::Log("pty: reader started".into()));
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => {
                            notify(PaneEvent::Log("pty: reader eof".into()));
                            // EOF sentinel: drain side treats empty vec as end-of-stream.
                            let _ = tx.send(Vec::new());
                            notify(PaneEvent::Closed(id));
                            break;
                        }
                        Ok(n) => {
                            if first {
                                notify(PaneEvent::Log(format!(
                                    "pty: first output {n} bytes: {:02x?}",
                                    &buf[..n.min(24)]
                                )));
                                first = false;
                            }
                            if let Some(cwd) = extract_osc7(&buf[..n], &mut osc_tail) {
                                notify(PaneEvent::Cwd(id, cwd));
                            }
                            if tx.send(buf[..n].to_vec()).is_err() {
                                break; // UI dropped the pane
                            }
                            notify(PaneEvent::Output(id));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            notify(PaneEvent::Log(format!("pty: reader error: {e}")));
                            notify(PaneEvent::Closed(id));
                            break;
                        }
                    }
                }
            })?;

        Ok(Self {
            term,
            parser: Processor::new(),
            pending: rx,
            writer_jobs: wtx,
            master,
            child,
            size,
        })
    }

    /// Feed pending PTY bytes into the parser/grid. Returns chunks processed (0 = grid unchanged).
    pub fn pump(&mut self) -> usize {
        let mut chunks = 0;
        for bytes in self.pending.try_iter() {
            if bytes.is_empty() {
                break; // EOF sentinel; Closed was (or will be) observed separately.
            }
            self.parser.advance(&mut self.term, &bytes);
            chunks += 1;
        }
        chunks
    }

    /// Grid handle for the renderer (lines/cells incl. scrollback offsets).
    pub fn term(&self) -> &Term<TermListener> {
        &self.term
    }

    /// Whether the child shell is still running (polled; does not block).
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Exit code once the child has been reaped (None while running or if it
    /// died from a signal). Used to surface unexpected shell exits.
    pub fn exit_code(&mut self) -> Option<u32> {
        match self.child.try_wait() {
            Ok(Some(status)) if !status.success() => Some(status.exit_code()),
            _ => None,
        }
    }

    /// Kill the child shell; the reader thread sees EOF and emits Closed afterwards
    /// (tree cleanup happens on the UI side when that event lands).
    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }

    /// Shift the visible viewport (negative lines scroll up into scrollback on some terms;
    /// alacritty: positive Delta scrolls the display up, revealing earlier output).
    pub fn scroll_display(&mut self, scroll: Scroll) {
        self.term.scroll_display(scroll);
    }

    /// Lines the viewport is scrolled back from the live bottom (0 = tail).
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Lines of history available above the visible grid.
    pub fn history_size(&self) -> usize {
        self.term.history_size()
    }

    /// Jump back to the live bottom. Returns true when the viewport moved.
    pub fn scroll_to_bottom(&mut self) -> bool {
        if self.display_offset() == 0 {
            return false;
        }
        self.term.scroll_display(Scroll::Bottom);
        true
    }

    /// All hits of `query` in scrollback + screen (case-insensitive, per line).
    pub fn find_matches(&self, query: &str) -> Vec<GridMatch> {
        find_in_term(&self.term, query)
    }

    /// Scroll so absolute `line` is visible with `context` rows above it.
    pub fn reveal_line(&mut self, line: i32, context: usize) {
        let target = if line < 0 {
            (context as i32 - line).max(0)
        } else {
            0
        };
        let delta = target - self.display_offset() as i32;
        if delta != 0 {
            self.term.scroll_display(Scroll::Delta(delta));
        }
    }

    /// Link under a viewport cell: an OSC 8 hyperlink (expanded across the
    /// contiguous cells sharing its URI) or a URL detected in the row text.
    pub fn link_span_at(&self, row: usize, col: usize) -> Option<LinkSpan> {
        let grid = self.term.grid();
        let line = row as i32 - self.display_offset() as i32;
        if line >= grid.screen_lines() as i32 || line < -(self.term.history_size() as i32) {
            return None;
        }
        let grid_row = &grid[Line(line)];
        let cols = grid.columns();
        if col >= cols {
            return None;
        }
        if let Some(link) = grid_row[Column(col)].hyperlink() {
            let uri = link.uri().to_owned();
            if !is_safe_uri(&uri) {
                return None;
            }
            let same = |c: usize| {
                grid_row[Column(c)]
                    .hyperlink()
                    .is_some_and(|h| h.uri() == uri)
            };
            let mut start = col;
            while start > 0 && same(start - 1) {
                start -= 1;
            }
            let mut end = col + 1;
            while end < cols && same(end) {
                end += 1;
            }
            return Some(LinkSpan {
                row,
                start,
                end,
                uri,
            });
        }
        let chars: Vec<char> = (0..cols).map(|c| grid_row[Column(c)].c).collect();
        let (start, end) = url_at(&chars, col)?;
        let uri: String = chars[start..end].iter().collect();
        is_safe_uri(&uri).then_some(LinkSpan {
            row,
            start,
            end,
            uri,
        })
    }

    /// Drop saved scrollback lines, keeping the visible screen (xterm ED 3),
    /// and snap back to the live tail so the cleared view isn't blank.
    pub fn clear_scrollback(&mut self) {
        self.parser.advance(&mut self.term, b"\x1b[3J");
        self.term.scroll_display(Scroll::Bottom);
    }

    /// Begin/replace the drag selection anchored at `point`.
    pub fn start_selection(&mut self, point: TermPoint) {
        self.start_selection_kind(point, SelectionKind::Simple);
    }

    /// Begin a selection with explicit granularity (double/triple click).
    pub fn start_selection_kind(&mut self, point: TermPoint, kind: SelectionKind) {
        let ty = match kind {
            SelectionKind::Simple => SelectionType::Simple,
            SelectionKind::Word => SelectionType::Semantic,
            SelectionKind::Line => SelectionType::Lines,
        };
        self.term.selection = Some(Selection::new(ty, point, Side::Left));
    }

    /// Extend the selection to `point` (drag).
    pub fn update_selection(&mut self, point: TermPoint) {
        if let Some(sel) = &mut self.term.selection {
            sel.update(point, Side::Left);
        }
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    /// Selected text, if any (copy path).
    pub fn selection_text(&self) -> Option<String> {
        let non_empty = self.term.selection.as_ref().is_some_and(|s| !s.is_empty());
        if non_empty {
            self.term.selection_to_string()
        } else {
            None
        }
    }

    /// Raw stdin path: UTF-8 key input etc. goes straight to the shell.
    pub fn term_mode(&self) -> TermMode {
        *self.term.mode()
    }

    /// Queue input for the pane's writer thread (non-blocking).
    pub fn write(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        self.writer_jobs
            .send(bytes.to_vec())
            .map_err(|_| anyhow::anyhow!("writer gone (pane closed)"))?;
        Ok(())
    }

    /// Re-flow the grid + propagate to child (pixel dims for ConPTY on Windows).
    pub fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_px_w: u16,
        cell_px_h: u16,
    ) -> anyhow::Result<()> {
        self.size = PaneSize {
            cols,
            rows,
            scrollback: self.size.scrollback,
        };
        self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: cols.saturating_mul(cell_px_w),
            pixel_height: rows.saturating_mul(cell_px_h),
        })?;
        self.term.resize(PaneSize {
            cols,
            rows,
            scrollback: self.size.scrollback,
        });
        Ok(())
    }
}

/// Scan raw PTY bytes for complete OSC 7 sequences (`\x1b]7;file://host/path` ST/BEL)
/// and return the percent-decoded absolute path. `tail` carries a trailing partial
/// fragment across chunks (bounded to the previous chunk's length).
fn extract_osc7(chunk: &[u8], tail: &mut Vec<u8>) -> Option<String> {
    let mut data: Vec<u8> = std::mem::take(tail);
    data.extend_from_slice(chunk);
    *tail = Vec::new();

    let mut result: Option<String> = None;
    let mut i = 0;
    while i < data.len() {
        if data[i] != 0x1b || data.get(i + 1) != Some(&b']') {
            i += 1;
            continue;
        }
        // Find the terminator (BEL or ESC-backslash) from i+2.
        let end = (i + 2..data.len())
            .find(|&j| data[j] == 0x07 || (data[j] == 0x1b && data.get(j + 1) == Some(&b'\\')));
        let Some(end) = end else {
            // Incomplete sequence: carry it for the next chunk (cap so garbage
            // output can't grow it unboundedly).
            *tail = data[i.min(data.len().saturating_sub(4096))..].to_vec();
            return result;
        };
        let payload = &data[i + 2..end];
        i = if data[end] == 0x1b { end + 2 } else { end + 1 };
        if payload.starts_with(b"7;")
            && let Some(path) = osc7_path(std::str::from_utf8(payload).ok()?)
        {
            result = Some(path);
        }
    }
    // No dangling fragment: nothing to carry.
    result
}

/// `7;file://host/path` -> absolute OS path (host ignored; minimal percent-decode).
fn osc7_path(payload: &str) -> Option<String> {
    let rest = payload.strip_prefix("7;")?.trim_end();
    let after = rest.strip_prefix("file://")?;
    // The path begins at the first '/' (authority = host component before it).
    let path_raw = &after[after.find('/')?..];
    if !path_raw.starts_with('/') {
        return None;
    }
    let bytes = path_raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut k = 0;
    while k < bytes.len() {
        if bytes[k] == b'%' && k + 2 < bytes.len() {
            let hex = (bytes[k + 1], bytes[k + 2]);
            if let Ok(b) = u8::from_str_radix(std::str::from_utf8(&[hex.0, hex.1]).ok()?, 16) {
                out.push(b);
                k += 3;
                continue;
            }
        }
        out.push(bytes[k]);
        k += 1;
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod osc7_tests {
    use super::*;

    #[test]
    fn osc7_bel_esc_split_and_mismatch() {
        let mut tail = Vec::new();
        //BEL-terminated, percent-decoded
        assert_eq!(
            extract_osc7(b"junk\x1b]7;file://host/tmp/na%20me\x07tail", &mut tail),
            Some("/tmp/na me".into())
        );
        assert!(tail.is_empty());
        //Sequence split across chunks (tail carry)
        let seq = b"\x1b]7;file://h/tmp/x\x07";
        assert_eq!(extract_osc7(&seq[..5], &mut tail), None);
        assert_eq!(extract_osc7(&seq[5..], &mut tail), Some("/tmp/x".into()));
        //ESC-backslash terminator
        assert_eq!(
            extract_osc7(b"\x1b]7;file://h/tmp/y\x1b\\", &mut tail),
            Some("/tmp/y".into())
        );
        //Non-OSC-7 payload ignored
        assert_eq!(extract_osc7(b"\x1b]2;title\x07", &mut tail), None);
    }
}

#[cfg(test)]
mod tests {
    use super::{expand_tilde, is_safe_uri, url_at};

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn url_at_finds_span_and_trims_punctuation() {
        let row = chars("see https://example.com/a?b=1. now");
        let (s, e) = url_at(&row, 10).expect("inside link");
        assert_eq!(
            row[s..e].iter().collect::<String>(),
            "https://example.com/a?b=1"
        );
        // Outside the span (the trailing period / prose) there is no link.
        assert_eq!(url_at(&row, 0), None);
        assert_eq!(url_at(&row, e), None);

        let row = chars("(http://a.io/path), ok");
        let (s, e) = url_at(&row, 3).unwrap();
        assert_eq!(row[s..e].iter().collect::<String>(), "http://a.io/path");
    }

    #[test]
    fn url_at_handles_mailto_and_rejects_unknown_schemes() {
        let row = chars("mail bob@example.com or mailto:bob@example.com");
        let (s, e) = url_at(&row, 30).unwrap();
        assert_eq!(
            row[s..e].iter().collect::<String>(),
            "mailto:bob@example.com"
        );
        let js = chars("javascript:alert(1)");
        assert_eq!(url_at(&js, 2), None);
        assert!(!is_safe_uri("javascript:alert(1)"));
        assert!(is_safe_uri("HTTPS://Example.com"));
    }

    #[test]
    fn expands_leading_tilde_against_home() {
        let Some(home) = std::env::var_os("HOME") else {
            return; // no HOME in this environment; nothing meaningful to assert
        };
        assert_eq!(
            expand_tilde("~/code"),
            std::path::PathBuf::from(&home).join("code")
        );
        assert_eq!(expand_tilde("~"), std::path::PathBuf::from(&home));
        assert_eq!(expand_tilde("/tmp"), std::path::PathBuf::from("/tmp"));
        assert_eq!(
            expand_tilde("relative/dir"),
            std::path::PathBuf::from("relative/dir")
        );
    }
}

#[cfg(all(test, unix))]
mod shell_config_tests {
    use super::*;
    use berth_core::config::Shell;
    use std::time::{Duration, Instant};

    fn grid_text(pane: &Pane) -> String {
        let mut out = String::new();
        for indexed in pane.term().renderable_content().display_iter {
            if !indexed
                .flags
                .contains(alacritty_terminal::term::cell::Flags::HIDDEN)
            {
                out.push(indexed.c);
            }
        }
        out
    }

    fn wait_for(pane: &mut Pane, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            pane.pump();
            if grid_text(pane).contains(needle) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "missing {needle:?}; grid: {:?}",
                grid_text(pane)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn custom_shell_program_env_and_scrollback_apply() {
        let mut shell = Shell {
            program: Some("/bin/sh".into()),
            args: vec!["-c".into(), "printf CFG-$BERTH_TEST_VAR".into()],
            ..Default::default()
        };
        shell.env.insert("BERTH_TEST_VAR".into(), "ok".into());
        let mut pane = Pane::spawn(
            berth_core::layout::PaneId(1),
            Some("/tmp"),
            &shell,
            123,
            false,
            Arc::new(|_| {}),
        )
        .expect("spawn custom shell");
        assert_eq!(pane.size.scrollback, 123);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            pane.pump();
            if grid_text(&pane).contains("CFG-ok") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "shell output missing: {:?}",
                grid_text(&pane)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        pane.kill();
    }

    #[test]
    fn nonzero_exit_code_is_reported_and_clean_exit_is_not() {
        let shell = Shell {
            program: Some("/bin/sh".into()),
            args: vec!["-c".into(), "exit 7".into()],
            ..Default::default()
        };
        let mut pane = Pane::spawn(
            berth_core::layout::PaneId(9),
            Some("/tmp"),
            &shell,
            100,
            false,
            Arc::new(|_| {}),
        )
        .expect("spawn exiting shell");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            pane.pump();
            if let Some(code) = pane.exit_code() {
                assert_eq!(code, 7);
                break;
            }
            assert!(Instant::now() < deadline, "exit code never observed");
            std::thread::sleep(Duration::from_millis(10));
        }

        let shell = Shell {
            program: Some("/bin/sh".into()),
            args: vec!["-c".into(), "exit 0".into()],
            ..Default::default()
        };
        let mut pane = Pane::spawn(
            berth_core::layout::PaneId(10),
            Some("/tmp"),
            &shell,
            100,
            false,
            Arc::new(|_| {}),
        )
        .expect("spawn exiting shell");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            pane.pump();
            assert!(
                pane.exit_code().is_none(),
                "clean exit must not raise a notice"
            );
            if !pane.is_alive() {
                break;
            }
            assert!(Instant::now() < deadline, "clean exit never reaped");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn search_finds_history_and_reveals_the_line() {
        let shell = Shell {
            program: Some("/bin/sh".into()),
            args: vec![
                "-c".into(),
                "for i in $(seq 1 100); do echo line-$i; done; echo NEEDLE-42".into(),
            ],
            ..Default::default()
        };
        let mut pane = Pane::spawn(
            berth_core::layout::PaneId(2),
            Some("/tmp"),
            &shell,
            500,
            false,
            Arc::new(|_| {}),
        )
        .expect("spawn search shell");
        wait_for(&mut pane, "NEEDLE-42");

        let hits = pane.find_matches("needle-42");
        assert_eq!(hits.len(), 1, "case-insensitive single hit: {hits:?}");
        assert!(
            pane.history_size() > 0,
            "expected scrollback after 100 lines"
        );
        // Reveal a line 10 rows into history and verify it is on screen.
        pane.reveal_line(-10, 2);
        assert!(pane.display_offset() > 0, "reveal should scroll back");
        let offset = pane.display_offset() as i32;
        assert!((-10 + offset) >= 0 && (-10 + offset) < 24);
        pane.kill();
    }

    #[test]
    fn clear_scrollback_drops_history_and_snaps_to_tail() {
        let shell = Shell {
            program: Some("/bin/sh".into()),
            args: vec![
                "-c".into(),
                "for i in $(seq 1 100); do echo line-$i; done".into(),
            ],
            ..Default::default()
        };
        let mut pane = Pane::spawn(
            berth_core::layout::PaneId(4),
            Some("/tmp"),
            &shell,
            500,
            false,
            Arc::new(|_| {}),
        )
        .expect("spawn clear shell");
        wait_for(&mut pane, "line-100");
        assert!(pane.history_size() > 0);
        pane.reveal_line(-10, 2);
        assert!(pane.display_offset() > 0);

        pane.clear_scrollback();
        assert_eq!(pane.history_size(), 0, "ED 3 must drop saved lines");
        assert_eq!(pane.display_offset(), 0, "clear snaps to the live tail");
        pane.kill();
    }

    #[test]
    fn double_and_triple_click_select_word_and_line() {
        let shell = Shell {
            program: Some("/bin/sh".into()),
            args: vec![
                "-c".into(),
                "printf 'alpha beta gamma\\nsecond line\\n'".into(),
            ],
            ..Default::default()
        };
        let mut pane = Pane::spawn(
            berth_core::layout::PaneId(3),
            Some("/tmp"),
            &shell,
            500,
            false,
            Arc::new(|_| {}),
        )
        .expect("spawn selection shell");
        wait_for(&mut pane, "second line");

        // Column 8 sits inside "beta" on the first row.
        let inside_beta = Point::new(Line(0), Column(8));
        pane.start_selection_kind(inside_beta, SelectionKind::Word);
        let word = pane.selection_text().expect("word selection");
        assert_eq!(word.trim(), "beta", "semantic selection: {word:?}");

        pane.start_selection_kind(inside_beta, SelectionKind::Line);
        let line = pane.selection_text().expect("line selection");
        assert_eq!(line.trim(), "alpha beta gamma", "line selection: {line:?}");
        pane.kill();
    }

    #[test]
    fn bell_is_forwarded_to_the_ui() {
        use alacritty_terminal::event::EventListener;
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let listener = TermListener::new(
            berth_core::layout::PaneId(4),
            Arc::new(move |ev| sink.lock().expect("events").push(ev)),
        );
        listener.send_event(alacritty_terminal::event::Event::Bell);
        let events = seen.lock().expect("events");
        assert!(
            matches!(events.as_slice(), [PaneEvent::Bell(id)] if *id == berth_core::layout::PaneId(4))
        );
    }
}
