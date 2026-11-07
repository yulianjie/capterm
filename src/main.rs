#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod config;
mod file_text;
mod local;
mod platform;
mod ssh;
mod terminal;
mod workbench;

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(|cx| {
            gpui_kit::init(cx);
            terminal::init(cx);
            workbench::open(cx);
        });
}
