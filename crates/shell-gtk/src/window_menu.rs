//! GNOME 51's window menu (windowMenu.js), opened by a header bar's
//! right click (xdg_toplevel.show_window_menu): Take Screenshot, Hide,
//! Maximize or Restore, Move, Resize, Always on Top, Always on Visible
//! Workspace, Move to Workspace Left/Right, Close. Drawn as GNOME's popup
//! menu 7px under the click, first item focused.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use roost_shell_control::WindowAction;
use roost_shell_host::control::WindowMenuRequest;

/// What an item does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Screenshot,
    Action(WindowAction),
    Close,
}

/// One row of the menu: label, what it does, sensitive, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub label: &'static str,
    pub item: Option<Item>,
    pub sensitive: bool,
    pub checked: bool,
}

/// GNOME's rows for this window (`None` item: a separator).
pub fn rows(req: &WindowMenuRequest) -> Vec<Row> {
    let row = |label, item, sensitive, checked| Row {
        label,
        item: Some(item),
        sensitive,
        checked,
    };
    let mut rows = vec![
        row("Take Screenshot", Item::Screenshot, true, false),
        row("Hide", Item::Action(WindowAction::Minimize), true, false),
        row(
            if req.maximized { "Restore" } else { "Maximize" },
            Item::Action(WindowAction::ToggleMaximize),
            true,
            false,
        ),
        row("Move", Item::Action(WindowAction::Move), true, false),
        row("Resize", Item::Action(WindowAction::Resize), true, false),
        // GNOME greys Always on Top out for a maximized window.
        row(
            "Always on Top",
            Item::Action(WindowAction::ToggleAbove),
            !req.maximized,
            req.above,
        ),
        // Sticky windows are not supported yet: shown, but unavailable.
        Row {
            label: "Always on Visible Workspace",
            item: None,
            sensitive: false,
            checked: false,
        },
    ];
    if req.workspace_left {
        rows.push(row(
            "Move to Workspace Left",
            Item::Action(WindowAction::MoveToWorkspaceLeft),
            true,
            false,
        ));
    }
    if req.workspace_right {
        rows.push(row(
            "Move to Workspace Right",
            Item::Action(WindowAction::MoveToWorkspaceRight),
            true,
            false,
        ));
    }
    rows.push(Row {
        label: "",
        item: None,
        sensitive: false,
        checked: false,
    });
    rows.push(row("Close", Item::Close, true, false));
    rows
}

/// Carries out a chosen item on a window.
pub type Run = Rc<dyn Fn(u64, Item)>;

pub struct WindowMenu {
    window: gtk::Window,
    fixed: gtk::Fixed,
    run: Run,
    shown: RefCell<Option<gtk::Box>>,
}

impl WindowMenu {
    pub fn new(app: &gtk::Application, run: Run) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.add_css_class("roost-window-menu");
        window.set_title(Some("Window Menu"));
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some("roost-window-menu"));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        let fixed = gtk::Fixed::new();
        window.set_child(Some(&fixed));
        let ui = Rc::new(Self {
            window,
            fixed,
            run,
            shown: RefCell::new(None),
        });
        // A click anywhere but the menu, or Escape, closes it.
        let click = gtk::GestureClick::new();
        click.set_button(0);
        {
            let weak = Rc::downgrade(&ui);
            click.connect_pressed(move |gesture, _, x, y| {
                let Some(ui) = weak.upgrade() else { return };
                let inside = ui.shown.borrow().as_ref().is_some_and(|menu| {
                    menu.compute_bounds(&ui.window).is_some_and(|b| {
                        b.contains_point(&gtk::graphene::Point::new(x as f32, y as f32))
                    })
                });
                if !inside {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    ui.close();
                }
            });
        }
        ui.window.add_controller(click);
        let keys = gtk::EventControllerKey::new();
        {
            let weak = Rc::downgrade(&ui);
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape {
                    if let Some(ui) = weak.upgrade() {
                        ui.close();
                    }
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }
        ui.window.add_controller(keys);
        ui
    }

    /// Open GNOME's menu for `req`.
    pub fn open(self: &Rc<Self>, req: &WindowMenuRequest) {
        if let Some(old) = self.shown.borrow_mut().take() {
            self.fixed.remove(&old);
        }
        let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
        menu.add_css_class("popup-menu-content");
        let mut first: Option<gtk::Button> = None;
        for row in rows(req) {
            if row.label.is_empty() {
                let sep = gtk::Box::new(gtk::Orientation::Vertical, 0);
                sep.add_css_class("popup-separator-menu-item");
                let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                line.add_css_class("popup-separator-menu-item-separator");
                line.set_valign(gtk::Align::Center);
                line.set_vexpand(true);
                sep.append(&line);
                menu.append(&sep);
                continue;
            };
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let ornament = gtk::Image::new();
            ornament.add_css_class("popup-menu-ornament");
            ornament.set_pixel_size(16);
            if row.checked {
                ornament.set_icon_name(Some("object-select-symbolic"));
            }
            content.append(&ornament);
            let label = gtk::Label::new(Some(row.label));
            label.set_xalign(0.0);
            content.append(&label);
            let button = gtk::Button::builder().child(&content).build();
            button.add_css_class("popup-menu-item");
            // Rows without an action of ours stay visible but greyed.
            let sensitive = row.sensitive && row.item.is_some();
            button.set_sensitive(sensitive);
            if let Some(item) = row.item.filter(|_| sensitive) {
                let (weak, window) = (Rc::downgrade(self), req.window);
                button.connect_clicked(move |_| {
                    if let Some(ui) = weak.upgrade() {
                        ui.close();
                        (ui.run)(window, item);
                    }
                });
                if first.is_none() {
                    first = Some(button.clone());
                }
            }
            menu.append(&button);
        }
        // 7px below the click (the boxpointer's 6px rise and its edge),
        // kept on the screen.
        let (_, natural) = menu.preferred_size();
        let (w, h) = (natural.width(), natural.height());
        let screen = WidgetExt::display(&self.window)
            .monitors()
            .item(0)
            .and_then(|m| m.downcast::<gtk::gdk::Monitor>().ok())
            .map(|m| m.geometry())
            .map(|g| (g.width(), g.height()))
            .unwrap_or((1280, 800));
        let x = req.x.min(screen.0 - w).max(0);
        let y = (req.y + 7).min(screen.1 - h).max(0);
        self.fixed.put(&menu, f64::from(x), f64::from(y));
        *self.shown.borrow_mut() = Some(menu);
        self.window.present();
        if let Some(first) = first {
            first.grab_focus();
        }
    }

    fn close(&self) {
        if let Some(old) = self.shown.borrow_mut().take() {
            self.fixed.remove(&old);
        }
        self.window.set_visible(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(maximized: bool, left: bool) -> WindowMenuRequest {
        WindowMenuRequest {
            window: 7,
            x: 500,
            y: 280,
            maximized,
            above: false,
            workspace_left: left,
            workspace_right: true,
        }
    }

    #[test]
    fn rows_follow_gnome_51() {
        let labels: Vec<&str> = rows(&req(false, false)).iter().map(|r| r.label).collect();
        assert_eq!(
            labels,
            [
                "Take Screenshot",
                "Hide",
                "Maximize",
                "Move",
                "Resize",
                "Always on Top",
                "Always on Visible Workspace",
                "Move to Workspace Right",
                "",
                "Close",
            ]
        );
        let maximized = rows(&req(true, true));
        assert_eq!(maximized[2].label, "Restore");
        assert!(
            !maximized[5].sensitive,
            "Always on Top greys out when maximized"
        );
        assert!(maximized
            .iter()
            .any(|r| r.label == "Move to Workspace Left"));
    }
}
