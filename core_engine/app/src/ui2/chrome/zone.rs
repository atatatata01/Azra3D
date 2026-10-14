//! Zone hide/show — whole dock column or library band (mockup `__lhide` / `__rhide` / `__bhide`).
//!
//! Preferred size is **kept** while hidden so reopen restores a usable size.

use super::{ZONE_SLOP, ZONE_STRIP};

/// Which edge of the zone the resize / restore strip lives on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoneEdge {
    Left,
    Right,
    Bottom,
    Top,
}

impl ZoneEdge {
    pub fn is_horizontal(self) -> bool {
        matches!(self, ZoneEdge::Left | ZoneEdge::Right)
    }

    /// Drag delta component that *grows* the zone (outward from the hidden strip).
    pub fn outward_delta(self, dx: f32, dy: f32) -> f32 {
        match self {
            ZoneEdge::Left => dx,   // drag right grows
            ZoneEdge::Right => -dx, // drag left grows
            ZoneEdge::Bottom => -dy, // drag up grows
            ZoneEdge::Top => dy,    // drag down grows
        }
    }

    /// Drag that *shrinks* toward hide.
    pub fn toward_hide_delta(self, dx: f32, dy: f32) -> f32 {
        -self.outward_delta(dx, dy)
    }
}

/// Mutable zone geometry (left rail, right dock, or bottom library).
#[derive(Clone, Debug)]
pub struct ZoneState {
    /// False → zone is the 6px restore strip only.
    pub open: bool,
    /// Preferred size (width for L/R, height for T/B) when open.
    pub size: f32,
    pub min: f32,
    pub max: f32,
    /// Accumulated drag past min before hide (mockup SLOP).
    pub hide_slop: f32,
    pub edge: ZoneEdge,
}

impl ZoneState {
    pub fn new(edge: ZoneEdge, size: f32, min: f32, max: f32) -> Self {
        Self {
            open: true,
            size: size.clamp(min, max),
            min,
            max,
            hide_slop: 0.0,
            edge,
        }
    }

    /// Layout size this frame (full preferred or 6px strip).
    pub fn layout_size(&self) -> f32 {
        if self.open {
            self.size.clamp(self.min, self.max)
        } else {
            ZONE_STRIP
        }
    }

    pub fn hide(&mut self) {
        self.size = self.size.max(self.min);
        self.open = false;
        self.hide_slop = 0.0;
    }

    pub fn reopen(&mut self, size: Option<f32>) {
        self.open = true;
        if let Some(s) = size {
            self.size = s.clamp(self.min, self.max);
        } else {
            self.size = self.size.max(self.min).clamp(self.min, self.max);
        }
        self.hide_slop = 0.0;
    }

    /// Apply a resize delta along the zone's axis. Returns whether zone became hidden.
    pub fn apply_resize_delta(&mut self, delta_along_axis: f32) -> bool {
        // Positive delta_along_axis = grow (caller maps edge).
        let toward_hide = delta_along_axis < 0.0;
        if toward_hide && self.size <= self.min + 0.5 {
            self.hide_slop += delta_along_axis.abs();
            if self.hide_slop >= ZONE_SLOP {
                self.hide();
                return true;
            }
        } else {
            self.hide_slop = 0.0;
            self.size = (self.size + delta_along_axis).clamp(self.min, self.max);
            if !self.open {
                self.open = true;
            }
        }
        false
    }
}
