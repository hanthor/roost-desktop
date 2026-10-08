//! Notification group expansion: 200 ms ease-out-quad, clipped to its animated height.
use gtk::prelude::*;
use gtk4 as gtk;
use std::cell::Cell;
use std::rc::Rc;

pub fn height_at(from: f64, to: f64, elapsed_ms: f64, enabled: bool) -> f64 {
    let p = if enabled {
        (elapsed_ms / 200.0).clamp(0.0, 1.0)
    } else {
        1.0
    };
    from + (to - from) * (1.0 - (1.0 - p).powi(2))
}

pub struct Group {
    pub widget: gtk::ScrolledWindow,
    stack: gtk::Stack,
    height: Cell<f64>,
    generation: Cell<u64>,
}
impl Group {
    pub fn new(
        collapsed: &impl IsA<gtk::Widget>,
        expanded: &impl IsA<gtk::Widget>,
        open: bool,
    ) -> Rc<Self> {
        let stack = gtk::Stack::new();
        stack.set_vhomogeneous(false);
        stack.add_named(collapsed, Some("collapsed"));
        stack.add_named(expanded, Some("expanded"));
        stack.set_visible_child_name(if open { "expanded" } else { "collapsed" });
        let widget = gtk::ScrolledWindow::new();
        widget.set_policy(gtk::PolicyType::Never, gtk::PolicyType::External);
        widget.set_propagate_natural_height(true);
        widget.set_child(Some(&stack));
        widget.set_overflow(gtk::Overflow::Hidden);
        Rc::new(Self {
            widget,
            stack,
            height: Cell::new(-1.0),
            generation: Cell::new(0),
        })
    }
    fn set_height(&self, height: f64) {
        let h = height.round().max(1.0) as i32;
        // Keep GTK's min <= max invariant when changing either direction.
        self.widget.set_min_content_height(0);
        self.widget.set_max_content_height(h);
        self.widget.set_min_content_height(h);
        self.height.set(height);
    }
    pub fn set_expanded(self: &Rc<Self>, open: bool) {
        let from = if self.height.get() < 0.0 {
            f64::from(self.widget.height())
        } else {
            self.height.get()
        };
        self.stack
            .set_visible_child_name(if open { "expanded" } else { "collapsed" });
        let child = self.stack.visible_child().expect("group has both states");
        let (_, natural, _, _) =
            child.measure(gtk::Orientation::Vertical, self.widget.width().max(1));
        let to = f64::from(natural);
        // Height growth is motion: fade-only snaps it like off.
        let enabled = crate::motion::current().allows_motion();
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        if !enabled {
            self.set_height(to);
            return;
        }
        self.set_height(from);
        let weak = Rc::downgrade(self);
        let started = Cell::new(None);
        self.widget.add_tick_callback(move |_, clock| {
            let Some(group) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if generation != group.generation.get() {
                return glib::ControlFlow::Break;
            }
            let start = started.get().unwrap_or_else(|| {
                let time = clock.frame_time();
                started.set(Some(time));
                time
            });
            let elapsed_ms = (clock.frame_time() - start) as f64 / 1000.0;
            let policy = crate::motion::current();
            let enabled = policy.allows_motion();
            // GNOME's slow-down factor stretches the 200 ms ease.
            let elapsed_ms = elapsed_ms / policy.slowdown();
            group.set_height(height_at(from, to, elapsed_ms, enabled));
            if !enabled || elapsed_ms >= 200.0 {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::height_at;
    #[test]
    fn notification_groups_expand_and_collapse_with_200_ms_ease_out_quad() {
        assert_eq!(height_at(100.0, 300.0, 0.0, true), 100.0);
        assert_eq!(height_at(100.0, 300.0, 100.0, true), 250.0);
        assert_eq!(height_at(100.0, 300.0, 200.0, true), 300.0);
        assert_eq!(height_at(300.0, 100.0, 100.0, true), 150.0);
        assert_eq!(height_at(100.0, 300.0, 0.0, false), 300.0);
    }
}
