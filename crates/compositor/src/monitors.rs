//! GNOME's display configuration (`~/.config/monitors.xml`, #59).
//!
//! GNOME Settings writes each monitor arrangement the user has chosen:
//! per connector, a logical position and a scale. Roost reads the
//! arrangement matching the connectors that are lit, so a laptop set to
//! 150 percent in GNOME opens Roost at 150 percent too. Logical layout
//! mode (GNOME's default) is assumed: positions are logical pixels.

use std::collections::HashMap;
use std::path::PathBuf;

/// One monitor in one arrangement.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorConfig {
    /// Connector name as the kernel names it (`eDP-1`, `HDMI-A-1`).
    pub connector: String,
    /// Output scale (1, 1.25, 1.5, 2, …).
    pub scale: f64,
    /// Logical position.
    pub x: i32,
    pub y: i32,
    /// GNOME's primary monitor (top bar).
    pub primary: bool,
}

/// Every arrangement in a monitors.xml, each as its monitors.
pub fn parse(xml: &str) -> Vec<Vec<MonitorConfig>> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return Vec::new();
    };
    let child_text = |node: roxmltree::Node<'_, '_>, name: &str| -> Option<String> {
        node.children()
            .find(|c| c.has_tag_name(name))
            .and_then(|c| c.text())
            .map(|t| t.trim().to_owned())
    };
    doc.root_element()
        .children()
        .filter(|n| n.has_tag_name("configuration"))
        .map(|configuration| {
            configuration
                .children()
                .filter(|n| n.has_tag_name("logicalmonitor"))
                .flat_map(|logical| {
                    let scale = child_text(logical, "scale")
                        .and_then(|s| s.parse::<f64>().ok())
                        .filter(|s| s.is_finite() && *s > 0.0)
                        .unwrap_or(1.0);
                    let x = child_text(logical, "x")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    let y = child_text(logical, "y")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    let primary = child_text(logical, "primary").is_some_and(|p| p == "yes");
                    // A logical monitor may mirror several connectors.
                    logical
                        .children()
                        .filter(|n| n.has_tag_name("monitor"))
                        .filter_map(move |monitor| {
                            let spec =
                                monitor.children().find(|c| c.has_tag_name("monitorspec"))?;
                            Some(MonitorConfig {
                                connector: child_text(spec, "connector")?,
                                scale,
                                x,
                                y,
                                primary,
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .collect()
        })
        .collect()
}

/// The arrangement for exactly these connectors, as GNOME picks it; else
/// the first one that covers them all. Keyed by connector.
pub fn choose(
    configurations: &[Vec<MonitorConfig>],
    connected: &[String],
) -> HashMap<String, MonitorConfig> {
    let mut wanted: Vec<&str> = connected.iter().map(String::as_str).collect();
    wanted.sort_unstable();
    fn names(c: &[MonitorConfig]) -> Vec<&str> {
        let mut n: Vec<&str> = c.iter().map(|m| m.connector.as_str()).collect();
        n.sort_unstable();
        n.dedup();
        n
    }
    let exact = configurations.iter().find(|c| names(c) == wanted);
    let covering = || {
        configurations
            .iter()
            .find(|c| wanted.iter().all(|w| names(c).contains(w)))
    };
    exact
        .or_else(covering)
        .map(|c| c.iter().map(|m| (m.connector.clone(), m.clone())).collect())
        .unwrap_or_default()
}

/// `$XDG_CONFIG_HOME/monitors.xml` (default `~/.config/monitors.xml`).
pub fn path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|dir| dir.join("monitors.xml"))
}

/// The user's arrangement for these connectors (empty when there is none).
pub fn load(connected: &[String]) -> HashMap<String, MonitorConfig> {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|xml| choose(&parse(&xml), connected))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<monitors version="2">
  <configuration>
    <logicalmonitor>
      <x>0</x><y>0</y><scale>1.5</scale><primary>yes</primary>
      <monitor>
        <monitorspec><connector>eDP-1</connector><vendor>BOE</vendor><product>0x095f</product><serial>0x00000000</serial></monitorspec>
        <mode><width>2256</width><height>1504</height><rate>59.999</rate></mode>
      </monitor>
    </logicalmonitor>
  </configuration>
  <configuration>
    <logicalmonitor>
      <x>0</x><y>0</y><scale>1.5</scale><primary>yes</primary>
      <monitor><monitorspec><connector>eDP-1</connector></monitorspec></monitor>
    </logicalmonitor>
    <logicalmonitor>
      <x>1504</x><y>0</y><scale>1</scale>
      <monitor><monitorspec><connector>HDMI-A-1</connector></monitorspec></monitor>
    </logicalmonitor>
  </configuration>
</monitors>"#;

    #[test]
    fn parses_gnome_monitors_xml() {
        let configs = parse(XML);
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].len(), 1);
        assert_eq!(configs[0][0].connector, "eDP-1");
        assert_eq!(configs[0][0].scale, 1.5);
        assert!(configs[0][0].primary);
        assert_eq!(configs[1][1].connector, "HDMI-A-1");
        assert_eq!((configs[1][1].x, configs[1][1].scale), (1504, 1.0));
        assert!(parse("<not xml").is_empty());
    }

    #[test]
    fn picks_the_arrangement_for_the_lit_connectors() {
        let configs = parse(XML);
        let laptop = choose(&configs, &["eDP-1".to_owned()]);
        assert_eq!(laptop.len(), 1);
        assert_eq!(laptop["eDP-1"].scale, 1.5);
        let docked = choose(&configs, &["HDMI-A-1".to_owned(), "eDP-1".to_owned()]);
        assert_eq!(docked["HDMI-A-1"].x, 1504);
        assert!(choose(&configs, &["DP-3".to_owned()]).is_empty());
    }
}
