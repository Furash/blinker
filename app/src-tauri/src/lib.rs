mod config;
mod paths;
mod theme;

pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running BlinkerUI");
}
