//! Actual external loader provenance, observed while its image/frame objects
//! remain alive. A modern SVG renderer is not an in-process pixbuf module.
use std::{collections::BTreeSet, path::PathBuf};

pub fn observe(
    paths: &mut BTreeSet<PathBuf>,
    modern: bool,
) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error>> {
    if !modern {
        return Ok(Vec::new());
    }
    let mut processors = Vec::new();
    let mut pending = vec![(std::process::id(), 0usize)];
    let mut seen = BTreeSet::new();
    while let Some((pid, depth)) = pending.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if seen.len() > 64 || depth > 8 {
            return Err("backend process count bound".into());
        }
        for entry in std::fs::read_dir(format!("/proc/{pid}/task"))? {
            match std::fs::read_to_string(entry?.path().join("children")) {
                Ok(children) => {
                    for child in children.split_whitespace() {
                        pending.push((child.parse::<u32>()?, depth + 1));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        if pid != std::process::id() {
            let executable = std::fs::read_link(format!("/proc/{pid}/exe"))?;
            let process_stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
            let start = process_stat
                .rsplit_once(')')
                .ok_or("backend process identity")?
                .1
                .split_whitespace()
                .nth(19)
                .ok_or("backend process start time")?;
            processors.push(serde_json::json!({"pid":pid,"executable":executable.to_string_lossy(),"start_ticks":start}));
            paths.insert(executable);
            for line in std::fs::read_to_string(format!("/proc/{pid}/maps"))?.lines() {
                if let Some((_, path)) = line.split_once('/') {
                    let path = PathBuf::from(format!("/{path}"));
                    // Namespace-only font mappings also have their configured
                    // native source identities in the FontConfig inventory.
                    if path.is_file() {
                        paths.insert(path);
                    }
                }
            }
        }
    }
    if processors.is_empty() {
        return Err("actual modern external loader not observed".into());
    }
    // Actual Glycin2 config search: override, otherwise user data followed by
    // system data (primary config.rs). Hash every registered descriptor to
    // preserve selection precedence and detect same-path configuration edits.
    let data = std::env::var_os("GLYCIN_DATA_DIR")
        .map(|p| vec![PathBuf::from(p)])
        .unwrap_or_else(|| {
            let mut paths = vec![std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
                })];
            paths.extend(std::env::split_paths(
                &std::env::var_os("XDG_DATA_DIRS")
                    .unwrap_or_else(|| "/usr/local/share:/usr/share".into()),
            ));
            paths
        });
    for data in data {
        match std::fs::read_dir(data.join("glycin-loaders/2+/conf.d")) {
            Ok(entries) => {
                for entry in entries {
                    let path = entry?.path();
                    if path.extension().is_some_and(|p| p == "conf") {
                        paths.insert(path);
                    }
                    if paths.len() > 16384 {
                        return Err("backend config count bound".into());
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(processors)
}
