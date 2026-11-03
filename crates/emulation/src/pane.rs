//! Pane: the emulation atom. Wraps one `alacritty_terminal::Term` grid plus one `portable-pty`
//! master. Provides pump/head/write/resize; UI never touches the PtyPair directly.
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config as TermConfig, Term};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::Write;

/// Events the UI layer cares about (extend incrementally: titles, exit, OSC cwd...).
#[derive(Debug)]
pub enum PaneEvent {
    /// shell wrote bytes (grid changed; redraw)
    PropertyChanged,
    /// child process exited
    Closed,
}

pub struct Pane {
    term: Term<VoidListener>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send>,
}

/// Cell geometry for the alacritty grid; lives next to its Term (Term borrows dims only at new()).
struct PaneSize {
    cols: u16,
    rows: u16,
    scrollback: usize,
    // NOTE: Dimensions::total_lines includes scrollback per grid semantics.
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
    /// Spawn a pane running the default user shell at `initial_size` cell geometry.
    pub fn spawn(cwd: Option<&str>) -> anyhow::Result<Self> {
        let pty_system = native_pty_system();

        // TODO(M1): plumb real cell size (cols/rows * cell px) from ui layer.
        let pair = pty_system.openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let master = pair.master;

        let mut cmd = CommandBuilder::new_default_prog();
        if let Some(cwd) = cwd {
            cmd.cwd(cwd);
        }
        let child = pair.slave.spawn_command(cmd)?;
        let writer = master.take_writer()?;
        let reader = master.try_clone_reader()?;

        let size = PaneSize {
            cols: 80,
            rows: 24,
            scrollback: 10_000,
        };
        let term = Term::new(TermConfig::default(), &size, VoidListener);

        // TODO(M1): spawn io thread(s): reader -> term (via vte parser) -> emit PaneEvent;
        // writer <- write(); forward damage regions up instead of PropertyChanged.
        let _ = reader;

        Ok(Self {
            term,
            master,
            writer,
            child,
        })
    }

    /// Grid handle for the renderer (lines/cells incl. scrollback offsets).
    pub fn term(&self) -> &Term<VoidListener> {
        &self.term
    }

    /// Whether the child shell is still running ( polled; does not block ).
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Raw stdin path: UTF-8 key input etc. goes straight to the shell.
    pub fn write(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Re-flow the grid + propagate to child (pixel dims for ConPTY on Windows).
    pub fn resize(
        &self,
        cols: u16,
        rows: u16,
        cell_px_w: u16,
        cell_px_h: u16,
    ) -> anyhow::Result<()> {
        self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: cols.saturating_mul(cell_px_w),
            pixel_height: rows.saturating_mul(cell_px_h),
        })?;
        Ok(())
    }
}
