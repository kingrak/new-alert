//! The client's own input vocabulary. The macroquad shell translates real
//! device input into these; nothing above the shell ever sees a macroquad type
//! (DESIGN.md §4.8, §4.7). Tests synthesize [`InputEvent`]s directly.

/// A logical key the terrain camera cares about. Extended as features land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Scroll the camera left.
    Left,
    /// Scroll the camera right.
    Right,
    /// Scroll the camera up.
    Up,
    /// Scroll the camera down.
    Down,
    /// Deploy the selected MCV into a construction yard (M5).
    Deploy,
    /// Toggle the on-screen controls hint overlay (M7, F1).
    Help,
}

/// A pointer button. Left drives selection; right issues move orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    /// Left button — box/click selection.
    Left,
    /// Right button — move order to the selection.
    Right,
}

/// A single input event delivered to [`crate::AppCore::handle`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputEvent {
    /// A key was pressed.
    KeyDown(Key),
    /// A key was released.
    KeyUp(Key),
    /// The pointer moved to viewport pixel coordinates (used for edge scroll).
    MouseMoved {
        /// X in viewport pixels (0 = left edge).
        x: i32,
        /// Y in viewport pixels (0 = top edge).
        y: i32,
    },
    /// The pointer left the window (stops edge scrolling).
    MouseLeft,
    /// A pointer button was pressed at viewport pixel coordinates.
    MouseDown {
        /// Which button.
        button: MouseButton,
        /// X in viewport pixels.
        x: i32,
        /// Y in viewport pixels.
        y: i32,
    },
    /// A pointer button was released at viewport pixel coordinates.
    MouseUp {
        /// Which button.
        button: MouseButton,
        /// X in viewport pixels.
        x: i32,
        /// Y in viewport pixels.
        y: i32,
    },
    /// The drawable viewport was resized to `width`×`height` pixels.
    Resize {
        /// New viewport width in pixels.
        width: u32,
        /// New viewport height in pixels.
        height: u32,
    },
}

/// A rectangle in map-pixel space: the region [`crate::AppCore::compose`] renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Left edge in map pixels (may be clamped to 0).
    pub x: i64,
    /// Top edge in map pixels.
    pub y: i64,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}
