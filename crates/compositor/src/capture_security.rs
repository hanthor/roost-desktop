//! Capture admission is an authenticated desktop-service boundary, not a bus name.
use std::os::unix::fs::MetadataExt;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc,
};
use zbus::{fdo, message::Header, Connection};

/// GNOME's typed display bootstrap roles. These never confer capture authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServiceClient {
    Portal,
    FileChooser,
    GlobalShortcuts,
}
impl ServiceClient {
    pub(crate) fn from_wire(value: u32) -> fdo::Result<Self> {
        match value {
            1 => Ok(Self::Portal),
            2 => Ok(Self::FileChooser),
            3 => Ok(Self::GlobalShortcuts),
            _ => Err(fdo::Error::InvalidArgs(
                "unsupported service client type".into(),
            )),
        }
    }
    fn executables(self) -> &'static [&'static str] {
        match self {
            Self::Portal => &[
                "/usr/libexec/xdg-desktop-portal-gnome",
                "/usr/lib/xdg-desktop-portal-gnome",
            ],
            Self::FileChooser => &["/usr/bin/nautilus"],
            Self::GlobalShortcuts => &[
                "/usr/libexec/gnome-control-center-global-shortcuts-provider",
                "/usr/lib/gnome-control-center-global-shortcuts-provider",
            ],
        }
    }
}

#[derive(Clone, Default)]
pub struct Authority {
    locked: Arc<AtomicBool>,
    shell_pid: Arc<AtomicU32>,
}
impl Authority {
    /// The portal opens its native display before acquiring its service name.
    /// This authenticates that bootstrap executable, without granting capture.
    pub async fn admit_portal_connection(
        &self,
        conn: &Connection,
        header: &Header<'_>,
    ) -> fdo::Result<String> {
        self.admit_service_connection(conn, header, ServiceClient::Portal)
            .await
    }
    /// Resolve bootstrap identity from the bus credentials and installed binary,
    /// independently of provider names acquired after GTK display initialization.
    pub(crate) async fn admit_service_connection(
        &self,
        conn: &Connection,
        header: &Header<'_>,
        role: ServiceClient,
    ) -> fdo::Result<String> {
        let sender = header.sender().ok_or_else(|| denied("missing sender"))?;
        let dbus = fdo::DBusProxy::new(conn).await?;
        let uid = dbus.get_connection_unix_user(sender.clone().into()).await?;
        let pid = dbus
            .get_connection_unix_process_id(sender.clone().into())
            .await?;
        if uid != rustix::process::geteuid().as_raw() || !installed_service(pid, role) {
            return Err(denied(
                "service connection requires the installed provider for its type",
            ));
        }
        Ok(sender.to_string())
    }
    pub fn publish(&self, locked: bool, shell_pid: Option<u32>) {
        self.locked.store(locked, Ordering::SeqCst);
        self.shell_pid
            .store(shell_pid.unwrap_or(0), Ordering::SeqCst);
    }
    pub fn unlocked(&self) -> fdo::Result<()> {
        if self.locked.load(Ordering::SeqCst) {
            return Err(denied("session locked"));
        }
        Ok(())
    }
    pub async fn owner_alive(&self, conn: &Connection, owner: &str) -> bool {
        if self.unlocked().is_err() {
            return false;
        }
        let Ok(name) = zbus::names::BusName::try_from(owner) else {
            return false;
        };
        let Ok(dbus) = fdo::DBusProxy::new(conn).await else {
            return false;
        };
        let Ok(pid) = dbus.get_connection_unix_process_id(name).await else {
            return false;
        };
        if pid != 0 && pid == self.shell_pid.load(Ordering::SeqCst) {
            return true;
        }
        let name =
            zbus::names::BusName::try_from("org.freedesktop.impl.portal.desktop.gnome").unwrap();
        dbus.get_name_owner(name)
            .await
            .is_ok_and(|current| current.as_str() == owner)
            && installed_portal(pid)
    }
    pub async fn admit(&self, conn: &Connection, header: &Header<'_>) -> fdo::Result<String> {
        self.unlocked()?;
        let sender = self.authenticate(conn, header).await?;
        self.unlocked()?;
        Ok(sender)
    }
    /// Authenticate desktop-service identity without authorizing capture.
    /// Screenshot's documented false result can deny an authenticated locked
    /// backend while other capture APIs retain their AccessDenied result.
    pub async fn authenticate(
        &self,
        conn: &Connection,
        header: &Header<'_>,
    ) -> fdo::Result<String> {
        let sender = header.sender().ok_or_else(|| denied("missing sender"))?;
        let dbus = fdo::DBusProxy::new(conn).await?;
        let pid = dbus
            .get_connection_unix_process_id(sender.clone().into())
            .await?;
        if pid != 0 && pid == self.shell_pid.load(Ordering::SeqCst) {
            return Ok(sender.to_string());
        }
        let name =
            zbus::names::BusName::try_from("org.freedesktop.impl.portal.desktop.gnome").unwrap();
        let owner = dbus
            .get_name_owner(name)
            .await
            .map_err(|_| denied("portal not running"))?;
        if owner != *sender || !installed_portal(pid) {
            return Err(denied(
                "capture requires the supervised shell or installed portal",
            ));
        }
        Ok(sender.to_string())
    }
}
pub fn owns_session(owner: &str, sender: Option<&str>, revoked: bool) -> bool {
    !revoked && sender.is_some_and(|sender| sender == owner)
}
pub fn denied(reason: &str) -> fdo::Error {
    fdo::Error::AccessDenied(reason.into())
}
fn installed_portal(pid: u32) -> bool {
    installed_service(pid, ServiceClient::Portal)
}
fn installed_service(pid: u32, role: ServiceClient) -> bool {
    let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) else {
        return false;
    };
    role.executables().iter().any(|path| {
        let Ok(real) = std::fs::canonicalize(path) else {
            return false;
        };
        let Ok(meta) = real.metadata() else {
            return false;
        };
        real == exe && meta.uid() == 0 && meta.mode() & 0o022 == 0
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lock_admission_fails_closed_and_updates() {
        let authority = Authority::default();
        authority.publish(true, Some(42));
        assert!(matches!(
            authority.unlocked(),
            Err(fdo::Error::AccessDenied(_))
        ));
        authority.publish(false, None);
        assert!(authority.unlocked().is_ok());
        assert_eq!(authority.shell_pid.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn sessions_are_owned_by_the_unique_creator_and_revoke_permanently() {
        assert!(owns_session(":1.1", Some(":1.1"), false));
        assert!(!owns_session(":1.1", Some(":1.2"), false));
        assert!(!owns_session(":1.1", None, false));
        assert!(!owns_session(":1.1", Some(":1.1"), true));
    }
    #[test]
    fn arbitrary_process_cannot_be_the_installed_portal() {
        assert!(!installed_portal(std::process::id()));
        assert!(!installed_portal(u32::MAX));
    }
    #[test]
    fn typed_service_roles_do_not_accept_an_arbitrary_process() {
        for value in 1..=3 {
            let role = ServiceClient::from_wire(value).unwrap();
            assert!(!installed_service(std::process::id(), role));
            assert!(!installed_service(u32::MAX, role));
        }
        for value in [0, 4, u32::MAX] {
            assert!(matches!(
                ServiceClient::from_wire(value),
                Err(fdo::Error::InvalidArgs(_))
            ));
        }
        assert_eq!(
            ServiceClient::from_wire(2).unwrap(),
            ServiceClient::FileChooser
        );
        assert_eq!(
            ServiceClient::from_wire(3).unwrap(),
            ServiceClient::GlobalShortcuts
        );
        for left in [
            ServiceClient::Portal,
            ServiceClient::FileChooser,
            ServiceClient::GlobalShortcuts,
        ] {
            for right in [
                ServiceClient::Portal,
                ServiceClient::FileChooser,
                ServiceClient::GlobalShortcuts,
            ] {
                if left != right {
                    assert!(left
                        .executables()
                        .iter()
                        .all(|path| !right.executables().contains(path)));
                }
            }
        }
    }
}
