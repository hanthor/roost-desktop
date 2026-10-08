//! Typed metadata for the private wallpaper drop, not control-protocol messages.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Placement {
    None,
    Wallpaper,
    Centered,
    Scaled,
    Stretched,
    #[default]
    Zoom,
    Spanned,
}
impl Placement {
    pub fn from_key(value: &str) -> Option<Self> {
        Some(match value {
            "none" => Self::None,
            "wallpaper" => Self::Wallpaper,
            "centered" => Self::Centered,
            "scaled" => Self::Scaled,
            "stretched" => Self::Stretched,
            "zoom" => Self::Zoom,
            "spanned" => Self::Spanned,
            _ => return None,
        })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Shading {
    #[default]
    Solid,
    Horizontal,
    Vertical,
}
impl Shading {
    pub fn from_key(value: &str) -> Option<Self> {
        Some(match value {
            "solid" => Self::Solid,
            "horizontal" => Self::Horizontal,
            "vertical" => Self::Vertical,
            _ => return None,
        })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PictureSettings {
    pub placement: Placement,
    pub shading: Shading,
    pub primary: [u8; 3],
    pub secondary: [u8; 3],
}
impl Default for PictureSettings {
    fn default() -> Self {
        Self {
            placement: Placement::Zoom,
            shading: Shading::Solid,
            primary: [2, 60, 136],
            secondary: [87, 137, 202],
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackgroundMetadata {
    pub version: u32,
    pub desktop: PictureSettings,
    pub lock: PictureSettings,
}
