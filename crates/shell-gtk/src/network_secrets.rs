//! NetworkManager secret fields, VPN external-UI protocol and libsecret storage.
//! Behaviors follow GNOME Shell 51's networkAgent.js and ShellNetworkAgent.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::Path;

use crate::network_agent::WifiSecret;
use gio::prelude::*;

#[derive(Clone, Debug)]
pub struct Field {
    pub key: Option<String>,
    pub label: String,
    pub value: String,
    pub password: bool,
    pub validator: Option<WifiSecret>,
}
impl Field {
    pub fn valid(&self, value: &str) -> bool {
        if self.key.as_deref() == Some("pin") {
            return (4..=8).contains(&value.len()) && value.bytes().all(|c| c.is_ascii_digit());
        }
        self.key.is_none()
            || self
                .validator
                .as_ref()
                .map_or(!value.is_empty(), |v| v.valid(value))
    }
}
pub fn section(connection: &glib::Variant, name: &str) -> Option<glib::VariantDict> {
    connection.iter().find_map(|e| {
        (e.child_value(0).str() == Some(name))
            .then(|| glib::VariantDict::new(Some(&e.child_value(1))))
    })
}
pub fn text(dict: Option<&glib::VariantDict>, key: &str) -> String {
    dict.and_then(|s| s.lookup_value(key, None))
        .and_then(|v| v.get::<String>())
        .unwrap_or_default()
}
pub fn number(dict: Option<&glib::VariantDict>, key: &str) -> u32 {
    dict.and_then(|s| s.lookup_value(key, None))
        .and_then(|v| v.get::<u32>())
        .unwrap_or_default()
}
fn field(
    dict: Option<&glib::VariantDict>,
    key: &str,
    label: &str,
    password: bool,
    editable: bool,
) -> Field {
    Field {
        key: editable.then(|| key.to_owned()),
        label: label.to_owned(),
        value: text(dict, key),
        password,
        validator: None,
    }
}
pub fn fields(connection: &glib::Variant, setting: &str, hints: &[String]) -> Option<Vec<Field>> {
    let dict = section(connection, setting);
    let dict = dict.as_ref();
    match setting {
        "802-11-wireless-security" => {
            let secret = WifiSecret::for_key_mgmt(
                &text(dict, "key-mgmt"),
                number(dict, "wep-tx-keyidx"),
                number(dict, "wep-key-type"),
            )?;
            Some(vec![Field {
                key: Some(secret.key()),
                label: secret.label().to_owned(),
                value: text(dict, &secret.key()),
                password: true,
                validator: Some(secret),
            }])
        }
        "802-1x" => {
            let choices = [
                ("identity", "Username", false),
                ("password", "Password", true),
                ("private-key-password", "Private key password", true),
                (
                    "phase2-private-key-password",
                    "Phase 2 private key password",
                    true,
                ),
            ];
            if !hints.is_empty() {
                let requested: Vec<_> = choices
                    .iter()
                    .filter(|(key, _, _)| hints.iter().any(|h| h == key))
                    .map(|(key, label, password)| field(dict, key, label, *password, true))
                    .collect();
                return (!requested.is_empty()).then_some(requested);
            }
            let eap = dict
                .and_then(|s| s.lookup_value("eap", None))
                .and_then(|v| v.get::<Vec<String>>())
                .unwrap_or_default();
            let key = match eap.first().map(String::as_str) {
                Some("tls") => "private-key-password",
                Some("md5" | "leap" | "ttls" | "peap" | "fast") => "password",
                _ => return None,
            };
            Some(vec![
                field(
                    dict,
                    "identity",
                    if key == "password" {
                        "Username"
                    } else {
                        "Identity"
                    },
                    false,
                    false,
                ),
                field(
                    dict,
                    key,
                    if key == "password" {
                        "Password"
                    } else {
                        "Private key password"
                    },
                    true,
                    true,
                ),
            ])
        }
        "gsm" => Some(vec![field(
            dict,
            if hints.iter().any(|h| h == "pin") {
                "pin"
            } else {
                "password"
            },
            if hints.iter().any(|h| h == "pin") {
                "PIN"
            } else {
                "Password"
            },
            true,
            true,
        )]),
        _ => None,
    }
}

/// Secret values travel through stdin/stdout, never process arguments or logs.
async fn tool(args: &[String], input: Option<String>, missing_ok: bool) -> Result<String, String> {
    let argv: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    let process = gio::Subprocess::newv(
        &argv,
        gio::SubprocessFlags::STDIN_PIPE
            | gio::SubprocessFlags::STDOUT_PIPE
            | gio::SubprocessFlags::STDERR_PIPE,
    )
    .map_err(|_| "secret helper is unavailable".to_owned())?;
    let timed = process.clone();
    let timer = glib::timeout_add_local_once(std::time::Duration::from_secs(30), move || {
        timed.force_exit()
    });
    let result = process.communicate_utf8_future(input).await;
    // A fired one-shot source is already gone; only remove a still-live source.
    if glib::MainContext::default()
        .find_source_by_id(&timer)
        .is_some()
    {
        timer.remove();
    }
    let (out, err) = result.map_err(|_| "secret helper failed".to_owned())?;
    // secret-tool clear exits1 with no stderr when no item matches.
    let absent = missing_ok
        && process.has_exited()
        && process.exit_status() == 1
        && err.as_deref().unwrap_or_default().trim().is_empty();
    if !process.is_successful() && !absent {
        return Err("secret helper failed".to_owned());
    }
    Ok(out.map(|s| s.to_string()).unwrap_or_default())
}
fn attributes(uuid: &str, setting: &str, key: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "connection-uuid".into(),
        uuid.into(),
        "setting-name".into(),
        setting.into(),
    ];
    if let Some(key) = key {
        args.extend(["setting-key".into(), key.into()]);
    }
    args
}
pub async fn lookup(uuid: &str, setting: &str, key: &str) -> Option<String> {
    if uuid.is_empty() {
        return None;
    }
    let mut args = vec!["secret-tool".into(), "lookup".into()];
    args.extend(attributes(uuid, setting, Some(key)));
    tool(&args, None)
        .await
        .ok()
        .map(|s| s.trim_end_matches('\n').to_owned())
        .filter(|s| !s.is_empty())
}
pub async fn store(
    uuid: &str,
    id: &str,
    setting: &str,
    key: &str,
    value: &str,
) -> Result<(), String> {
    if uuid.is_empty() {
        return Err("connection has no UUID".into());
    }
    let mut args = vec![
        "secret-tool".into(),
        "store".into(),
        format!("--label=Network secret for {id}/{setting}/{key}"),
    ];
    args.extend(attributes(uuid, setting, Some(key)));
    tool(&args, Some(value.to_owned()), false).await.map(|_| ())
}
pub async fn delete(uuid: &str, setting: &str) -> Result<(), String> {
    if uuid.is_empty() {
        return Err("connection has no UUID".into());
    }
    let mut args = vec!["secret-tool".into(), "clear".into()];
    args.extend(attributes(uuid, setting, None));
    tool(&args, None, true).await.map(|_| ())
}
/// Only AGENT_OWNED (1), never system-owned, NOT_SAVED or NOT_REQUIRED.
pub fn agent_owned(connection: &glib::Variant, setting: &str, key: &str) -> bool {
    let Some(dict) = section(connection, setting) else {
        return false;
    };
    if setting == "vpn" {
        let data = dict
            .lookup_value("data", None)
            .and_then(|v| v.get::<BTreeMap<String, String>>())
            .unwrap_or_default();
        data.get(&format!("{key}-flags")).is_some_and(|v| v == "1")
    } else {
        number(Some(&dict), &format!("{key}-flags")) == 1
    }
}

pub struct VpnPrompt {
    pub title: String,
    pub message: String,
    pub fields: Vec<Field>,
    pub values: BTreeMap<String, String>,
}
fn plugin(service: &str) -> Option<(String, bool)> {
    let mut dirs = vec![
        "/usr/lib/NetworkManager/VPN".to_owned(),
        "/usr/lib64/NetworkManager/VPN".to_owned(),
        "/usr/lib/x86_64-linux-gnu/NetworkManager/VPN".to_owned(),
        "/etc/NetworkManager/VPN".to_owned(),
    ];
    if std::env::var_os("ROOST_SHELL_DBUS_UNRESTRICTED").is_some() {
        if let Ok(dir) = std::env::var("ROOST_NM_VPN_PLUGIN_DIR") {
            dirs.insert(0, dir);
        }
    }
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.path().extension().and_then(|s| s.to_str()) != Some("name") {
                continue;
            }
            let key = glib::KeyFile::new();
            if key
                .load_from_file(entry.path(), glib::KeyFileFlags::NONE)
                .is_err()
            {
                continue;
            }
            if key.string("VPN Connection", "service").ok().as_deref() != Some(service) {
                continue;
            }
            if !key
                .boolean("GNOME", "supports-external-ui-mode")
                .unwrap_or(false)
            {
                continue;
            }
            let Ok(helper) = key.string("GNOME", "auth-dialog") else {
                continue;
            };
            if Path::new(helper.as_str()).is_absolute() {
                return Some((
                    helper.to_string(),
                    key.boolean("GNOME", "supports-hints").unwrap_or(false),
                ));
            }
        }
    }
    None
}
pub fn parse_vpn(output: &str) -> Result<VpnPrompt, String> {
    let key = glib::KeyFile::new();
    key.load_from_data(output, glib::KeyFileFlags::NONE)
        .map_err(|_| "invalid VPN prompt".to_owned())?;
    if key.integer("VPN Plugin UI", "Version").ok() != Some(2) {
        return Err("unsupported VPN prompt version".into());
    }
    let mut prompt = VpnPrompt {
        title: key
            .string("VPN Plugin UI", "Title")
            .map_err(|_| "missing VPN title")?
            .to_string(),
        message: key
            .string("VPN Plugin UI", "Description")
            .map_err(|_| "missing VPN description")?
            .to_string(),
        fields: vec![],
        values: BTreeMap::new(),
    };
    for group in key.groups().iter() {
        if group.as_str() == "VPN Plugin UI" {
            continue;
        }
        let value = key
            .string(group, "Value")
            .map_err(|_| "missing VPN field value")?
            .to_string();
        if key
            .boolean(group, "ShouldAsk")
            .map_err(|_| "missing VPN field policy")?
        {
            prompt.fields.push(Field {
                key: Some(group.to_string()),
                label: key
                    .string(group, "Label")
                    .map_err(|_| "missing VPN field label")?
                    .to_string(),
                value,
                password: key
                    .boolean(group, "IsSecret")
                    .map_err(|_| "missing VPN secret policy")?,
                validator: None,
            });
        } else if !value.is_empty() {
            prompt.values.insert(group.to_string(), value);
        }
    }
    Ok(prompt)
}
pub async fn vpn(
    connection: &glib::Variant,
    hints: &[String],
    flags: u32,
) -> Result<VpnPrompt, String> {
    let dict = section(connection, "vpn").ok_or("missing VPN setting")?;
    let service = text(Some(&dict), "service-type");
    let (helper, supports_hints) =
        plugin(&service).ok_or("VPN plugin has no external-UI authentication helper")?;
    let conn = section(connection, "connection");
    let mut args = vec![
        helper,
        "-u".into(),
        text(conn.as_ref(), "uuid"),
        "-n".into(),
        text(conn.as_ref(), "id"),
        "-s".into(),
        service,
        "--external-ui-mode".into(),
    ];
    if flags & 1 != 0 {
        args.push("-i".into());
    }
    if flags & 2 != 0 {
        args.push("-r".into());
    }
    if supports_hints {
        for hint in hints {
            args.extend(["-t".into(), hint.clone()]);
        }
    }
    let mut input = String::new();
    for (section, prefix) in [("data", "DATA"), ("secrets", "SECRET")] {
        let items = dict
            .lookup_value(section, None)
            .and_then(|v| v.get::<BTreeMap<String, String>>())
            .unwrap_or_default();
        for (key, value) in items {
            if key.contains(['\n', '\r']) || value.contains(['\n', '\r']) {
                return Err("VPN helper cannot encode multiline values".into());
            }
            input.push_str(&format!("{prefix}_KEY={key}\n{prefix}_VAL={value}\n\n"));
        }
    }
    input.push_str("DONE\n\n");
    let output = tool(&args, Some(input), false).await?;
    if output.is_empty() {
        return Ok(VpnPrompt {
            title: String::new(),
            message: String::new(),
            fields: vec![],
            values: BTreeMap::new(),
        });
    }
    parse_vpn(&output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enterprise_fields_respect_hints_and_secret_ownership() {
        use std::collections::HashMap;
        let connection = HashMap::from([(
            "802-1x".to_owned(),
            HashMap::from([
                ("identity".to_owned(), "alice".to_variant()),
                ("eap".to_owned(), vec!["peap".to_owned()].to_variant()),
                ("password-flags".to_owned(), 1u32.to_variant()),
            ]),
        )])
        .to_variant();
        let standard = fields(&connection, "802-1x", &[]).unwrap();
        assert_eq!(standard[0].key, None);
        assert_eq!(standard[0].value, "alice");
        assert_eq!(standard[1].key.as_deref(), Some("password"));
        let hinted = fields(
            &connection,
            "802-1x",
            &["identity".into(), "private-key-password".into()],
        )
        .unwrap();
        assert_eq!(hinted[0].key.as_deref(), Some("identity"));
        assert_eq!(hinted[1].key.as_deref(), Some("private-key-password"));
        assert!(agent_owned(&connection, "802-1x", "password"));
        assert!(!agent_owned(&connection, "802-1x", "identity"));
        let pin = Field {
            key: Some("pin".into()),
            label: "PIN".into(),
            value: String::new(),
            password: true,
            validator: None,
        };
        assert!(pin.valid("1234"));
        assert!(!pin.valid("123"));
        assert!(!pin.valid("abcd"));
    }

    #[test]
    fn vpn_prompt_keeps_supplied_values_and_requests_typed_fields() {
        let prompt = parse_vpn("[VPN Plugin UI]\nVersion=2\nTitle=VPN authentication\nDescription=Connect to office\n[password]\nValue=\nShouldAsk=true\nLabel=Password\nIsSecret=true\n[username]\nValue=alice\nShouldAsk=false\n").unwrap();
        assert_eq!(prompt.fields.len(), 1);
        assert_eq!(prompt.fields[0].key.as_deref(), Some("password"));
        assert!(prompt.fields[0].password);
        assert_eq!(
            prompt.values.get("username").map(String::as_str),
            Some("alice")
        );
        assert!(parse_vpn("[VPN Plugin UI]\nVersion=1\n").is_err());
    }
}
