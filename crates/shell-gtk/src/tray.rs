//! AppIndicator tray in the panel (StatusNotifier host).
//!
//! GNOME 51 needs an extension for this; Tuna Desktop ships it natively, as the
//! legacy shell does (ledger P-TR-01). The shell-host `WatcherBus`
//! owns `org.kde.StatusNotifierWatcher` and talks to items. Its calls
//! block, so it lives on a worker thread: the panel only exchanges
//! messages with it, and a hung item can never freeze the shell.

use std::sync::mpsc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::prelude::*;
use tuna_shell_host::watcher::{pick_pixmap, MenuEntry, WatcherBus};

/// How often the worker re-reads the registered items.
const POLL: Duration = Duration::from_secs(2);

/// One item as the panel draws it.
#[derive(Debug, Clone, PartialEq)]
struct Item {
    service: String,
    title: String,
    icon_name: String,
    /// Largest pixmap as big-endian ARGB32 (`width`, `height`, bytes).
    pixmap: Option<(i32, i32, Vec<u8>)>,
}

enum ToWorker {
    Menu(String),
    Fire(String, i32),
    Activate(String),
}

enum FromWorker {
    Items(Vec<Item>),
    Menu(String, Vec<MenuEntry>),
}

fn worker(rx: mpsc::Receiver<ToWorker>, tx: mpsc::Sender<FromWorker>) {
    let mut bus = WatcherBus::new();
    let mut last: Vec<Item> = Vec::new();
    loop {
        // Requests first, then a poll every POLL.
        match rx.recv_timeout(POLL) {
            Ok(ToWorker::Menu(service)) => {
                let menu = bus.fetch_menu(&service);
                if tx.send(FromWorker::Menu(service, menu)).is_err() {
                    return;
                }
                continue;
            }
            Ok(ToWorker::Fire(service, id)) => {
                bus.fire_menu(&service, id);
                continue;
            }
            Ok(ToWorker::Activate(service)) => {
                bus.activate(&service);
                continue;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if !bus.ensure() {
            continue;
        }
        let registered = bus.registered();
        let live = bus.prune_vanished(&registered);
        let gone: Vec<String> = registered
            .iter()
            .filter(|s| !live.contains(s))
            .cloned()
            .collect();
        bus.forget(&gone);
        let items: Vec<Item> = live
            .iter()
            .filter_map(|service| bus.fetch_info(service))
            .filter(|info| info.status != "Passive")
            .map(|info| {
                let attention = info.needs_attention();
                let (name, pixmaps) = if attention && !info.attention_name.is_empty() {
                    (info.attention_name.clone(), info.attention_pixmap.clone())
                } else {
                    (info.icon_name.clone(), info.icon_pixmap.clone())
                };
                Item {
                    service: info.service.clone(),
                    title: info.title.clone(),
                    icon_name: name,
                    pixmap: pick_pixmap(pixmaps),
                }
            })
            .collect();
        if items != last {
            last = items.clone();
            if tx.send(FromWorker::Items(items)).is_err() {
                return;
            }
        }
    }
}

fn icon_for(item: &Item) -> gtk::Image {
    if !item.icon_name.is_empty() {
        let image = gtk::Image::from_icon_name(&item.icon_name);
        if item.icon_name.starts_with('/') {
            image.set_from_file(Some(&item.icon_name));
        }
        return image;
    }
    if let Some((w, h, bytes)) = &item.pixmap {
        let texture = gtk::gdk::MemoryTexture::new(
            *w,
            *h,
            gtk::gdk::MemoryFormat::A8r8g8b8,
            &glib::Bytes::from(bytes),
            *w as usize * 4,
        );
        return gtk::Image::from_paintable(Some(&texture));
    }
    gtk::Image::from_icon_name("image-missing")
}

/// The tray box for the panel's right side. Empty (and invisible)
/// until an item registers.
pub fn tray() -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    row.set_visible(false);
    let (to_worker, worker_rx) = mpsc::channel();
    let (worker_tx, from_worker) = mpsc::channel();
    std::thread::Builder::new()
        .name("tuna-tray".into())
        .spawn(move || worker(worker_rx, worker_tx))
        .expect("spawn tray worker");

    let menus: std::rc::Rc<std::cell::RefCell<Vec<(String, gtk::Popover)>>> = Default::default();
    let row2 = row.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        while let Ok(message) = from_worker.try_recv() {
            match message {
                FromWorker::Items(items) => {
                    while let Some(child) = row2.first_child() {
                        row2.remove(&child);
                    }
                    menus.borrow_mut().clear();
                    for item in &items {
                        let popover = gtk::Popover::new();
                        popover.add_css_class("tuna-shell-popover");
                        popover.set_has_arrow(false);
                        let button = gtk::MenuButton::builder()
                            .child(&icon_for(item))
                            .popover(&popover)
                            .always_show_arrow(false)
                            .build();
                        button.add_css_class("panel-button");
                        let label = if item.title.is_empty() {
                            item.service.as_str()
                        } else {
                            item.title.as_str()
                        };
                        button.update_property(&[gtk::accessible::Property::Label(label)]);
                        // Fresh rows each time the menu opens.
                        let (to, service) = (to_worker.clone(), item.service.clone());
                        popover.connect_show(move |_| {
                            let _ = to.send(ToWorker::Menu(service.clone()));
                        });
                        menus.borrow_mut().push((item.service.clone(), popover));
                        row2.append(&button);
                    }
                    row2.set_visible(!items.is_empty());
                }
                FromWorker::Menu(service, entries) => {
                    let menus = menus.borrow();
                    let Some((_, popover)) = menus.iter().find(|(s, _)| *s == service) else {
                        continue;
                    };
                    let column = gtk::Box::new(gtk::Orientation::Vertical, 2);
                    if entries.is_empty() {
                        // No menu: the item's own activation, as GNOME's
                        // AppIndicator extension does on a primary click.
                        let _ = to_worker.send(ToWorker::Activate(service.clone()));
                        popover.popdown();
                        continue;
                    }
                    for entry in entries {
                        let button = gtk::Button::with_label(&entry.label);
                        button.add_css_class("flat");
                        button.set_sensitive(entry.enabled);
                        let (to, service, popover) =
                            (to_worker.clone(), service.clone(), popover.clone());
                        button.connect_clicked(move |_| {
                            let _ = to.send(ToWorker::Fire(service.clone(), entry.id));
                            popover.popdown();
                        });
                        column.append(&button);
                    }
                    popover.set_child(Some(&column));
                }
            }
        }
        glib::ControlFlow::Continue
    });
    row
}
