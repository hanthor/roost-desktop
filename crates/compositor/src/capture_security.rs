//! Capture admission is an authenticated desktop-service boundary, not a bus name.
use std::os::unix::fs::MetadataExt;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc,
};
use zbus::{fdo, message::Header, Connection};

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
        let sender = header.sender().ok_or_else(|| denied("missing sender"))?;
        let dbus = fdo::DBusProxy::new(conn).await?;
        let pid = dbus
            .get_connection_unix_process_id(sender.clone().into())
            .await?;
        if !installed_portal(pid) {
            return Err(denied("service connection requires the installed portal"));
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
        self.unlocked()?;
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
    let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) else {
        return false;
    };
    let allowed = [
        "/usr/libexec/xdg-desktop-portal-gnome",
        "/usr/lib/xdg-desktop-portal-gnome",
    ];
    allowed.iter().any(|path| {
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
}
