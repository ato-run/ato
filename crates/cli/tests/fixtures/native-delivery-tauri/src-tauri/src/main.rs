use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    if let Some(marker_path) = env::var_os("SAMPLE_NATIVE_CAPSULE_MARKER") {
        let marker_path = PathBuf::from(marker_path);
        if let Some(parent) = marker_path.parent() {
            fs::create_dir_all(parent).expect("create marker parent");
        }
        fs::write(&marker_path, b"sample-native-capsule\n").expect("write marker");
        if env::var_os("SAMPLE_NATIVE_CAPSULE_EXIT_AFTER_MARK").is_some() {
            return;
        }
    }

    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running sample-native-capsule");
}
