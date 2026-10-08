//! The app grid's pages side by side (appDisplay.js AppDisplay's scroll
//! view): a page switch eases out-cubic over 300 ms as GNOME's
//! adjustment does, rather than adw::Carousel's spring. A discrete
//! wheel step turns one page (at most every 150 ms), a touchpad or
//! touch swipe follows the fingers and settles on release.
//!
//! The pages are allocated once at the page they settle on; while the
//! position moves only the drawing slides, so a switch costs no
//! relayout per frame.
use crate::grid_animation::{self as anim, Slot};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// `SCROLL_TIMEOUT_TIME`.
const SCROLL_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(150);
/// Smooth vertical scrolling turns a page per this many pixels.
const SMOOTH_STEP_PX: f64 = 30.0;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct GridPager {
        /// Where the drawing is, in pages.
        pub position: Cell<f64>,
        /// Where the pages are allocated (the current page).
        pub page: Cell<u32>,
        pub slot: Rc<Slot>,
        pub can_scroll: Cell<bool>,
        pub smooth_dy: Cell<f64>,
        /// A swipe in progress: the position it started from.
        pub swipe: Cell<Option<f64>>,
        pub page_changed: RefCell<Vec<Rc<dyn Fn(u32)>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for GridPager {
        const NAME: &'static str = "TunaGridPager";
        type Type = super::GridPager;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for GridPager {
        fn constructed(&self) {
            self.parent_constructed();
            self.can_scroll.set(true);
            let obj = self.obj();
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.set_hexpand(true);
            obj.set_vexpand(true);
        }

        fn dispose(&self) {
            self.slot.stop();
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for GridPager {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let mut child = self.obj().first_child();
            let (mut min, mut nat) = (0, 0);
            while let Some(page) = child {
                let (m, n, _, _) = page.measure(orientation, for_size);
                min = min.max(m);
                nat = nat.max(n);
                child = page.next_sibling();
            }
            (min, nat, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let page = i64::from(self.page.get());
            let mut child = self.obj().first_child();
            let mut index = 0i64;
            while let Some(widget) = child {
                let x = ((index - page) * i64::from(width)) as f32;
                let transform =
                    gtk::gsk::Transform::new().translate(&gtk::graphene::Point::new(x, 0.0));
                widget.allocate(width, height, -1, Some(transform));
                index += 1;
                child = widget.next_sibling();
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let width = f64::from(obj.width());
            let offset = (f64::from(self.page.get()) - self.position.get()) * width;
            snapshot.save();
            snapshot.translate(&gtk::graphene::Point::new(offset as f32, 0.0));
            // Only the pages that can show this frame.
            let position = self.position.get();
            let mut child = obj.first_child();
            let mut index = 0.0;
            while let Some(widget) = child {
                if (index - position).abs() < 1.0 {
                    obj.snapshot_child(&widget, snapshot);
                }
                index += 1.0;
                child = widget.next_sibling();
            }
            snapshot.restore();
        }
    }
}

glib::wrapper! {
    pub struct GridPager(ObjectSubclass<imp::GridPager>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for GridPager {
    fn default() -> Self {
        Self::new()
    }
}

impl GridPager {
    pub fn new() -> Self {
        let pager = glib::Object::new::<Self>();
        pager.add_scrolling();
        pager
    }

    pub fn append(&self, page: &impl IsA<gtk::Widget>) {
        page.set_parent(self);
    }

    pub fn remove(&self, page: &impl IsA<gtk::Widget>) {
        if page.parent().as_ref() == Some(self.upcast_ref()) {
            page.unparent();
        }
        let last = self.n_pages().saturating_sub(1);
        if self.page() > last {
            self.scroll_to(last, false);
        }
    }

    pub fn n_pages(&self) -> u32 {
        let mut n = 0;
        let mut child = self.first_child();
        while let Some(widget) = child {
            n += 1;
            child = widget.next_sibling();
        }
        n
    }

    pub fn nth_page(&self, index: u32) -> Option<gtk::Widget> {
        let mut child = self.first_child();
        for _ in 0..index {
            child = child?.next_sibling();
        }
        child
    }

    /// The page shown, or being switched to.
    pub fn page(&self) -> u32 {
        self.imp().page.get()
    }

    /// Hears the current page change (when a switch begins).
    pub fn connect_page_changed(&self, f: impl Fn(u32) + 'static) {
        self.imp().page_changed.borrow_mut().push(Rc::new(f));
    }

    /// One page forward or back, clamped.
    pub fn step(&self, step: i32) {
        let last = self.n_pages().saturating_sub(1) as i64;
        let target = (i64::from(self.page()) + i64::from(step)).clamp(0, last);
        self.scroll_to(target as u32, true);
    }

    /// Go to page `index`: GNOME's goToPage, eased unless unmapped.
    pub fn scroll_to(&self, index: u32, animate: bool) {
        let imp = self.imp();
        let index = index.min(self.n_pages().saturating_sub(1));
        let from = imp.position.get();
        let changed = imp.page.replace(index) != index;
        if changed {
            self.queue_allocate();
        }
        let motion = anim::motion();
        let distance = f64::from(index) - from;
        let total = if animate && self.is_mapped() && distance != 0.0 {
            anim::PAGE_SWITCH
                .lasting(anim::page_switch_ms(distance))
                .end_ms(motion)
        } else {
            0.0
        };
        let timing = anim::PAGE_SWITCH.lasting(anim::page_switch_ms(distance));
        let weak = self.downgrade();
        imp.slot.start(
            self,
            "page-switch",
            total,
            move |elapsed| {
                if let Some(pager) = weak.upgrade() {
                    let to = f64::from(index);
                    pager
                        .imp()
                        .position
                        .set(anim::lerp(from, to, timing.at(elapsed, motion)));
                    pager.queue_draw();
                }
            },
            || {},
        );
        if changed {
            let callbacks = imp.page_changed.borrow().clone();
            for f in callbacks {
                f(index);
            }
        }
    }

    fn set_position(&self, position: f64) {
        let last = f64::from(self.n_pages().saturating_sub(1));
        self.imp().position.set(position.clamp(0.0, last));
        self.queue_draw();
    }

    fn swipe_begin(&self) {
        let imp = self.imp();
        imp.slot.stop();
        if imp.swipe.get().is_none() {
            imp.swipe.set(Some(imp.position.get()));
        }
    }

    /// Follow the fingers by `pages`.
    fn swipe_update(&self, pages: f64) {
        self.swipe_begin();
        self.set_position(self.imp().position.get() + pages);
    }

    /// Settle on the nearest page, or the next one in the swipe's
    /// direction once it went a quarter of the way.
    fn swipe_end(&self) {
        let imp = self.imp();
        let Some(start) = imp.swipe.take() else {
            return;
        };
        let position = imp.position.get();
        let moved = position - start;
        let base = start.round();
        let target = if moved.abs() >= 0.25 {
            base + moved.signum()
        } else {
            base
        };
        let last = f64::from(self.n_pages().saturating_sub(1));
        self.scroll_to(target.clamp(0.0, last) as u32, true);
    }

    fn add_scrolling(&self) {
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let weak = self.downgrade();
        scroll.connect_scroll(move |controller, dx, dy| {
            let Some(pager) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let imp = pager.imp();
            let rtl = pager.direction() == gtk::TextDirection::Rtl;
            if controller.unit() == gtk::gdk::ScrollUnit::Surface {
                // A touchpad: sideways follows the fingers, up and down
                // turns a page per step as GNOME's discrete scrolling.
                if dx.abs() > dy.abs() {
                    let width = f64::from(pager.width().max(1));
                    pager.swipe_update(if rtl { -dx } else { dx } / width);
                    return glib::Propagation::Stop;
                }
                let acc = imp.smooth_dy.get() + dy;
                if acc.abs() < SMOOTH_STEP_PX {
                    imp.smooth_dy.set(acc);
                    return glib::Propagation::Stop;
                }
                imp.smooth_dy.set(0.0);
                return pager.wheel(0.0, acc);
            }
            pager.wheel(if rtl { -dx } else { dx }, dy)
        });
        let weak = self.downgrade();
        scroll.connect_scroll_end(move |_| {
            if let Some(pager) = weak.upgrade() {
                pager.imp().smooth_dy.set(0.0);
                pager.swipe_end();
            }
        });
        self.add_controller(scroll);
        // A touch swipe.
        let drag = gtk::GestureDrag::new();
        drag.set_touch_only(true);
        let origin = Rc::new(Cell::new(0.0));
        {
            let (weak, origin) = (self.downgrade(), origin.clone());
            drag.connect_drag_begin(move |_, _, _| {
                if let Some(pager) = weak.upgrade() {
                    pager.swipe_begin();
                    origin.set(pager.imp().position.get());
                }
            });
        }
        {
            let weak = self.downgrade();
            drag.connect_drag_update(move |_, dx, _| {
                if let Some(pager) = weak.upgrade() {
                    let width = f64::from(pager.width().max(1));
                    pager.set_position(origin.get() - dx / width);
                }
            });
        }
        let weak = self.downgrade();
        drag.connect_drag_end(move |_, _, _| {
            if let Some(pager) = weak.upgrade() {
                pager.swipe_end();
            }
        });
        self.add_controller(drag);
    }

    /// GNOME's _onScroll: one page per discrete step, then 150 ms rest.
    fn wheel(&self, dx: f64, dy: f64) -> glib::Propagation {
        let imp = self.imp();
        let step = if dy != 0.0 {
            dy.signum() as i32
        } else if dx != 0.0 {
            dx.signum() as i32
        } else {
            return glib::Propagation::Proceed;
        };
        if !imp.can_scroll.get() {
            return glib::Propagation::Stop;
        }
        self.step(step);
        imp.can_scroll.set(false);
        let weak = self.downgrade();
        glib::timeout_add_local_once(SCROLL_TIMEOUT, move || {
            if let Some(pager) = weak.upgrade() {
                pager.imp().can_scroll.set(true);
            }
        });
        glib::Propagation::Stop
    }
}
