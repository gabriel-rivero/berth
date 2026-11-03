//! Layout tree: Workspaces -> Tabs -> Pane splits. The sidebar renders `Layout::workspaces` order;
//! pane content lives in `berth-emulation::Pane`, keyed by `PaneId` (no UI deps here).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WorkspaceId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId(pub u64);

/// Binary split tree for a tab's panes. `v` = stacked vertically, `h` = side by side.
#[derive(Debug, Clone, PartialEq)]
pub enum SplitNode {
    Pane(PaneId),
    Split {
        first: Box<SplitNode>,
        second: Box<SplitNode>,
        ratio: f32,
        vertical: bool,
    },
}

/// Axis-aligned pixel-space rectangle (window-content coordinates).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Thickness of the divider drawn between sibling panes (px).
pub const DIVIDER_PX: u32 = 2;

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

impl Tab {
    /// New tab whose single root pane is `root_pane` (ids are allocated by the caller:
    /// TabId/PaneId spaces are independent, so `PaneId(id.0)` would collide with split panes).
    pub fn new(id: TabId, root_pane: PaneId, cwd: Option<String>) -> Self {
        Self {
            id,
            // Session-style numbering (monotonic tab ids) keeps labels distinct.
            label: format!("shell {}", id.0 + 1),
            cwd,
            focused: root_pane,
            root: SplitNode::Pane(root_pane),
        }
    }

    /// In-order leaf walk collecting pane ids (grid/tab order).
    pub fn pane_ids(&self, node: &SplitNode, out: &mut Vec<PaneId>) {
        match node {
            SplitNode::Pane(id) => out.push(*id),
            SplitNode::Split { first, second, .. } => {
                self.pane_ids(first, out);
                self.pane_ids(second, out);
            }
        }
    }

    pub fn pane_ids_in_order(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.pane_ids(&self.root, &mut out);
        out
    }

    /// Tile every leaf pane into `area`, returning (PaneId, Rect) in layout order.
    pub fn pane_rects(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        self.split_rects(&self.root, area, &mut out);
        out
    }

    fn split_rects(&self, node: &SplitNode, area: Rect, out: &mut Vec<(PaneId, Rect)>) {
        match node {
            SplitNode::Pane(id) => out.push((*id, area)),
            SplitNode::Split {
                first,
                second,
                ratio,
                vertical,
            } => {
                let ratio = ratio.clamp(0.0, 1.0);
                if *vertical {
                    let (top, bottom) = split_two(area.h, ratio);
                    self.split_rects(
                        first,
                        Rect {
                            y: area.y,
                            h: top,
                            ..area
                        },
                        out,
                    );
                    self.split_rects(
                        second,
                        Rect {
                            y: area.y + top,
                            h: bottom,
                            ..area
                        },
                        out,
                    );
                } else {
                    let (left, right) = split_two(area.w, ratio);
                    self.split_rects(
                        first,
                        Rect {
                            x: area.x,
                            w: left,
                            ..area
                        },
                        out,
                    );
                    self.split_rects(
                        second,
                        Rect {
                            x: area.x + left,
                            w: right,
                            ..area
                        },
                        out,
                    );
                }
            }
        }
    }

    /// Replace the `target` leaf with a split hosting `new` (target keeps the
    /// first/second position it had). Returns false if `target` is not a leaf.
    pub fn split_pane(&mut self, target: PaneId, new: PaneId, vertical: bool) -> bool {
        split_in(&mut self.root, target, new, vertical)
    }

    /// Remove `id` from the tree, collapsing its parent split. Returns true when
    /// the root itself was `id` (the tab became empty).
    pub fn remove_pane(&mut self, id: PaneId) -> bool {
        if let SplitNode::Pane(only) = &self.root
            && *only == id
        {
            return true;
        }
        remove_in(&mut self.root, id);
        false
    }

    /// Divider quads (DIVIDER_PX thick) between sibling splits, pane coordinates.
    pub fn dividers(&self, area: Rect) -> Vec<Rect> {
        let mut out = Vec::new();
        self.divider_rects(&self.root, area, &mut out);
        out
    }

    fn divider_rects(&self, node: &SplitNode, area: Rect, out: &mut Vec<Rect>) {
        if let SplitNode::Split {
            first,
            second,
            ratio,
            vertical,
        } = node
        {
            let ratio = ratio.clamp(0.0, 1.0);
            if *vertical {
                let (top, bottom) = split_two(area.h, ratio);
                out.push(Rect {
                    x: area.x,
                    y: area.y + top,
                    w: area.w,
                    h: DIVIDER_PX,
                });
                self.divider_rects(
                    first,
                    Rect {
                        y: area.y,
                        h: top,
                        ..area
                    },
                    out,
                );
                self.divider_rects(
                    second,
                    Rect {
                        y: area.y + top,
                        h: bottom.saturating_sub(DIVIDER_PX),
                        ..area
                    },
                    out,
                );
            } else {
                let (left, right) = split_two(area.w, ratio);
                out.push(Rect {
                    x: area.x + left,
                    y: area.y,
                    w: DIVIDER_PX,
                    h: area.h,
                });
                self.divider_rects(
                    first,
                    Rect {
                        x: area.x,
                        w: left,
                        ..area
                    },
                    out,
                );
                self.divider_rects(
                    second,
                    Rect {
                        x: area.x + left,
                        w: right.saturating_sub(DIVIDER_PX),
                        ..area
                    },
                    out,
                );
            }
        }
    }
}

fn split_two(size: u32, ratio: f32) -> (u32, u32) {
    let first = (size as f32 * ratio).round() as u32;
    (first.min(size), size - first.min(size))
}

fn split_in(node: &mut SplitNode, target: PaneId, new: PaneId, vertical: bool) -> bool {
    match node {
        SplitNode::Pane(id) if *id == target => {
            *node = SplitNode::Split {
                first: Box::new(SplitNode::Pane(target)),
                second: Box::new(SplitNode::Pane(new)),
                ratio: 0.5,
                vertical,
            };
            true
        }
        SplitNode::Pane(_) => false,
        SplitNode::Split { first, second, .. } => {
            split_in(first, target, new, vertical) || split_in(second, target, new, vertical)
        }
    }
}

/// Collapse the split whose child leaf is `id` by keeping the surviving sibling.
fn remove_in(node: &mut SplitNode, id: PaneId) -> bool {
    let SplitNode::Split { first, second, .. } = node else {
        return false;
    };
    if let SplitNode::Pane(x) = &**first
        && *x == id
    {
        let sib = std::mem::replace(second, Box::new(SplitNode::Pane(PaneId(u64::MAX))));
        *node = *sib;
        return true;
    }
    if let SplitNode::Pane(x) = &**second
        && *x == id
    {
        let sib = std::mem::replace(first, Box::new(SplitNode::Pane(PaneId(u64::MAX))));
        *node = *sib;
        return true;
    }
    remove_in(first, id) || remove_in(second, id)
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

    pub fn active_workspace_mut(&mut self) -> Option<&mut Workspace> {
        self.workspaces.first_mut()
    }

    /// Active tab of the active workspace.
    pub fn active_tab(&self) -> Option<&Tab> {
        self.active_workspace().and_then(Workspace::focused_tab)
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.active_workspace_mut()
            .and_then(|ws| ws.focused_tab_mut())
    }

    /// Next free pane id across the whole layout (ids are global, not per-tab).
    pub fn alloc_pane_id(&self) -> PaneId {
        let mut max = 0;
        for ws in &self.workspaces {
            for tab in &ws.tabs {
                for id in tab.pane_ids_in_order() {
                    max = max.max(id.0);
                }
            }
        }
        PaneId(max + 1)
    }

    /// Next free tab id across the whole layout (panes draw from a separate id space).
    pub fn alloc_tab_id(&self) -> TabId {
        let mut max = 0;
        for ws in &self.workspaces {
            for tab in &ws.tabs {
                max = max.max(tab.id.0);
            }
        }
        TabId(max + 1)
    }

    pub fn with_defaults(label: impl Into<String>) -> Self {
        Self {
            workspaces: vec![Workspace {
                id: WorkspaceId(0),
                label: label.into(),
                tabs: vec![Tab::new(TabId(0), PaneId(0), None)],
                focused_tab: TabId(0),
            }],
        }
    }
}

impl Workspace {
    pub fn focused_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == self.focused_tab)
    }

    pub fn focused_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == self.focused_tab)
    }

    /// Move focus `delta` tabs with wrap-around; returns the new focused tab id.
    pub fn cycle_tab(&mut self, delta: isize) -> Option<TabId> {
        if self.tabs.is_empty() {
            return None;
        }
        let len = self.tabs.len() as isize;
        let pos = self
            .tabs
            .iter()
            .position(|t| t.id == self.focused_tab)
            .unwrap_or(0) as isize;
        let next = self.tabs[(pos + delta).rem_euclid(len) as usize].id;
        self.focused_tab = next;
        Some(next)
    }

    /// Drop a tab (its panes are usually already dead); focus moves to the last
    /// remaining tab. Returns true when removed. If none remain the focused_tab
    /// id is left stale — callers treat an empty workspace as "exit".
    pub fn remove_tab(&mut self, id: TabId) -> bool {
        let Some(pos) = self.tabs.iter().position(|t| t.id == id) else {
            return false;
        };
        self.tabs.remove(pos);
        if self.focused_tab == id {
            self.focused_tab = self.tabs.last().map_or(id, |t| t.id);
        }
        true
    }
}
