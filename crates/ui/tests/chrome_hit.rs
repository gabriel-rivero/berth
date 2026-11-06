//! Chrome hit-testing regressions: close-✕ precedence, tab-strip vs sidebar
//! edge-grab overlap, plus/empty zones — at 1x and 2x DPI.

use berth_ui::render::{Chrome, ChromeZone, chrome_zone_at, sidebar_width};

const SIDEBAR_LOGICAL: f32 = 200.0;
const WORKSPACES: usize = 3;
const TABS: usize = 3;

fn zone_at(x: f64, y: f64, scale: f64) -> Option<ChromeZone> {
    let ch = Chrome::new(scale);
    chrome_zone_at(x, y, true, SIDEBAR_LOGICAL, WORKSPACES, TABS, ch)
}

fn sb_w(scale: f64) -> f64 {
    f64::from(sidebar_width(true, SIDEBAR_LOGICAL, scale))
}

#[test]
fn sidebar_rows_close_and_plus_at_1x() {
    let sb = sb_w(1.0);
    let ch = Chrome::new(1.0);
    let row1_y = f64::from(ch.sidebar_top + ch.sidebar_entry_h * 1.5);

    assert_eq!(zone_at(100.0, 44.0, 1.0), Some(ChromeZone::SidebarEntry(0)));
    assert_eq!(
        zone_at(sb - 1.0, 44.0, 1.0),
        Some(ChromeZone::SidebarClose(0)),
        "right-hand ✕ zone must win over row selection"
    );
    assert_eq!(
        zone_at(sb - f64::from(ch.close_w), 44.0, 1.0),
        Some(ChromeZone::SidebarClose(0))
    );
    assert_eq!(
        zone_at(sb - f64::from(ch.close_w) - 1.0, 44.0, 1.0),
        Some(ChromeZone::SidebarEntry(0))
    );
    assert_eq!(
        zone_at(100.0, row1_y, 1.0),
        Some(ChromeZone::SidebarEntry(1))
    );

    let plus_y = f64::from(ch.sidebar_top + WORKSPACES as f32 * ch.sidebar_entry_h + 8.0);
    assert_eq!(zone_at(100.0, plus_y, 1.0), Some(ChromeZone::SidebarPlus));
    assert_eq!(
        zone_at(100.0, plus_y + f64::from(ch.sidebar_entry_h), 1.0),
        Some(ChromeZone::SidebarEmpty)
    );
}

#[test]
fn tab_slots_close_plus_and_empty_at_1x() {
    let sb = sb_w(1.0);
    let ch = Chrome::new(1.0);
    let strip_y = f64::from(ch.tab_bar_h * 0.5);

    assert_eq!(zone_at(sb + 10.0, strip_y, 1.0), Some(ChromeZone::Tab(0)));
    assert_eq!(
        zone_at(sb + f64::from(ch.tab_w) - 1.0, strip_y, 1.0),
        Some(ChromeZone::TabClose(0))
    );
    assert_eq!(
        zone_at(
            sb + f64::from(ch.tab_w) - f64::from(ch.close_w) - 1.0,
            strip_y,
            1.0
        ),
        Some(ChromeZone::Tab(0)),
        "one px left of the ✕ zone is still tab selection"
    );
    assert_eq!(
        zone_at(sb + f64::from(ch.tab_w * 2.5), strip_y, 1.0),
        Some(ChromeZone::Tab(2))
    );
    let plus_x = sb + f64::from(ch.tab_w * TABS as f32 + ch.plus_w * 0.5);
    assert_eq!(zone_at(plus_x, strip_y, 1.0), Some(ChromeZone::TabPlus));
    assert_eq!(
        zone_at(plus_x + f64::from(ch.plus_w), strip_y, 1.0),
        Some(ChromeZone::TabStripEmpty)
    );
}

#[test]
fn tab_close_is_not_stolen_by_edge_grab() {
    let sb = sb_w(1.0);
    let ch = Chrome::new(1.0);
    let strip_y = f64::from(ch.tab_bar_h * 0.5);
    // x just right of the sidebar edge on the strip row: tab, never resize.
    assert_eq!(zone_at(sb + 2.0, strip_y, 1.0), Some(ChromeZone::Tab(0)));
    // Sidebar ✕ also wins over the edge band.
    assert_eq!(
        zone_at(sb - 1.0, 44.0, 1.0),
        Some(ChromeZone::SidebarClose(0))
    );
    // Below the strip the edge band does appear.
    assert_eq!(
        zone_at(sb + 2.0, f64::from(ch.tab_bar_h + 40.0), 1.0),
        Some(ChromeZone::SidebarEdge)
    );
    // But not over pane space.
    assert_eq!(zone_at(sb + 40.0, 300.0, 1.0), None);
}

#[test]
fn hit_zones_scale_with_dpi() {
    let sb = sb_w(2.0);
    assert_eq!(sb, 400.0, "sidebar logical width × 2");
    let ch = Chrome::new(2.0);
    assert_eq!(ch.tab_bar_h, 56.0);
    assert_eq!(ch.tab_w, 280.0);
    assert_eq!(ch.close_w, 28.0);

    let strip_y = f64::from(ch.tab_bar_h * 0.5);
    assert_eq!(
        zone_at(sb + f64::from(ch.tab_w) - 1.0, strip_y, 2.0),
        Some(ChromeZone::TabClose(0))
    );
    assert_eq!(
        zone_at(sb - 1.0, 88.0, 2.0),
        Some(ChromeZone::SidebarClose(0))
    );
    assert_eq!(
        zone_at(sb + 2.0, f64::from(ch.tab_bar_h + 80.0), 2.0),
        Some(ChromeZone::SidebarEdge)
    );
    assert_eq!(
        zone_at(sb + f64::from(ch.tab_w * 1.5), strip_y, 2.0),
        Some(ChromeZone::Tab(1))
    );
}

#[test]
fn edge_band_is_not_clickable_for_pointer_cursor() {
    assert!(!ChromeZone::SidebarEdge.is_clickable());
    assert!(ChromeZone::TabClose(0).is_clickable());
    assert!(ChromeZone::SidebarPlus.is_clickable());
    assert_eq!(ChromeZone::TabClose(4).tab_index(), Some(4));
    assert_eq!(ChromeZone::SidebarClose(2).workspace_index(), Some(2));
}

#[test]
fn hidden_sidebar_moves_tab_origin_to_window_edge() {
    let ch = Chrome::new(1.0);
    let z = chrome_zone_at(10.0, 10.0, false, SIDEBAR_LOGICAL, WORKSPACES, TABS, ch);
    assert_eq!(z, Some(ChromeZone::Tab(0)));
}
