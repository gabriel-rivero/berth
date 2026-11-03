//! Pane: the emulation atom. Wraps one `alacritty_terminal::Term` grid plus one `portable-pty`
//! master. Provides pump/head/write/resize; UI never touches the PtyPair directly.
use alacritty_terminal::term::{Config as TermConfig, SizeInfo, Term};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
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
    term: Term<Dispatch>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send>,
}

struct Dispatch;
impl alacritty_terminal::EventListener for Dispatch {}

impl Pane {
    /// Spawn a pane running the default user shell at `initial_size` cell geometry.
    pub fn spawn(cwd: Option<&str>) -> anyhow::Result<Self> {
        let pty_system = native_pty_system();

        // TODO(M1): plumb real cell size (cols/rows * cell px) from ui layer.
        let master = pty_system.open_pty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
            cell_width: 0,
            cell_height: 0,
        })?;

        let mut cmd = CommandBuilder::new_user_default();
        if let Some(cwd) = cwd {
            cmd.cwd(cwd);
        }
        let child = master.spawn_command(cmd)?;
        let writer = master.take_writer()?;
        let reader = master.try_clone_reader()?;

        let size = SizeInfo::<f32>::default();
        let term = Term::new(TermConfig::default(), &size, Dispatch);

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
    pub fn term(&self) -> &Term<Dispatch> {
        &self.term
    }

    /// Raw stdin path: UTF-8 key input etc. goes straight to the shell.
    pub fn write(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Re-flow the grid + propagate to child (pixel dims for ConPTY on Windows).
    pub fn resize(&self, cols: u16, rows: u16, cell_px_w: u16, cell_px_h: u16) -> anyhow::Result<()> {
        self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: u16::from(cols).saturating_mul(cell_px_w),
            pixel_height: u16::from(rows).saturating_mul(cell_px_h),
            cell_width: cell_px_w,
            cell_height: cell_px_h,
        })?;
        Ok(())
    }
}
