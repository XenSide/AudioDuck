//! Writes the plugin's manifest icons: `cargo run --example icons`.
#![allow(dead_code)] // only ACCENT is used here
#[path = "../src/icon.rs"]
mod icon;

fn main() {
    let dir = std::path::Path::new("streamdeck/com.xenside.audioduck.sdPlugin/imgs");
    for (name, size) in [("plugin/icon", 256), ("actions/toggle/icon", 20), ("actions/toggle/key", 72)] {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path.with_extension("png"), icon::png(icon::ACCENT, size)).unwrap();
        std::fs::write(format!("{}@2x.png", path.display()), icon::png(icon::ACCENT, size * 2)).unwrap();
    }
}
