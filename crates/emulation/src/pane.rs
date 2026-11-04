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
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => {
                            // EOF sentinel: drain side treats empty vec as end-of-stream.
                            let _ = tx.send(Vec::new());
                            notify(PaneEvent::Closed(id));
                            break;
                        }
                        Ok(n) => {
                            if tx.send(buf[..n].to_vec()).is_err() {
                                break; // UI dropped the pane
                            }
                            notify(PaneEvent::Output(id));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => {
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
