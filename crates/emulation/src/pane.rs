//! Pane: the emulation atom. Wraps one `alacritty_terminal::Term` grid plus one `portable-pty`
//! master. A reader thread forwards raw PTY bytes to an internal channel; the UI calls `pump()`
//! on its side to drain into the grid and drive redraws. UI never touches the PtyPair directly.
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use berth_core::layout::PaneId;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::Read;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

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
            // Terminal-query replies (DSR 6n cursor report, DA1/DA2, DECRPM, ...).
            // ConPTY's INHERIT_CURSOR gates cmd.exe startup on the 6n reply.
            alacritty_terminal::event::Event::PtyWrite(text) => {
                (self.notify)(PaneEvent::PtyWrite(self.pane, text.into_bytes()));
            }
            _ => {}
        }
    }
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
    pub fn spawn(id: PaneId, cwd: Option<&str>, notify: Notifier) -> anyhow::Result<Self> {
        match Self::spawn_inner(id, cwd, &notify) {
            Ok(pane) => Ok(pane),
            Err(e) => {
                notify(PaneEvent::Log(format!("pty: spawn FAILED: {e:#}")));
                Err(e)
            }
        }
    }

    fn spawn_inner(id: PaneId, cwd: Option<&str>, notify: &Notifier) -> anyhow::Result<Self> {
        let pty_system = native_pty_system();

        // NOTE: rows/cols/scrollback must stay in sync with `size` below.
        let pair = pty_system.openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let master = pair.master;

        let mut cmd = CommandBuilder::new_default_prog();
        // Full-capability terminfo: most children inherit a minimal/dumb TERM
        // from the PTY session, which degrades TUI palettes (claude/vim...) to
        // one color. Only what we actually implement goes here.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "berth");
        if let Some(cwd) = cwd {
            cmd.cwd(cwd);
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

        let size = PaneSize {
            cols: 80,
            rows: 24,
            scrollback: 10_000,
        };
        let term = Term::new(
            TermConfig::default(),
            &size,
            TermListener::new(id, notify.clone()),
        );

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

    /// Begin/replace the drag selection anchored at `point`.
    pub fn start_selection(&mut self, point: TermPoint) {
        self.term.selection = Some(Selection::new(SelectionType::Simple, point, Side::Left));
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
