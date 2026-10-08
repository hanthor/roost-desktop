//! Unified error types for the shell-host crate.

use std::fmt;

pub use crate::control::ControlError;
pub use crate::notifications::NotificationError;
pub use crate::panel::PanelError;

/// Unified error enum representing failures across the shell-host subsystem.
#[derive(Debug)]
pub enum ShellHostError {
    /// Wayland, layer-shell, or panel lifecycle failures.
    Panel(PanelError),
    /// Compositor control IPC transport or protocol failures.
    Control(ControlError),
    /// Desktop notification service and action failures.
    Notification(NotificationError),
    /// Generic startup or runtime I/O failure.
    Io(std::io::Error),
}

impl fmt::Display for ShellHostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Panel(err) => write!(f, "panel error: {err}"),
            Self::Control(err) => write!(f, "control error: {err}"),
            Self::Notification(err) => write!(f, "notification error: {err}"),
            Self::Io(err) => write!(f, "I/O error: {err}"),
        }
    }
}

impl std::error::Error for ShellHostError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Panel(err) => Some(err),
            Self::Control(err) => Some(err),
            Self::Notification(err) => Some(err),
            Self::Io(err) => Some(err),
        }
    }
}

impl From<PanelError> for ShellHostError {
    fn from(err: PanelError) -> Self {
        Self::Panel(err)
    }
}

impl From<ControlError> for ShellHostError {
    fn from(err: ControlError) -> Self {
        Self::Control(err)
    }
}

impl From<NotificationError> for ShellHostError {
    fn from(err: NotificationError) -> Self {
        Self::Notification(err)
    }
}

impl From<std::io::Error> for ShellHostError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions_and_source() {
        use std::error::Error;

        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "socket missing");
        let shell_err: ShellHostError = io_err.into();
        assert!(matches!(shell_err, ShellHostError::Io(_)));
        assert!(shell_err.source().is_some());
        assert!(shell_err.to_string().contains("socket missing"));

        let notif_err = NotificationError::UnknownNotification;
        let shell_err: ShellHostError = notif_err.into();
        assert!(matches!(shell_err, ShellHostError::Notification(_)));
        assert!(shell_err.source().is_some());
        assert!(shell_err.to_string().contains("unknown notification"));

        let ctrl_err = ControlError::Unexpected("protocol desync".into());
        let shell_err: ShellHostError = ctrl_err.into();
        assert!(matches!(shell_err, ShellHostError::Control(_)));
        assert!(shell_err.source().is_some());
        assert!(shell_err.to_string().contains("protocol desync"));

        let panel_err = PanelError::Flush("poll failed".into());
        let shell_err: ShellHostError = panel_err.into();
        assert!(matches!(shell_err, ShellHostError::Panel(_)));
        assert!(shell_err.to_string().contains("poll failed"));
    }
}
