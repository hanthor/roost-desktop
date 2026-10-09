//! Bundle GNOME Shell's own icons (icons/README.md) as a GResource, as
//! GNOME Shell does, so the shell finds them whatever the system theme.

use std::fmt::Write as _;
use std::path::Path;

fn main() {
    let root = Path::new("icons");
    let mut files = Vec::new();
    for context in ["actions", "status"] {
        let dir = root.join("scalable").join(context);
        println!("cargo:rerun-if-changed={}", dir.display());
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .expect("icons dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".svg"))
            .collect();
        names.sort();
        files.extend(names.into_iter().map(|n| format!("scalable/{context}/{n}")));
    }
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gresources>\n  <gresource prefix=\"/org/tuna/Shell/icons\">\n",
    );
    for file in &files {
        let _ = writeln!(
            xml,
            "    <file preprocess=\"xml-stripblanks\">{file}</file>"
        );
    }
    xml.push_str("  </gresource>\n</gresources>\n");
    let out = std::env::var("OUT_DIR").expect("OUT_DIR");
    let manifest = Path::new(&out).join("icons.gresource.xml");
    std::fs::write(&manifest, xml).expect("write manifest");
    glib_build_tools::compile_resources(
        &[root],
        manifest.to_str().expect("utf-8"),
        "icons.gresource",
    );
}
