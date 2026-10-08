//! The GNOME 51 protocols Roost added to close P-SY-06's gaps (#89):
//! presentation-time, fifo, keyboard-shortcuts-inhibit, xdg-dialog,
//! xdg-foreign, xdg-system-bell and xdg-toplevel-tag, driven by a real
//! client against the headless compositor.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;

use roost_compositor::windows::{
    ManagerInput, WindowManager, ESCAPE_KEYCODE, PAGE_DOWN_KEYCODE, SUPER_LEFT_KEYCODE,
};
use roost_compositor::TestCompositor;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use wayland_client::{
    protocol::{
        wl_callback::{self, WlCallback},
        wl_compositor::WlCompositor,
        wl_registry::{self, WlRegistry},
        wl_seat::WlSeat,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols::wp::fifo::v1::client::{
    wp_fifo_manager_v1::WpFifoManagerV1, wp_fifo_v1::WpFifoV1,
};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
    zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
    zwp_keyboard_shortcuts_inhibitor_v1::{self, ZwpKeyboardShortcutsInhibitorV1},
};
use wayland_protocols::wp::presentation_time::client::{
    wp_presentation::{self, WpPresentation},
    wp_presentation_feedback::{self, WpPresentationFeedback},
};
use wayland_protocols::xdg::dialog::v1::client::{
    xdg_dialog_v1::XdgDialogV1, xdg_wm_dialog_v1::XdgWmDialogV1,
};
use wayland_protocols::xdg::foreign::zv2::client::{
    zxdg_exported_v2::{self, ZxdgExportedV2},
    zxdg_exporter_v2::ZxdgExporterV2,
    zxdg_imported_v2::ZxdgImportedV2,
    zxdg_importer_v2::ZxdgImporterV2,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};
use wayland_protocols::xdg::system_bell::v1::client::xdg_system_bell_v1::XdgSystemBellV1;
use wayland_protocols::xdg::toplevel_tag::v1::client::xdg_toplevel_tag_manager_v1::XdgToplevelTagManagerV1;

const ROUNDS: usize = 10;

/// What a feedback object (by its tag) was told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    Presented { seq: u64, refresh: u32 },
    Discarded,
}

#[derive(Default)]
struct Client {
    frames: usize,
    globals: HashMap<String, (u32, u32)>,
    clock_id: Option<u32>,
    fates: HashMap<u32, Fate>,
    inhibitor_active: Option<bool>,
    handle: Option<String>,
}

impl Dispatch<WlCallback, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.frames += 1;
        }
    }
}

impl Client {
    fn bind<I>(&self, registry: &WlRegistry, qh: &QueueHandle<Self>, version: u32) -> I
    where
        I: wayland_client::Proxy + 'static,
        Self: Dispatch<I, ()>,
    {
        let (name, offered) = self.globals[I::interface().name];
        registry.bind(name, version.min(offered), qh, ())
    }
}

impl Dispatch<WlRegistry, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            state.globals.insert(interface, (name, version));
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Client {
    fn event(
        _: &mut Self,
        wm: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Client {
    fn event(
        _: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
        }
    }
}

impl Dispatch<WpPresentation, ()> for Client {
    fn event(
        state: &mut Self,
        _: &WpPresentation,
        event: wp_presentation::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            state.clock_id = Some(clk_id);
        }
    }
}

impl Dispatch<WpPresentationFeedback, u32> for Client {
    fn event(
        state: &mut Self,
        _: &WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        tag: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_presentation_feedback::Event::Presented {
                refresh,
                seq_hi,
                seq_lo,
                ..
            } => {
                let seq = (u64::from(seq_hi) << 32) | u64::from(seq_lo);
                state.fates.insert(*tag, Fate::Presented { seq, refresh });
            }
            wp_presentation_feedback::Event::Discarded => {
                state.fates.insert(*tag, Fate::Discarded);
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpKeyboardShortcutsInhibitorV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &ZwpKeyboardShortcutsInhibitorV1,
        event: zwp_keyboard_shortcuts_inhibitor_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Active => {
                state.inhibitor_active = Some(true)
            }
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Inactive => {
                state.inhibitor_active = Some(false)
            }
            _ => {}
        }
    }
}

impl Dispatch<ZxdgExportedV2, ()> for Client {
    fn event(
        state: &mut Self,
        _: &ZxdgExportedV2,
        event: zxdg_exported_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_exported_v2::Event::Handle { handle } = event {
            state.handle = Some(handle);
        }
    }
}

wayland_client::delegate_noop!(Client: ignore WlCompositor);
wayland_client::delegate_noop!(Client: ignore WlSurface);
wayland_client::delegate_noop!(Client: ignore WlSeat);
wayland_client::delegate_noop!(Client: ignore XdgToplevel);
wayland_client::delegate_noop!(Client: ignore WpFifoManagerV1);
wayland_client::delegate_noop!(Client: ignore WpFifoV1);
wayland_client::delegate_noop!(Client: ignore ZwpKeyboardShortcutsInhibitManagerV1);
wayland_client::delegate_noop!(Client: ignore XdgSystemBellV1);
wayland_client::delegate_noop!(Client: ignore XdgWmDialogV1);
wayland_client::delegate_noop!(Client: ignore XdgDialogV1);
wayland_client::delegate_noop!(Client: ignore XdgToplevelTagManagerV1);
wayland_client::delegate_noop!(Client: ignore ZxdgExporterV2);
wayland_client::delegate_noop!(Client: ignore ZxdgImporterV2);
wayland_client::delegate_noop!(Client: ignore ZxdgImportedV2);

struct Peer {
    _conn: Connection,
    queue: EventQueue<Client>,
    registry: WlRegistry,
    client: Client,
}

impl Peer {
    fn qh(&self) -> QueueHandle<Client> {
        self.queue.handle()
    }

    fn bind<I>(&self, version: u32) -> I
    where
        I: wayland_client::Proxy + 'static,
        Client: Dispatch<I, ()>,
    {
        self.client.bind(&self.registry, &self.qh(), version)
    }
}

/// A compositor with one 60 Hz output, as the nested session has.
fn compositor() -> (TestCompositor, WindowManager) {
    let mut comp = TestCompositor::new();
    let output = Output::new(
        "roost-0".to_owned(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Tuna Desktop".to_owned(),
            model: "Test".to_owned(),
        },
    );
    let mode = Mode {
        size: (1280, 800).into(),
        refresh: 60_000,
    };
    output.change_current_state(Some(mode), None, None, Some((0, 0).into()));
    comp.state.add_output("roost-0", Some(output), 1280, 800);
    let manager = comp.window_manager();
    (comp, manager)
}

fn connect(comp: &mut TestCompositor, manager: &mut WindowManager) -> Peer {
    let (server, stream) = UnixStream::pair().unwrap();
    comp.add_client(server);
    let conn = Connection::from_socket(stream).unwrap();
    let queue = conn.new_event_queue();
    let registry = conn.display().get_registry(&queue.handle(), ());
    let mut peer = Peer {
        _conn: conn,
        queue,
        registry,
        client: Client::default(),
    };
    pump(comp, manager, &mut [&mut peer]);
    peer
}

fn pump(comp: &mut TestCompositor, manager: &mut WindowManager, peers: &mut [&mut Peer]) {
    for _ in 0..ROUNDS {
        for p in peers.iter_mut() {
            p.queue.flush().unwrap();
        }
        comp.pump();
        manager.reconcile(&mut comp.state);
        comp.pump();
        for p in peers.iter_mut() {
            if let Some(guard) = p.queue.prepare_read() {
                let _ = guard.read();
            }
            p.queue.dispatch_pending(&mut p.client).unwrap();
        }
    }
}

/// Map a toplevel titled `title`; returns its surface and role.
fn window(peer: &mut Peer, title: &str) -> (WlSurface, XdgToplevel) {
    let qh = peer.qh();
    let compositor: WlCompositor = peer.bind(6);
    let wm: XdgWmBase = peer.bind(6);
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title(title.into());
    surface.commit();
    // The protocol objects live with the connection.
    std::mem::forget((compositor, wm, xdg));
    (surface, toplevel)
}

fn id_of(manager: &WindowManager, title: &str) -> u64 {
    manager
        .model()
        .windows()
        .find(|w| w.title == title)
        .map(|w| w.id)
        .expect("window mapped")
}

fn focused_title(manager: &WindowManager) -> Option<String> {
    let id = manager.model().focused()?;
    manager
        .model()
        .windows()
        .find(|w| w.id == id)
        .map(|w| w.title.clone())
}

fn key(manager: &mut WindowManager, comp: &mut TestCompositor, keycode: u32, pressed: bool) {
    manager.on_input(
        &mut comp.state,
        ManagerInput::Key {
            keycode,
            pressed,
            time: 5000,
        },
    );
}

fn chord(manager: &mut WindowManager, comp: &mut TestCompositor, keycode: u32) {
    key(manager, comp, SUPER_LEFT_KEYCODE, true);
    key(manager, comp, keycode, true);
    key(manager, comp, keycode, false);
    key(manager, comp, SUPER_LEFT_KEYCODE, false);
}

#[test]
fn a_frame_request_without_pixel_damage_keeps_native_refresh_work_pending() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let (surface, _toplevel) = window(&mut peer, "idle client");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let roots = roost_compositor::frame_timing::frame_roots(&comp.state, &manager);
    assert!(!roost_compositor::frame_timing::pending_frame_work(&roots));
    let _callback = surface.frame(&peer.qh(), ());
    // No buffer attachment or damage: this is a request for pacing alone.
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(roost_compositor::frame_timing::pending_frame_work(&roots));
    assert!(
        roost_compositor::frame_timing::pending_output_work(&roots, &[]),
        "callback-only pacing remains owed even without visible presentation"
    );
    assert_eq!(peer.client.frames, 0);
    let output = comp.state.primary_output().unwrap();
    for root in &roots {
        smithay::desktop::utils::send_frames_surface_tree(
            root,
            &output,
            std::time::Duration::from_millis(20),
            Some(std::time::Duration::ZERO),
            |_, _| Some(output.clone()),
        );
    }
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(peer.client.frames, 1);
    assert!(!roost_compositor::frame_timing::pending_frame_work(&roots));
}

#[test]
fn presentation_feedback_reports_the_frame_that_drew_the_window() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let presentation: WpPresentation = peer.bind(2);
    let (surface, _toplevel) = window(&mut peer, "video");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(
        peer.client.clock_id,
        Some(libc::CLOCK_MONOTONIC as u32),
        "Mutter reports on CLOCK_MONOTONIC"
    );

    let _feedback = presentation.feedback(&surface, &peer.qh(), 1);
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(peer.client.fates.is_empty(), "nothing drawn yet");
    let roots = roost_compositor::frame_timing::frame_roots(&comp.state, &manager);
    assert!(roost_compositor::frame_timing::pending_frame_work(&roots));

    roost_compositor::frame_timing::present_nested_frame(&mut comp.state, &manager, false, 42);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(
        peer.client.fates.get(&1),
        Some(&Fate::Presented {
            seq: 42,
            refresh: 16_666_666,
        })
    );
    assert!(!roost_compositor::frame_timing::pending_frame_work(&roots));
}

#[test]
fn a_hidden_window_keeps_its_feedback_until_drawn() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let presentation: WpPresentation = peer.bind(2);
    let (surface, _toplevel) = window(&mut peer, "video");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let _feedback = presentation.feedback(&surface, &peer.qh(), 7);
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);

    // Locked: only the lock screen is drawn.
    roost_compositor::frame_timing::present_nested_frame(&mut comp.state, &manager, true, 1);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(peer.client.fates.get(&7), None);
    let roots = roost_compositor::frame_timing::frame_roots(&comp.state, &manager);
    assert!(roost_compositor::frame_timing::pending_frame_work(&roots));
    assert!(
        !roost_compositor::frame_timing::pending_output_work(&roots, &[]),
        "hidden presentation must not keep an unchanged output repainting"
    );
    let drawn: Vec<_> = roost_compositor::frame_timing::drawn_roots(&comp.state, &manager, false)
        .into_iter()
        .map(|(surface, _)| surface)
        .collect();
    assert!(
        roost_compositor::frame_timing::pending_output_work(&roots, &drawn),
        "revealing the actual window owes a real submission"
    );
    assert_eq!(
        peer.client.fates.get(&7),
        None,
        "visibility check does not resolve feedback"
    );

    roost_compositor::frame_timing::present_nested_frame(&mut comp.state, &manager, false, 2);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(matches!(
        peer.client.fates.get(&7),
        Some(Fate::Presented { seq: 2, .. })
    ));
}

#[test]
fn fifo_holds_a_waiting_commit_until_the_next_refresh() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let presentation: WpPresentation = peer.bind(2);
    let fifo_manager: WpFifoManagerV1 = peer.bind(1);
    let (surface, _toplevel) = window(&mut peer, "game");
    pump(&mut comp, &mut manager, &mut [&mut peer]);

    let fifo = fifo_manager.get_fifo(&surface, &peer.qh(), ());
    fifo.set_barrier();
    surface.commit();
    // The next update waits for a refresh after the barrier.
    fifo.wait_barrier();
    fifo.set_barrier();
    let _feedback = presentation.feedback(&surface, &peer.qh(), 2);
    surface.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);

    // The first refresh draws what was applied (not the held update),
    // then releases the barrier.
    let roots = roost_compositor::frame_timing::frame_roots(&comp.state, &manager);
    assert!(roost_compositor::frame_timing::pending_frame_work(&roots));
    roost_compositor::frame_timing::present_nested_frame(&mut comp.state, &manager, false, 1);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(peer.client.fates.get(&2), None, "held behind the barrier");

    roost_compositor::frame_timing::present_nested_frame(&mut comp.state, &manager, false, 2);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(matches!(
        peer.client.fates.get(&2),
        Some(Fate::Presented { seq: 2, .. })
    ));
}

#[test]
fn an_inhibiting_window_gets_the_shortcuts_until_super_escape() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let seat: WlSeat = peer.bind(7);
    let inhibit: ZwpKeyboardShortcutsInhibitManagerV1 = peer.bind(1);
    let (surface, _toplevel) = window(&mut peer, "vm");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(focused_title(&manager).as_deref(), Some("vm"));

    let _inhibitor = inhibit.inhibit_shortcuts(&surface, &seat, &peer.qh(), ());
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(peer.client.inhibitor_active, Some(false));
    assert!(!comp.state.shortcuts_inhibited());
    let request = comp.state.shortcut_consent_request().unwrap().0;
    comp.state.answer_shortcut_consent(request, true);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(peer.client.inhibitor_active, Some(true));
    assert!(comp.state.shortcuts_inhibited());

    // Super+PageDown goes to the virtual machine, not the workspaces.
    chord(&mut manager, &mut comp, PAGE_DOWN_KEYCODE);
    assert_eq!(manager.model().active_workspace(), 0);

    // Registered shell grabs must obey the same consent. This catches a
    // global accelerator bypass even when built-in workspace keys work.
    manager.set_accelerators(vec![roost_shell_control::Accelerator {
        action: 77,
        keysym: 0xff56, // Page_Down
        mods: roost_shell_control::MOD_LOGO,
        modes: roost_shell_control::MODE_NORMAL,
    }]);
    chord(&mut manager, &mut comp, PAGE_DOWN_KEYCODE);
    assert!(manager.take_accelerators_fired().is_empty());

    // Super+Escape (Mutter's restore-shortcuts) takes them back.
    chord(&mut manager, &mut comp, ESCAPE_KEYCODE);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert!(!comp.state.shortcuts_inhibited());
    assert_eq!(peer.client.inhibitor_active, Some(false));
    chord(&mut manager, &mut comp, PAGE_DOWN_KEYCODE);
    assert_eq!(
        manager.take_accelerators_fired(),
        [(77, 5000, roost_shell_control::MODE_NORMAL)]
    );
    manager.set_accelerators(vec![]);
    chord(&mut manager, &mut comp, PAGE_DOWN_KEYCODE);
    assert_eq!(manager.model().active_workspace(), 1);
}

#[test]
fn denied_shortcut_inhibition_stays_denied_after_refocus() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let seat: WlSeat = peer.bind(7);
    let inhibit: ZwpKeyboardShortcutsInhibitManagerV1 = peer.bind(1);
    let (surface, _) = window(&mut peer, "denied-vm");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let _inhibitor = inhibit.inhibit_shortcuts(&surface, &seat, &peer.qh(), ());
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let request = comp.state.shortcut_consent_request().unwrap().0;
    comp.state.answer_shortcut_consent(request, false);
    let vm = id_of(&manager, "denied-vm");
    let _other = window(&mut peer, "other");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    manager.focus(&mut comp.state, Some(vm));
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(peer.client.inhibitor_active, Some(false));
    assert!(!comp.state.shortcuts_inhibited());
    chord(&mut manager, &mut comp, PAGE_DOWN_KEYCODE);
    assert_eq!(manager.model().active_workspace(), 1);
}

#[test]
fn shortcut_consent_cannot_survive_a_lock_or_focus_change() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let seat: WlSeat = peer.bind(7);
    let inhibit: ZwpKeyboardShortcutsInhibitManagerV1 = peer.bind(1);
    let (surface, _) = window(&mut peer, "vm");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let inhibitor = inhibit.inhibit_shortcuts(&surface, &seat, &peer.qh(), ());
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let request = comp.state.shortcut_consent_request().unwrap().0;
    let _other = window(&mut peer, "other");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    comp.state.answer_shortcut_consent(request, true);
    let vm = id_of(&manager, "vm");
    manager.focus(&mut comp.state, Some(vm));
    assert!(!comp.state.shortcuts_inhibited());
    assert!(comp.state.shortcut_consent_request().is_none());
    inhibitor.destroy();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let _inhibitor = inhibit.inhibit_shortcuts(&surface, &seat, &peer.qh(), ());
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let request = comp.state.shortcut_consent_request().unwrap().0;
    comp.state.set_shortcut_inhibition_locked(true);
    comp.state.answer_shortcut_consent(request, true);
    comp.state.set_shortcut_inhibition_locked(false);
    assert!(!comp.state.shortcuts_inhibited());
    assert!(comp.state.shortcut_consent_request().is_none());
}

#[test]
fn modal_dialogs_keep_focus_from_their_parent() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let dialogs: XdgWmDialogV1 = peer.bind(1);
    let (_main, main_toplevel) = window(&mut peer, "main");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let (child, child_toplevel) = window(&mut peer, "save");
    child_toplevel.set_parent(Some(&main_toplevel));
    let dialog = dialogs.get_xdg_dialog(&child_toplevel, &peer.qh(), ());
    dialog.set_modal();
    child.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);

    let main = id_of(&manager, "main");
    manager.focus(&mut comp.state, Some(main));
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(focused_title(&manager).as_deref(), Some("save"));

    // No longer modal: the parent may take focus again.
    dialog.unset_modal();
    child.commit();
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    manager.focus(&mut comp.state, Some(main));
    assert_eq!(focused_title(&manager).as_deref(), Some("main"));
}

#[test]
fn exported_windows_parent_another_clients_dialog() {
    let (mut comp, mut manager) = compositor();
    let mut app = connect(&mut comp, &mut manager);
    let mut portal = connect(&mut comp, &mut manager);
    let exporter: ZxdgExporterV2 = app.bind(1);
    let importer: ZxdgImporterV2 = portal.bind(1);
    let (app_surface, _app_toplevel) = window(&mut app, "app");
    let (dialog_surface, _dialog_toplevel) = window(&mut portal, "file chooser");
    pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);

    let _exported = exporter.export_toplevel(&app_surface, &app.qh(), ());
    pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);
    let handle = app.client.handle.clone().expect("export handle");
    let imported = importer.import_toplevel(handle, &portal.qh(), ());
    imported.set_parent_of(&dialog_surface);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);

    let toplevels = comp.state.toplevels();
    assert_eq!(toplevels.len(), 2);
    assert!(
        toplevels.iter().any(|child| child
            .parent()
            .is_some_and(|parent| toplevels.iter().any(|t| *t.wl_surface() == parent))),
        "the portal's dialog is parented to the app's window"
    );
}

#[test]
fn running_app_eligibility_follows_real_transient_parent_changes() {
    let (mut comp, mut manager) = compositor();
    let mut app = connect(&mut comp, &mut manager);
    let mut portal = connect(&mut comp, &mut manager);
    let exporter: ZxdgExporterV2 = app.bind(1);
    let importer: ZxdgImporterV2 = portal.bind(1);
    let (parent_surface, parent) = window(&mut app, "app parent");
    let (_child_surface, child) = window(&mut app, "app dialog");
    let (picker_surface, picker) = window(&mut portal, "portal picker");
    parent.set_app_id("org.gnome.TextEditor".into());
    child.set_app_id("org.gnome.EditorDialog".into());
    picker.set_app_id("org.gnome.Nautilus".into());
    pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);
    let id = |manager: &WindowManager, title: &str| {
        manager
            .model()
            .windows()
            .find(|w| w.title == title)
            .unwrap()
            .id
    };
    let parent_id = id(&manager, "app parent");
    let child_id = id(&manager, "app dialog");
    let picker_id = id(&manager, "portal picker");
    assert!(manager.is_standalone(child_id));
    assert!(manager.is_standalone(picker_id));
    child.set_parent(Some(&parent));
    pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);
    assert!(!manager.is_standalone(child_id));
    assert!(
        manager.is_introspect_eligible(child_id),
        "native dialogs remain selectable"
    );
    assert_eq!(manager.application_window(child_id), Some(parent_id));
    child.set_parent(None);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);
    assert!(manager.is_standalone(child_id));
    let exported = exporter.export_toplevel(&parent_surface, &app.qh(), ());
    pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);
    let imported = importer.import_toplevel(app.client.handle.clone().unwrap(), &portal.qh(), ());
    imported.set_parent_of(&picker_surface);
    pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);
    assert!(!manager.is_standalone(picker_id));
    assert!(
        manager.is_introspect_eligible(picker_id),
        "foreign-parented pickers remain selectable"
    );
    assert_eq!(manager.application_window(picker_id), Some(parent_id));
    assert_eq!(
        manager.model().window(picker_id).unwrap().app_id.as_deref(),
        Some("org.gnome.Nautilus")
    );
    exported.destroy();
    pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);
    assert!(
        manager.is_standalone(picker_id),
        "withdrawn foreign parent clears eligibility"
    );
    assert_eq!(manager.application_window(picker_id), Some(picker_id));
    picker.destroy();
    picker_surface.destroy();
    pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);
    assert!(
        !manager.is_standalone(picker_id),
        "unmapped window is not a running app"
    );
    assert_eq!(manager.application_window(picker_id), None);
    assert!(!manager.is_introspect_eligible(picker_id));
    assert!(!manager.is_introspect_eligible(u64::MAX));
}

#[test]
fn the_system_bell_rings() {
    // No sound from the test run.
    std::env::set_var("ROOST_BELL", "0");
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let bell: XdgSystemBellV1 = peer.bind(1);
    let (surface, _toplevel) = window(&mut peer, "terminal");
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(comp.state.bell_rings(), 0);
    bell.ring(Some(&surface));
    bell.ring(None);
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    assert_eq!(comp.state.bell_rings(), 2);
}

#[test]
fn windows_keep_their_toplevel_tag() {
    let (mut comp, mut manager) = compositor();
    let mut peer = connect(&mut comp, &mut manager);
    let tags: XdgToplevelTagManagerV1 = peer.bind(1);
    let (_surface, toplevel) = window(&mut peer, "mail");
    tags.set_toplevel_tag(&toplevel, "composer".into());
    tags.set_toplevel_description(&toplevel, "New message".into());
    pump(&mut comp, &mut manager, &mut [&mut peer]);
    let surface = comp.state.toplevels()[0].wl_surface().clone();
    let tag = roost_compositor::protocols::toplevel_tag(&surface);
    assert_eq!(tag.tag.as_deref(), Some("composer"));
    assert_eq!(tag.description.as_deref(), Some("New message"));
}

#[test]
fn parented_dialogs_remain_centered_on_left_and_upper_outputs() {
    for parent_location in [(-1100, 50), (100, -750)] {
        for foreign in [false, true] {
            let (mut comp, mut manager) = compositor();
            for (name, location) in [("left", (-1280, 0)), ("above", (0, -800))] {
                let output = Output::new(
                    name.into(),
                    PhysicalProperties {
                        size: (0, 0).into(),
                        subpixel: Subpixel::Unknown,
                        make: "Tuna Desktop".into(),
                        model: "Dialog topology test".into(),
                    },
                );
                output.change_current_state(
                    Some(Mode {
                        size: (1280, 800).into(),
                        refresh: 60_000,
                    }),
                    None,
                    None,
                    Some(location.into()),
                );
                comp.state.add_output(name, Some(output), 1280, 800);
                comp.state.set_output_location(name, location);
            }
            let mut app = connect(&mut comp, &mut manager);
            let (parent_surface, parent_toplevel) = window(&mut app, "negative-origin parent");
            pump(&mut comp, &mut manager, &mut [&mut app]);
            let parent = id_of(&manager, "negative-origin parent");
            let initial = manager.geometry(parent).unwrap();
            assert!(manager.move_window(
                parent,
                parent_location.0 - initial.loc.x,
                parent_location.1 - initial.loc.y
            ));
            let parent_geometry = manager.geometry(parent).unwrap();
            let mut portal_peer = None;
            if foreign {
                let exporter: ZxdgExporterV2 = app.bind(1);
                let _exported = exporter.export_toplevel(&parent_surface, &app.qh(), ());
                pump(&mut comp, &mut manager, &mut [&mut app]);
                let mut portal = connect(&mut comp, &mut manager);
                let importer: ZxdgImporterV2 = portal.bind(1);
                let imported =
                    importer.import_toplevel(app.client.handle.clone().unwrap(), &portal.qh(), ());
                let (surface, _) = window(&mut portal, "negative-origin chooser");
                imported.set_parent_of(&surface);
                surface.commit();
                pump(&mut comp, &mut manager, &mut [&mut app, &mut portal]);
                portal_peer = Some(portal);
            } else {
                let (surface, toplevel) = window(&mut app, "negative-origin chooser");
                toplevel.set_parent(Some(&parent_toplevel));
                surface.commit();
                pump(&mut comp, &mut manager, &mut [&mut app]);
            }
            let child = id_of(&manager, "negative-origin chooser");
            let child_geometry = manager.geometry(child).unwrap();
            let expected = (
                parent_geometry.loc.x + (parent_geometry.size.w - child_geometry.size.w) / 2,
                parent_geometry.loc.y + (parent_geometry.size.h - child_geometry.size.h) / 2,
            );
            assert_eq!(
                (child_geometry.loc.x, child_geometry.loc.y),
                expected,
                "foreign={foreign}, parent={parent_location:?}"
            );
            assert_eq!(manager.geometry(parent).unwrap(), parent_geometry);
            drop(portal_peer);
        }
    }
}
