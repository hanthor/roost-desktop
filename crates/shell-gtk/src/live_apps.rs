//! The installed-app list, kept current while the shell runs.
//!
//! GNOME lists an app the moment it is installed. Every applications
//! directory is watched; a change marks the list stale, and the next
//! read rescans (so a burst of package-manager writes costs one scan).

use std::cell::{Cell, Ref, RefCell};
use std::rc::Rc;

use gio::prelude::*;
use roost_shell_host::apps::{default_app_dirs, AppProvider};

pub struct LiveApps {
    inner: RefCell<AppProvider>,
    stale: Rc<Cell<bool>>,
    _monitors: Vec<gio::FileMonitor>,
}

impl LiveApps {
    pub fn new() -> Rc<Self> {
        let stale = Rc::new(Cell::new(false));
        let monitors = default_app_dirs()
            .into_iter()
            .filter_map(|dir| {
                let monitor = gio::File::for_path(&dir)
                    .monitor_directory(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>)
                    .ok()?;
                let stale = stale.clone();
                monitor.connect_changed(move |_, _, _, _| stale.set(true));
                Some(monitor)
            })
            .collect();
        Rc::new(Self {
            inner: RefCell::new(AppProvider::system()),
            stale,
            _monitors: monitors,
        })
    }

    /// The current list, rescanned first if an app directory changed.
    pub fn get(&self) -> Ref<'_, AppProvider> {
        if self.stale.get() {
            if let Ok(mut inner) = self.inner.try_borrow_mut() {
                *inner = AppProvider::system();
                self.stale.set(false);
            }
        }
        self.inner.borrow()
    }
}
