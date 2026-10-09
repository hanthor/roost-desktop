//! Stable layer-shell names used by the compositor and both shell frontends.
//!
//! A namespace identifies a surface role; it never authenticates its owner.
//! Keep existing frontend-specific names stable for diagnostics and clients.

/// Layer-shell namespace for the overview surface (compositor ↔ shell contract).
/// The compositor matches on this to identify the overview; the shell advertises it
/// when creating the overview layer.
pub const OVERVIEW_NAMESPACE: &str = "tuna-shell-overview";

/// Layer-shell namespace for the panel surface (compositor ↔ shell contract).
/// The shell advertises this when creating the main panel layer.
pub const PANEL_NAMESPACE: &str = "tuna-shell-panel";

/// Layer-shell namespace for the banner/notifications surface (compositor ↔ shell contract).
/// The shell advertises this when creating the banner/notification layer.
pub const BANNER_NAMESPACE: &str = "tuna-shell-banner";

/// Layer-shell namespace for the legacy shell's dock.
pub const DOCK_NAMESPACE: &str = "tuna-shell-dock";
/// Layer-shell namespace shared by both switcher frontends.
pub const SWITCHER_NAMESPACE: &str = "tuna-shell-switcher";
/// Existing GTK top-bar namespace, distinct from the legacy panel.
pub const GTK_PANEL_NAMESPACE: &str = "tuna-panel";
/// Existing GTK notification-stack namespace, distinct from a legacy banner.
pub const GTK_BANNERS_NAMESPACE: &str = "tuna-shell-banners";
/// GTK switcher thumbnail surface.
pub const SWITCHER_THUMBNAILS_NAMESPACE: &str = "tuna-shell-switcher-thumbnails";
/// GTK folder-name dialog surface.
pub const FOLDER_DIALOG_NAMESPACE: &str = "tuna-folder-dialog";
/// GTK IBus candidate surface.
pub const IBUS_CANDIDATES_NAMESPACE: &str = "tuna-ibus-candidates";
/// GTK screenshot selection surface.
pub const SCREENSHOT_NAMESPACE: &str = "tuna-screenshot-ui";
/// GTK shortcut consent surface.
pub const SHORTCUT_CONSENT_NAMESPACE: &str = "tuna-shortcut-consent";
/// GTK window-menu surface.
pub const WINDOW_MENU_NAMESPACE: &str = "tuna-window-menu";
/// GTK workspace popup surface.
pub const WORKSPACE_POPUP_NAMESPACE: &str = "tuna-workspace-popup";
/// GTK network-agent prompt surface.
pub const NETWORK_AGENT_NAMESPACE: &str = "tuna-network-agent";
/// GTK on-screen display surface.
pub const OSD_NAMESPACE: &str = "tuna-osd";
/// GTK PolicyKit authentication surface.
pub const POLKIT_NAMESPACE: &str = "tuna-polkit";
/// GTK end-session dialog surface.
pub const END_SESSION_NAMESPACE: &str = "tuna-shell-end-session";
/// GTK overview preview chrome surface.
pub const PREVIEW_CHROME_NAMESPACE: &str = "tuna-shell-preview-chrome";
