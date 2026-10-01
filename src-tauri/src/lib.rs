//! OneCamp for the desktop: the workspace's own web app in a native window.
//!
//! The app holds no copy of OneCamp. It loads the address of the workspace the
//! person uses, so it is always the version their server runs, and adds what a
//! browser tab cannot: its own window and dock icon, a tray icon that keeps it
//! running for notifications, native notifications, and updates of the shell.
//!
//! Security model:
//! - The setup page (bundled) may call `open_workspace`; nothing remote may.
//! - The workspace's origin may call the notification plugin, granted at run
//!   time once its address is known, and nothing else.
//! - Any window a page tries to open goes to the system browser, so links in
//!   messages never turn into app windows with app privileges.

use std::sync::Mutex;

use tauri::{
    ipc::CapabilityBuilder,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    webview::NewWindowResponse,
    AppHandle, Manager, RunEvent, Url, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_store::StoreExt;
use tauri_plugin_updater::UpdaterExt;

const STORE: &str = "settings.json";
const WORKSPACE_KEY: &str = "workspace_url";
const MAIN: &str = "main";

/// Set while the app is quitting, so closing the window then really quits
/// instead of hiding to the tray.
struct Quitting(Mutex<bool>);

/// Turns what a person typed into the workspace's origin, or says what is wrong.
/// https only, except a local address for development.
pub fn normalize_workspace(input: &str) -> Result<Url, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("Enter your workspace address.".into());
    }
    let with_scheme = if s.contains("://") { s.to_string() } else { format!("https://{s}") };
    let url = Url::parse(&with_scheme).map_err(|_| "That is not a web address.".to_string())?;
    let host = url.host_str().unwrap_or("").to_string();
    if host.is_empty() {
        return Err("That is not a web address.".into());
    }
    let local = host == "localhost" || host == "127.0.0.1";
    match url.scheme() {
        "https" => {}
        "http" if local => {}
        _ => return Err("Use the https:// address of your workspace.".into()),
    }
    let mut origin = url.clone();
    origin.set_path("/");
    origin.set_query(None);
    origin.set_fragment(None);
    let _ = origin.set_username("");
    let _ = origin.set_password(None);
    Ok(origin)
}

fn saved_workspace(app: &AppHandle) -> Option<Url> {
    let store = app.store(STORE).ok()?;
    let v = store.get(WORKSPACE_KEY)?;
    normalize_workspace(v.as_str()?).ok()
}

/// Lets the workspace's own pages use native notifications. Nothing else.
fn grant_workspace(app: &AppHandle, origin: &Url) {
    let pattern = format!("{}*", origin.as_str());
    let cap = CapabilityBuilder::new("workspace")
        .remote(pattern)
        .local(false)
        .window(MAIN)
        .permission("notification:default");
    if let Err(e) = app.add_capability(cap) {
        eprintln!("could not grant notifications to the workspace: {e}");
    }
}

/// Called by the bundled setup page with what the person typed.
#[tauri::command]
fn open_workspace(app: AppHandle, url: String) -> Result<(), String> {
    let origin = normalize_workspace(&url)?;
    let store = app.store(STORE).map_err(|e| e.to_string())?;
    store.set(WORKSPACE_KEY, serde_json::json!(origin.as_str()));
    store.save().map_err(|e| e.to_string())?;
    grant_workspace(&app, &origin);
    let window = app.get_webview_window(MAIN).ok_or("The window is gone. Restart OneCamp.")?;
    window.navigate(origin).map_err(|e| e.to_string())
}

/// Forgets the workspace and starts again at the setup page.
fn change_workspace(app: &AppHandle) {
    if let Ok(store) = app.store(STORE) {
        store.delete(WORKSPACE_KEY);
        let _ = store.save();
    }
    app.restart();
}

/// Brings the window forward. If a link without a new-tab target led the
/// window away from the workspace, it also goes back, because the app has no
/// back button of its own.
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(MAIN) {
        if let (Some(home), Ok(current)) = (saved_workspace(app), w.url()) {
            let away = matches!(current.scheme(), "http" | "https") && current.origin() != home.origin();
            if away {
                let _ = w.navigate(home);
            }
        }
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Checks for a newer desktop app. `interactive` is a person asking from the
/// tray, who hears the answer either way; the check at start-up only speaks up
/// when there is something to install.
fn check_for_updates(app: AppHandle, interactive: bool) {
    tauri::async_runtime::spawn(async move {
        let notify = |title: &str, body: String| {
            let _ = app.notification().builder().title(title).body(body).show();
        };
        let updater = match app.updater() {
            Ok(u) => u,
            Err(e) => {
                if interactive {
                    notify("Couldn't check for updates", e.to_string());
                }
                return;
            }
        };
        match updater.check().await {
            Ok(Some(update)) => {
                if !interactive {
                    notify(
                        "An update is ready",
                        format!("OneCamp {} is available. Choose \"Check for updates\" in the tray icon's menu to install it.", update.version),
                    );
                    return;
                }
                notify("Updating OneCamp", format!("Installing {}. OneCamp restarts when it is done.", update.version));
                match update.download_and_install(|_, _| {}, || {}).await {
                    Ok(()) => app.restart(),
                    Err(e) => notify("The update did not install", e.to_string()),
                }
            }
            Ok(None) => {
                if interactive {
                    notify("OneCamp is up to date", format!("You have the newest version, {}.", app.package_info().version));
                }
            }
            Err(e) => {
                if interactive {
                    notify("Couldn't check for updates", e.to_string());
                }
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        // First, so a second launch focuses the running app instead of
        // starting another one.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| show_main(app)))
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(Quitting(Mutex::new(false)))
        .invoke_handler(tauri::generate_handler![open_workspace])
        .setup(|app| {
            let handle = app.handle().clone();
            let saved = saved_workspace(&handle);
            if let Some(origin) = &saved {
                grant_workspace(&handle, origin);
            }
            let start = match &saved {
                Some(origin) => WebviewUrl::External(origin.clone()),
                None => WebviewUrl::App("index.html".into()),
            };

            let opener = handle.clone();
            WebviewWindowBuilder::new(app, MAIN, start)
                .title("OneCamp")
                .inner_size(1280.0, 820.0)
                .min_inner_size(880.0, 560.0)
                // Links that open a new window (a link in a message, a file
                // preview) go to the system browser.
                .on_new_window(move |url, _features| {
                    if matches!(url.scheme(), "http" | "https" | "mailto") {
                        let _ = opener.opener().open_url(url.as_str(), None::<&str>);
                    }
                    NewWindowResponse::Deny
                })
                // Top-level navigation stays in the app: signing in goes
                // through Google, an SSO provider or GitHub and comes back.
                // Only web addresses and the bundled page are loaded.
                .on_navigation(|url| matches!(url.scheme(), "http" | "https" | "tauri" | "asset" | "about"))
                .build()?;

            let open = MenuItem::with_id(app, "open", "Open OneCamp", true, None::<&str>)?;
            let change = MenuItem::with_id(app, "change", "Change workspace…", true, None::<&str>)?;
            let update = MenuItem::with_id(app, "update", "Check for updates", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit OneCamp", true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[&open, &PredefinedMenuItem::separator(app)?, &change, &update, &PredefinedMenuItem::separator(app)?, &quit],
            )?;
            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().cloned().expect("bundled icon"))
                .tooltip("OneCamp")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main(app),
                    "change" => change_workspace(app),
                    "update" => check_for_updates(app.clone(), true),
                    "quit" => {
                        *app.state::<Quitting>().0.lock().unwrap() = true;
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            check_for_updates(handle, false);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building OneCamp");

    app.run(|app, event| match event {
        // Closing the window keeps OneCamp in the tray, like a chat app, so
        // messages still arrive. Quit from the tray menu ends it.
        RunEvent::WindowEvent { label, event: WindowEvent::CloseRequested { api, .. }, .. } if label == MAIN => {
            if !*app.state::<Quitting>().0.lock().unwrap() {
                api.prevent_close();
                if let Some(w) = app.get_webview_window(MAIN) {
                    let _ = w.hide();
                }
            }
        }
        // macOS: clicking the dock icon brings the window back.
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => show_main(app),
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::normalize_workspace;

    #[test]
    fn accepts_what_people_type() {
        for (input, want) in [
            ("onecamp.acme.com", "https://onecamp.acme.com/"),
            ("https://onecamp.acme.com/app/chat?x=1#y", "https://onecamp.acme.com/"),
            ("  https://onecamp.acme.com  ", "https://onecamp.acme.com/"),
            ("http://localhost:3000", "http://localhost:3000/"),
            ("https://user:pw@onecamp.acme.com", "https://onecamp.acme.com/"),
        ] {
            assert_eq!(normalize_workspace(input).unwrap().as_str(), want, "{input}");
        }
    }

    #[test]
    fn refuses_what_it_should() {
        for input in ["", "http://onecamp.acme.com", "ftp://x.com", "javascript:alert(1)", "file:///etc/passwd"] {
            assert!(normalize_workspace(input).is_err(), "accepted {input:?}");
        }
    }
}
