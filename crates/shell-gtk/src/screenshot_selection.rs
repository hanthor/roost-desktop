//! Keyboard area geometry, following GNOME Shell 51 UIAreaSelector.
//! Endpoints remain oriented when an edge crosses its opposite edge.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    fn vertical(self) -> bool {
        matches!(self, Self::Up | Self::Down)
    }
}

/// GNOME's first selection: a quarter of the screen, centred.
pub fn initial_selection(width: f64, height: f64) -> Rect {
    let x = (width * 3.0 / 8.0).floor();
    let y = (height * 3.0 / 8.0).floor();
    Rect {
        x,
        y,
        w: ((width * 5.0 / 8.0).floor() - x).max(1.0),
        h: ((height * 5.0 / 8.0).floor() - y).max(1.0),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SelectionKeys {
    start: (f64, f64),
    last: (f64, f64),
    side: Direction,
    last_direction: Direction,
}

impl SelectionKeys {
    pub fn new(rect: Rect) -> Self {
        Self {
            start: (rect.x, rect.y),
            last: (
                rect.x + rect.w.max(1.0) - 1.0,
                rect.y + rect.h.max(1.0) - 1.0,
            ),
            side: Direction::Left,
            last_direction: Direction::Left,
        }
    }

    pub fn rect(self) -> Rect {
        Rect {
            x: self.start.0.min(self.last.0),
            y: self.start.1.min(self.last.1),
            w: (self.start.0 - self.last.0).abs() + 1.0,
            h: (self.start.1 - self.last.1).abs() + 1.0,
        }
    }

    pub fn reset_area(self, rect: Rect) -> Self {
        Self {
            side: self.side,
            last_direction: self.last_direction,
            ..Self::new(rect)
        }
    }

    pub fn adjust(
        &mut self,
        direction: Direction,
        move_area: bool,
        control: bool,
        shift: bool,
        (width, height): (f64, f64),
    ) -> Rect {
        if width < 1.0 || height < 1.0 || !width.is_finite() || !height.is_finite() {
            return self.rect();
        }
        let amount = if control {
            1.0
        } else if shift {
            if direction.vertical() { height } else { width }
        } else {
            5.0
        };
        let delta = if matches!(direction, Direction::Left | Direction::Up) {
            -amount
        } else {
            amount
        };
        if move_area {
            let rect = self.rect();
            let delta = if direction.vertical() {
                delta.clamp(-rect.y, (height - rect.y - rect.h).max(-rect.y))
            } else {
                delta.clamp(-rect.x, (width - rect.x - rect.w).max(-rect.x))
            };
            if direction.vertical() {
                self.start.1 += delta;
                self.last.1 += delta;
            } else {
                self.start.0 += delta;
                self.last.0 += delta;
            }
        } else if direction.vertical() != self.last_direction.vertical() {
            // First press on a new axis selects the corresponding edge.
            self.side = direction;
            self.last_direction = direction;
        } else {
            match self.side {
                Direction::Left => self.start.0 += delta,
                Direction::Right => self.last.0 += delta,
                Direction::Up => self.start.1 += delta,
                Direction::Down => self.last.1 += delta,
            }
            self.start.0 = self.start.0.clamp(0.0, width - 1.0);
            self.last.0 = self.last.0.clamp(0.0, width - 1.0);
            self.start.1 = self.start.1.clamp(0.0, height - 1.0);
            self.last.1 = self.last.1.clamp(0.0, height - 1.0);
            self.last_direction = direction;
        }
        self.rect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_selects_an_edge_on_axis_change_and_preserves_orientation() {
        let bounds = (1280.0, 800.0);
        let mut keys = SelectionKeys::new(initial_selection(bounds.0, bounds.1));
        let r = keys.adjust(Direction::Left, false, false, false, bounds);
        assert_eq!((r.x, r.w), (475.0, 325.0));
        // Up first chooses the top edge; the next press moves it one pixel.
        assert_eq!(keys.adjust(Direction::Up, false, true, false, bounds), r);
        let r = keys.adjust(Direction::Up, false, true, false, bounds);
        assert_eq!((r.y, r.h), (299.0, 201.0));
        let r = keys.adjust(Direction::Down, false, false, true, bounds);
        assert_eq!((r.y, r.h), (499.0, 301.0));
        // The same oriented endpoint shrinks the flipped rectangle again.
        let r = keys.adjust(Direction::Up, false, true, false, bounds);
        assert_eq!((r.y, r.h), (499.0, 300.0));
    }

    #[test]
    fn movement_and_coarse_resize_stay_inside_the_output() {
        let bounds = (1280.0, 800.0);
        let initial = initial_selection(bounds.0, bounds.1);
        let mut keys = SelectionKeys::new(initial);
        let r = keys.adjust(Direction::Left, true, false, true, bounds);
        assert_eq!(r, Rect { x: 0.0, ..initial });
        let r = keys.adjust(Direction::Right, true, false, true, bounds);
        assert_eq!(
            r,
            Rect {
                x: 960.0,
                ..initial
            }
        );
        let r = keys.adjust(Direction::Down, true, false, true, bounds);
        assert_eq!((r.y, r.h), (600.0, 200.0));
        // Ctrl wins over Shift, as upstream _getIncrement specifies.
        let r = keys.adjust(Direction::Left, true, true, true, bounds);
        assert_eq!(r.x, 959.0);
        let r = keys.adjust(Direction::Left, false, false, true, bounds);
        assert_eq!((r.x, r.w), (0.0, 1279.0));
        let r = keys.adjust(Direction::Left, false, false, true, bounds);
        assert_eq!((r.x, r.w), (0.0, 1279.0));
    }

    #[test]
    fn odd_output_reset_matches_gnome_integer_endpoint_geometry() {
        assert_eq!(
            initial_selection(1279.0, 799.0),
            Rect {
                x: 479.0,
                y: 299.0,
                w: 320.0,
                h: 200.0,
            }
        );
    }

    #[test]
    fn crossing_edges_and_reset_keep_selection_nonempty_and_bounded() {
        let bounds = (17.0, 13.0);
        let initial = initial_selection(bounds.0, bounds.1);
        let mut keys = SelectionKeys::new(initial);
        for direction in [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ] {
            for move_area in [false, true] {
                for control in [false, true] {
                    for shift in [false, true] {
                        for _ in 0..40 {
                            let r = keys.adjust(direction, move_area, control, shift, bounds);
                            assert!(r.x >= 0.0 && r.y >= 0.0 && r.w >= 1.0 && r.h >= 1.0);
                            assert!(r.x + r.w <= bounds.0 && r.y + r.h <= bounds.1);
                        }
                    }
                }
            }
        }
        let side = keys.side;
        keys = keys.reset_area(initial);
        assert_eq!(keys.rect(), initial);
        assert_eq!(keys.side, side);
    }
}
