//! System tray icon: restore the widget (left-click) and quit the sensor
//! (right-click menu).
//!
//! The widget is `skipTaskbar` + decorationless, so the tray is the only
//! affordance for un-hiding it and the canonical Quit path.

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

/// Build the tray icon, menu, and handlers. Called once from `setup`.
pub fn init(app: &AppHandle) -> tauri::Result<()> {
    let quit_i = MenuItem::with_id(app, "tray_quit", "Quit Whence", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&quit_i])?;

    TrayIconBuilder::with_id("whence-tray")
        .icon(app.default_window_icon().expect("bundle icon missing").clone())
        .tooltip("Whence")
        .menu(&menu)
        // Right-click opens the menu; left-click is handled below as "restore".
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            if event.id.as_ref() == "tray_quit" {
                app.exit(0);
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                reveal(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

/// Un-minimize, show, and focus the main widget window.
fn reveal(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
    }
}
