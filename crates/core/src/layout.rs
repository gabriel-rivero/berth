//! Layout tree: Workspaces -> Tabs -> Pane splits. The sidebar renders `Layout::workspaces` order;
//! pane content lives in `berth-emulation::Pane`, keyed by `PaneId` (no UI deps here).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WorkspaceId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId(pub u64);

/// Binary split tree for a tab's panes. `v` = stacked vertically, `h` = side by side.
#[derive(Debug, Clone)]
pub enum SplitNode {
    Pane(PaneId),
    Split {
        first: Box<SplitNode>,
        second: Box<SplitNode>,
        ratio: f32,
        vertical: bool,
    },
}

#[derive(Debug, Clone)]
pub struct Tab {
    pub id: TabId,
    pub label: String,
    pub cwd: Option<String>,
    pub root: SplitNode,
    /// pane in keyboard focus
    pub focused: PaneId,
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub label: String,
    pub tabs: Vec<Tab>,
    pub focused_tab: TabId,
}

/// Whole-window layout: ordered workspaces (sidebar top-to-bottom) + which is active.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub workspaces: Vec<Workspace>,
}

impl Layout {
    pub fn active_workspace(&self) -> Option<&Workspace> {
        self.workspaces.first()
    }

    pub fn with_defaults(label: impl Into<String>) -> Self {
        Self {
            workspaces: vec![Workspace {
                id: WorkspaceId(0),
                label: label.into(),
                tabs: vec![Tab {
                    id: TabId(0),
                    label: "shell".to_string(),
                    cwd: None,
                    root: SplitNode::Pane(PaneId(0)),
                    focused: PaneId(0),
                }],
                focused_tab: TabId(0),
            }],
        }
    }
}
