use tray_icon::{TrayIconBuilder, menu::Menu,Icon};

fn main() {
    let icon = Icon::from_rgba(vec![255; 64 * 64 * 4], 64, 64).expect("Failed to create icon");


    let tray_menu = Menu::new();
    let tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_tooltip("system-tray - tray icon library!")
        .with_icon(icon)
        .build()
        .unwrap();

    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
