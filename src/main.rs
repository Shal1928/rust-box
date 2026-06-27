#![windows_subsystem = "windows"]

mod config;

use std::env;
use std::io::Cursor;
use std::path::Path;
use std::process;
use std::process::Stdio;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration};
use std::os::windows::process::CommandExt;

use chrono::Local;
use image::{ImageReader, RgbaImage, Rgba};
use processkit::ProcessGroup;
use sysinfo::System;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    TrayIconBuilder, TrayIconEvent,
};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::platform::run_on_demand::EventLoopExtRunOnDemand;

#[cfg(windows)]
use winapi::um::winuser::{MessageBoxW, MB_DEFBUTTON1, MB_ICONERROR, MB_OK, MB_YESNO};

use rfd::FileDialog;
use winapi::um::winuser::MB_ICONQUESTION;
use crate::config::Config;

const CREATE_NO_WINDOW: u32 = 0x08000000;

// ===== Logging =====
fn log_event(msg: &str) {
    use std::io::Write;
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(dir) = exe_path.parent() {
            let log_path = dir.join("app.log");
            let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
            {
                let _ = writeln!(file, "[{}] {}", timestamp, msg);
            }
        }
    }
    eprintln!("{}", msg);
}

// ===== Icon utilities =====
fn create_green_icon(original_rgba: &[u8], width: u32, height: u32) -> tray_icon::Icon {
    let mut img = RgbaImage::from_raw(width, height, original_rgba.to_vec())
        .expect("Failed to create RgbaImage");
    for y in 0..height {
        for x in 0..width {
            let pixel = img.get_pixel_mut(x, y);
            let [r, g, b, a] = pixel.0;
            if a == 0 { continue; }
            if r > 200 && g > 200 && b > 200 {
                *pixel = Rgba([0, 255, 0, a]);
            }
        }
    }
    let (w, h) = img.dimensions();
    tray_icon::Icon::from_rgba(img.into_raw(), w, h).expect("Failed to create green icon")
}

fn create_colored_icon(original_rgba: &[u8], width: u32, height: u32, color: Rgba<u8>) -> tray_icon::Icon {
    let mut img = RgbaImage::from_raw(width, height, original_rgba.to_vec())
        .expect("Failed to create RgbaImage");
    for y in 0..height {
        for x in 0..width {
            let pixel = img.get_pixel_mut(x, y);
            let [r, g, b, a] = pixel.0;
            if a == 0 { continue; }
            if r > 200 && g > 200 && b > 200 {
                *pixel = color;
            }
        }
    }
    let (w, h) = img.dimensions();
    tray_icon::Icon::from_rgba(img.into_raw(), w, h).expect("Failed to create colored icon")
}

fn create_blue_icon(original_rgba: &[u8], width: u32, height: u32) -> tray_icon::Icon {
    create_colored_icon(original_rgba, width, height, Rgba([0x42, 0xAA, 0xFF, 255]))
}

fn is_process_alive(pid: u32) -> bool {
    let output = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid), "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok();
    if let Some(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout);
        stdout.contains(&pid.to_string())
    } else {
        false
    }
}

enum ChildCommand {
    Install,
    Start,
    Stop,
    UpdateConfigPath(String),
    AutoStart,
}

enum InstallStatus {
    Installing { app_name: String },
    Installed { path: String, app_name: String },
    Failed { app_name: String, error: String },
}

enum DialogCommand {
    Restart,
    Exit,
    RetryAutoStart,
}

enum IconCommand {
    Restore,
    StartRain,
    StopRain,
    Glitch,
    StopGlitch,
    InstallProgress,
    SetRainFrame(tray_icon::Icon),
    SetGlitchFrame(tray_icon::Icon),
    SetInstallFrame(tray_icon::Icon),
}

// ===== Auto-start via Task Scheduler =====
const TASK_NAME: &str = "RustBox";

fn get_username() -> String {
    if let Ok(user) = std::env::var("USERNAME") {
        return user;
    }
    let output = std::process::Command::new("whoami")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok());
    output.unwrap_or_else(|| "".to_string())
}

fn get_autostart_state() -> bool {
    let output = std::process::Command::new("schtasks")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["/query", "/tn", TASK_NAME])
        .output();
    match output {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

fn set_autostart_state(enable: bool) -> Result<(), Box<dyn std::error::Error>> {
    let exe_path = env::current_exe()?.display().to_string();
    let quoted_path = format!("\"{}\"", exe_path);

    if enable {
        let username = get_username();
        if username.is_empty() {
            return Err("Could not determine username for task creation".into());
        }

        let output = std::process::Command::new("schtasks")
            .creation_flags(CREATE_NO_WINDOW)
            .args([
                "/create",
                "/tn",
                TASK_NAME,
                "/tr",
                &quoted_path,
                "/sc",
                "onlogon",
                "/ru",
                &username,
                "/rl",
                "HIGHEST",
                "/it",
                "/f",
            ])
            .output()?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(format!("schtasks create failed: {}", err).into());
        }
        log_event(&format!("Autostart enabled (user {})", username));
    } else {
        let _ = std::process::Command::new("schtasks")
            .creation_flags(CREATE_NO_WINDOW)
            .args(["/delete", "/tn", TASK_NAME, "/f"])
            .output();
        log_event("Autostart disabled");
    }
    Ok(())
}

// ===== Restart dialog =====
#[cfg(windows)]
fn show_restart_dialog() -> DialogCommand {
    let message =
        "The application has crashed or terminated unexpectedly.\nDo you want to restart it?";
    let title = "Rust Box - Error";
    let message_utf16: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    let title_utf16: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let result = unsafe {
        MessageBoxW(
            null_mut(),
            message_utf16.as_ptr(),
            title_utf16.as_ptr(),
            MB_YESNO | MB_ICONERROR | MB_DEFBUTTON1,
        )
    };
    if result == 6 {
        DialogCommand::Restart
    } else {
        DialogCommand::Exit
    }
}

#[cfg(not(windows))]
fn show_restart_dialog() -> DialogCommand {
    DialogCommand::Exit
}

// ===== Package managers =====
fn is_winget_available() -> bool {
    std::process::Command::new("winget")
        .creation_flags(CREATE_NO_WINDOW)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn is_choco_available() -> bool {
    std::process::Command::new("choco")
        .creation_flags(CREATE_NO_WINDOW)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn is_scoop_available() -> bool {
    std::process::Command::new("scoop")
        .creation_flags(CREATE_NO_WINDOW)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ===== Application installation and path resolution =====
fn find_app_by_name(app_name: &str) -> Option<String> {
    let ps_command = format!(
        "Get-Command {} -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source",
        app_name
    );
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &ps_command])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if output.status.success() {
        let path = String::from_utf8(output.stdout).ok()?;
        let path = path.trim();
        if !path.is_empty() && std::path::Path::new(path).exists() {
            return Some(path.to_string());
        }
    }
    None
}

fn resolve_app_path(app_path_from_config: &str) -> Option<String> {
    if (app_path_from_config.contains('\\') || app_path_from_config.contains('/'))
        && app_path_from_config.to_lowercase().ends_with(".exe")
        && std::path::Path::new(app_path_from_config).exists()
    {
        return Some(app_path_from_config.to_string());
    }

    let app_name = app_path_from_config;
    if app_name.is_empty() {
        return None;
    }

    if let Some(path) = find_app_by_name(app_name) {
        return Some(path);
    }

    find_app_binary(app_name)
}

fn find_app_binary(app_name: &str) -> Option<String> {
    if let Some(path) = find_app_by_name(app_name) {
        return Some(path);
    }

    let choco_shim = format!(r"C:\ProgramData\chocolatey\bin\{}.exe", app_name);
    if std::path::Path::new(&choco_shim).exists() {
        return Some(choco_shim);
    }

    let choco_base = format!(r"C:\ProgramData\chocolatey\lib\{}", app_name);
    if let Ok(entries) = std::fs::read_dir(&choco_base) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                if let Some(exe) = find_exe_recursive(&path, app_name) {
                    return Some(exe);
                }
            }
        }
    }

    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        let winget_base = format!(r"{}\Microsoft\WinGet\Packages", local_app_data);
        if let Ok(entries) = std::fs::read_dir(&winget_base) {
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(exe) = find_exe_recursive(&path, app_name) {
                        return Some(exe);
                    }
                }
            }
        }
    }

    if let Ok(user) = std::env::var("USERNAME") {
        let scoop_path = format!(
            r"C:\Users\{}\scoop\apps\{}\current\{}.exe",
            user, app_name, app_name
        );
        if std::path::Path::new(&scoop_path).exists() {
            return Some(scoop_path);
        }
    }

    let common_paths = [
        format!(r"C:\Program Files\{}\{}.exe", app_name, app_name),
        format!(r"C:\Program Files (x86)\{}\{}.exe", app_name, app_name),
    ];
    for path in common_paths.iter() {
        if std::path::Path::new(path).exists() {
            return Some(path.clone());
        }
    }

    None
}

fn find_exe_recursive(dir: &std::path::Path, app_name: &str) -> Option<String> {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                if let Some(exe) = find_exe_recursive(&path, app_name) {
                    return Some(exe);
                }
            } else if let Some(ext) = path.extension() {
                if ext == "exe" {
                    if let Some(stem) = path.file_stem() {
                        if stem.to_string_lossy().to_lowercase() == app_name.to_lowercase() {
                            return Some(path.to_string_lossy().to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

fn show_config_file_dialog() -> Option<String> {
    FileDialog::new()
        .add_filter("JSON files", &["json"])
        .add_filter("All files", &["*"])
        .pick_file()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
}

async fn install_app(app_name: &str) -> Result<String, Box<dyn std::error::Error>> {
    log_event(&format!("=== Install started for app: {} ===", app_name));

    let has_winget = is_winget_available();
    let has_choco = is_choco_available();
    let has_scoop = is_scoop_available();

    log_event(&format!("Has winget: {}, choco: {}, scoop: {}", has_winget, has_choco, has_scoop));

    let methods: Vec<(&str, Box<dyn Fn() -> tokio::process::Command + Send + Sync>)> = vec![
        (
            "winget",
            Box::new(|| {
                let mut cmd = Command::new("winget");
                cmd.args([
                    "install",
                    "--id",
                    app_name,
                    "-e",
                    "--silent",
                    "--source",
                    "winget",
                    "--accept-source-agreements",
                    "--accept-package-agreements",
                    "--disable-interactivity",
                ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .creation_flags(CREATE_NO_WINDOW);
                cmd
            }),
        ),
        (
            "choco",
            Box::new(|| {
                let mut cmd = Command::new("choco");
                cmd.args(["install", app_name, "-y", "--accept-license"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .creation_flags(CREATE_NO_WINDOW);
                cmd
            }),
        ),
        (
            "scoop",
            Box::new(|| {
                let mut cmd = Command::new("scoop");
                cmd.args(["install", app_name])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .creation_flags(CREATE_NO_WINDOW);
                cmd
            }),
        ),
    ];

    for (name, cmd_builder) in methods {
        let available = match name {
            "winget" => is_winget_available(),
            "choco" => is_choco_available(),
            "scoop" => is_scoop_available(),
            _ => false,
        };
        if !available {
            log_event(&format!("Method {} not available, skipping.", name));
            continue;
        }

        log_event(&format!("Attempting install via {}", name));
        let child = match cmd_builder().spawn() {
            Ok(c) => c,
            Err(e) => {
                log_event(&format!("Failed to spawn {}: {}", name, e));
                continue;
            }
        };

        log_event(&format!("{} spawned, waiting for completion...", name));

        let output =
            match tokio::time::timeout(Duration::from_secs(60), child.wait_with_output()).await {
                Ok(Ok(out)) => out,
                Ok(Err(e)) => {
                    log_event(&format!("{} wait error: {}", name, e));
                    continue;
                }
                Err(_) => {
                    log_event(&format!("{} timed out after 60 seconds, skipping to next method.", name));
                    continue;
                }
            };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        log_event(&format!("=== {} stdout ===\n{}", name, stdout));
        log_event(&format!("=== {} stderr ===\n{}", name, stderr));
        log_event(&format!("Exit code: {}", output.status.code().unwrap_or(-1)));

        if output.status.success() {
            log_event(&format!("{} succeeded, searching for binary...", name));
            if let Some(path) = find_app_binary(app_name) {
                log_event(&format!("Found binary at: {}", path));
                return Ok(path);
            } else if let Some(path) = find_app_by_name(app_name) {
                log_event(&format!("Found binary via Get-Command: {}", path));
                return Ok(path);
            } else {
                log_event(&format!("Binary not found after {} install, aborting.", name));
                return Err(format!("Binary not found after {} install", name).into());
            }
        } else {
            log_event(&format!("{} failed with non-zero exit code.", name));
        }
    }

    log_event("All installation methods failed.");
    Err("No installation method succeeded".into())
}

// ===== Main entry point =====
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Write an empty line to separate log sessions (without timestamp)
    {
        use std::io::Write;
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(dir) = exe_path.parent() {
                let log_path = dir.join("app.log");
                if let Ok(mut file) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)
                {
                    let _ = writeln!(file, "");
                }
            }
        }
    }

    let pid = std::process::id();
    log_event(&format!("=== Rust Box started (PID: {}) ===", pid));

    // --- Set current directory to the executable's directory (release only) ---
    if cfg!(not(debug_assertions)) {
        if let Some(exe_dir) = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        {
            if let Err(e) = std::env::set_current_dir(&exe_dir) {
                log_event(&format!("Could not set CWD: {}", e));
            } else {
                log_event(&format!("CWD set to: {:?}", exe_dir));
            }
        }
    }

    let config = match Config::load_or_create_default() {
        Ok(c) => c,
        Err(e) => {
            log_event(&format!("Config error: {}", e));
            eprintln!("{}", e);
            process::exit(1);
        }
    };

    let app_path = config.app_path.as_ref().unwrap().clone();
    let cfg_path = config.cfg_path.as_ref().unwrap().clone();

    log_event(&format!("Config loaded: app_path='{}', cfg_path='{}'", app_path, cfg_path));

    let file_stem = Path::new(&app_path)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let app_name = Path::new(&app_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let cfg_path_for_gui = Arc::new(Mutex::new(cfg_path.clone()));

    log_event(&format!("Startup: killing any existing {} processes", app_name));
    let _ = Command::new("taskkill")
        .creation_flags(CREATE_NO_WINDOW)
        .args(["/F", "/IM", &app_name])
        .output()
        .await;

    // Load tray icon
    let icon_bytes = include_bytes!("../rust-box.png");
    let img = ImageReader::new(Cursor::new(icon_bytes))
        .with_guessed_format()?
        .decode()?
        .into_rgba8();
    let (width, height) = img.dimensions();
    let orig_rgba = img.into_raw();
    let icon = tray_icon::Icon::from_rgba(orig_rgba.clone(), width, height)?;

    let is_running = Arc::new(AtomicBool::new(false));
    let is_running_task = is_running.clone();

    let resolved_path = resolve_app_path(&app_path);
    let app_installed = resolved_path.is_some();
    let cfg_exists = Path::new(&cfg_path).exists();

    log_event(&format!("Startup state: app_installed={}, cfg_exists={}", app_installed, cfg_exists));

    // Menu items with emojis
    let start_menu_item = MenuItem::new(
        if app_installed && cfg_exists {
            format!("▶ Start [{}]", &file_stem)
        } else if app_installed {
            format!("▶ Start [{}]", &file_stem)
        } else {
            format!("⤵ Install [{}]", &file_stem)
        },
        true,
        None,
    );
    let config_app_item = MenuItem::new(
        if cfg_exists {
            format!("Open json [{}]", &file_stem)
        } else {
            "Select json file".to_string()
        },
        true,
        None,
    );
    let autostart_initial = get_autostart_state();
    let autostart_item = MenuItem::new(
        if autostart_initial {
            "Auto-start: [ON]-OFF"
        } else {
            "Auto-start: ON-[OFF]"
        },
        app_installed,
        None,
    );
    let config_rustbox_item = MenuItem::new("Config", true, None);
    let reload_config_item = MenuItem::new("Reload", true, None);
    let quit_item = MenuItem::new("Exit", true, None);

    let start_menu_id = start_menu_item.id().clone();
    let autostart_id = autostart_item.id().clone();
    let config_rustbox_id = config_rustbox_item.id().clone();
    let config_app_id = config_app_item.id().clone();
    let reload_config_id = reload_config_item.id().clone();
    let quit_id = quit_item.id().clone();

    let menu = Menu::new();
    menu.append(&start_menu_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&config_app_item)?;
    menu.append(&autostart_item)?;
    // menu.append(&PredefinedMenuItem::separator())?;
    // menu.append(&config_rustbox_item)?;
    // menu.append(&reload_config_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit_item)?;

    let tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Rust Box")
        .with_icon(icon.clone())
        .build()?;

    log_event("Tray icon created and menu built");

    // Channels
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<ChildCommand>();
    let (install_tx, mut install_rx) = mpsc::unbounded_channel::<InstallStatus>();
    let (gui_tx, mut gui_rx) = mpsc::unbounded_channel::<bool>();
    let (dialog_tx, mut dialog_rx) = mpsc::unbounded_channel::<DialogCommand>();
    let (icon_cmd_tx, mut icon_cmd_rx) = mpsc::unbounded_channel::<IconCommand>();

    // Separate clones for different contexts
    let icon_cmd_tx_for_manager = icon_cmd_tx.clone();
    let icon_cmd_tx_gui = icon_cmd_tx.clone();

    let tray_event_rx = TrayIconEvent::receiver();
    let menu_event_rx = MenuEvent::receiver();

    // Clones for manager
    let cmd_tx_clone = cmd_tx.clone();
    let install_tx_clone = install_tx.clone();
    let cfg_path_clone_manager = cfg_path.clone();
    let resolved_app_path = resolve_app_path(&app_path).unwrap_or_else(|| app_path.clone());
    let app_name_clone = Path::new(&resolved_app_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let app_path_clone = resolved_app_path.clone();

    // Animation threads control
    let anim_running = Arc::new(AtomicBool::new(false));
    let anim_handle: Arc<Mutex<Option<std::thread::JoinHandle<()>>>> = Arc::new(Mutex::new(None));

    // --- Manager task ---
    let manager_handle = tokio::spawn(async move {
        let mut child_pid: Option<u32> = None;
        let mut process_group: Option<ProcessGroup> = None;
        let mut child_handle: Option<tokio::process::Child> = None;
        let mut dialog_shown = false;
        let mut current_app_path = app_path_clone;
        let mut current_app_name = app_name_clone;
        let mut current_cfg_path = cfg_path_clone_manager;
        let mut auto_start_attempts = 0;
        let mut auto_start_pending = false;

        // Clone for use inside loop
        let icon_tx = icon_cmd_tx_for_manager.clone();

        loop {
            tokio::select! {
                Some(cmd) = cmd_rx.recv() => {
                    match cmd {
                        ChildCommand::UpdateConfigPath(new_path) => {
                            current_cfg_path = new_path.clone();
                            log_event(&format!("Config path updated in manager to: {}", new_path));
                        }
                        ChildCommand::Install => {
                            let app_name = Path::new(&current_app_path)
                                .file_stem()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_string();

                            let _ = install_tx_clone.send(InstallStatus::Installing {
                                app_name: app_name.clone(),
                            });

                            let install_result = install_app(&app_name).await;

                            match install_result {
                                Ok(installed_path) => {
                                    current_app_path = installed_path.clone();
                                    current_app_name = Path::new(&installed_path)
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .to_string();

                                    log_event(&format!("Installation succeeded: {}", installed_path));
                                    let _ = install_tx_clone.send(InstallStatus::Installed {
                                        path: installed_path,
                                        app_name,
                                    });
                                }
                                Err(e) => {
                                    log_event(&format!("Installation failed: {}", e));
                                    let _ = install_tx_clone.send(InstallStatus::Failed {
                                        app_name,
                                        error: e.to_string(),
                                    });
                                }
                            }
                        }
                        ChildCommand::AutoStart => {
                            auto_start_pending = true;
                            auto_start_attempts = 0;
                            let _ = cmd_tx_clone.send(ChildCommand::Start);
                        }
                        ChildCommand::Start => {
                            if !std::path::Path::new(&current_app_path).exists() {
                                log_event(&format!("ERROR: Application not found at: {}", current_app_path));
                                if !auto_start_pending {
                                    let _ = gui_tx.send(false);
                                }
                                continue;
                            }

                            if !std::path::Path::new(&current_cfg_path).exists() {
                                log_event(&format!("ERROR: Config file not found: {}", current_cfg_path));
                                if !auto_start_pending {
                                    let _ = gui_tx.send(false);
                                }
                                continue;
                            }

                            if child_pid.is_none() {
                                log_event(&format!("Starting child: {} with config {}", current_app_path, current_cfg_path));

                                // Start rain animation (send command to GUI)
                                let _ = icon_tx.send(IconCommand::StartRain);

                                // Kill any existing process by name
                                let _ = Command::new("taskkill")
                                    .creation_flags(CREATE_NO_WINDOW)
                                    .args(["/F", "/IM", &current_app_name])
                                    .output()
                                    .await;

                                // Also kill by PID if we had one
                                if let Some(pid) = child_pid {
                                    let _ = Command::new("taskkill")
                                        .creation_flags(CREATE_NO_WINDOW)
                                        .args(["/F", "/PID", &pid.to_string()])
                                        .output()
                                        .await;
                                    child_pid = None;
                                }

                                // --- Force remove TUN adapter to release resources completely ---
                                log_event("Removing TUN adapter...");
                                let remove_script = r#"
$adapter = Get-NetAdapter | Where-Object { $_.Name -like "*tun*" -or $_.Name -like "*sing*" -or $_.Name -like "*wintun*" }
if ($adapter) {
    $adapter | Disable-NetAdapter -Confirm:$false -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2
    $adapter | Remove-NetAdapter -Confirm:$false -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 3
    Write-Host "TUN adapter removed"
} else {
    Write-Host "No TUN adapter found"
}
"#;
                                let remove_output = Command::new("powershell")
                                    .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", remove_script])
                                    .creation_flags(CREATE_NO_WINDOW)
                                    .output()
                                    .await;
                                if let Ok(out) = remove_output {
                                    let stdout = String::from_utf8_lossy(&out.stdout);
                                    let stderr = String::from_utf8_lossy(&out.stderr);
                                    log_event(&format!("Remove output: {} {}", stdout, stderr));
                                }

                                let ps_script = format!(
                                    r#"$p = Start-Process -FilePath "{}" -ArgumentList "run","-c","{}" -Verb RunAs -WindowStyle Hidden -PassThru; if ($p) {{ $p.Id }}"#,
                                    current_app_path, current_cfg_path
                                );
                                log_event(&format!("Launching: {} with config: {}", current_app_path, current_cfg_path));

                                // Create a process group to ensure termination
                                let group = match ProcessGroup::new() {
                                    Ok(g) => g,
                                    Err(e) => {
                                        log_event(&format!("Failed to create ProcessGroup: {}", e));
                                        if !auto_start_pending {
                                            let _ = gui_tx.send(false);
                                        }
                                        continue;
                                    }
                                };

                                // Use tokio::process::Command with CREATE_NO_WINDOW flag
                                let mut cmd = Command::new("powershell");
                                cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps_script])
                                    .stdout(std::process::Stdio::piped())
                                    .stderr(std::process::Stdio::piped())
                                    .creation_flags(CREATE_NO_WINDOW);

                                let mut child = match cmd.spawn() {
                                    Ok(c) => c,
                                    Err(e) => {
                                        log_event(&format!("Failed to spawn child: {}", e));
                                        if !auto_start_pending {
                                            let _ = gui_tx.send(false);
                                        }
                                        continue;
                                    }
                                };

                                // Adopt the child into the process group (so it gets killed on main exit)
                                if let Err(e) = group.adopt(&mut child) {
                                    log_event(&format!("Failed to adopt child into group: {}", e));
                                    // Continue anyway – process is still running
                                }

                                let mut stdout = String::new();
                                let mut stderr = String::new();
                                if let Some(mut out) = child.stdout.take() {
                                    let _ = out.read_to_string(&mut stdout).await;
                                }
                                if let Some(mut err) = child.stderr.take() {
                                    let _ = err.read_to_string(&mut stderr).await;
                                }
                                let _ = child.wait().await;

                                if !stderr.is_empty() {
                                    log_event(&format!("PowerShell stderr: {}", stderr));
                                }

                                let pid_str = stdout.trim();
                                log_event(&format!("PowerShell stdout: '{}'", pid_str));

                                if let Ok(pid) = pid_str.parse::<u32>() {
                                    // Use is_process_alive to verify
                                    if !is_process_alive(pid) {
                                        // Process died immediately
                                        log_event(&format!("Child process {} died immediately", pid));

                                        if auto_start_pending {
                                            // Progressive retry: 2, 4, 6, 8, 10 seconds
                                            let delay_secs = 2 + (auto_start_attempts * 2);
                                            auto_start_attempts += 1;
                                            if auto_start_attempts < 5 {
                                                log_event(&format!("Auto-start attempt {} failed, retrying in {} seconds...", auto_start_attempts, delay_secs));
                                                let cmd_tx_retry = cmd_tx_clone.clone();
                                                tokio::spawn(async move {
                                                    tokio::time::sleep(Duration::from_secs(delay_secs)).await;
                                                    let _ = cmd_tx_retry.send(ChildCommand::Start);
                                                });
                                                continue;
                                            } else {
                                                // all attempts exhausted
                                                auto_start_pending = false;
                                                auto_start_attempts = 0;
                                                let dialog_tx_clone = dialog_tx.clone();
                                                let _ = dialog_tx_clone.send(DialogCommand::RetryAutoStart);
                                                continue;
                                            }
                                        } else {
                                            // normal start, stop rain and restore icon
                                            let _ = icon_tx.send(IconCommand::StopRain);
                                            let _ = gui_tx.send(false);
                                            continue;
                                        }
                                    }

                                    // Successfully started
                                    child_pid = Some(pid);
                                    process_group = Some(group);
                                    child_handle = Some(child);
                                    is_running_task.store(true, Ordering::SeqCst);
                                    dialog_shown = false;
                                    auto_start_pending = false;
                                    auto_start_attempts = 0;
                                    // Wait 5 seconds before stopping rain animation
                                    let icon_tx_delayed = icon_tx.clone();
                                    tokio::spawn(async move {
                                        tokio::time::sleep(Duration::from_secs(5)).await;
                                        let _ = icon_tx_delayed.send(IconCommand::StopRain);
                                    });
                                    log_event(&format!("Child process started successfully (PID: {})", pid));
                                    let _ = gui_tx.send(true);
                                } else {
                                    log_event(&format!("Failed to parse PID from stdout: '{}'", pid_str));
                                    if auto_start_pending {
                                        let delay_secs = 2 + (auto_start_attempts * 2);
                                        auto_start_attempts += 1;
                                        if auto_start_attempts < 5 {
                                            log_event(&format!("Auto-start attempt {} failed (no PID), retrying in {} seconds...", auto_start_attempts, delay_secs));
                                            let cmd_tx_retry = cmd_tx_clone.clone();
                                            tokio::spawn(async move {
                                                tokio::time::sleep(Duration::from_secs(delay_secs)).await;
                                                let _ = cmd_tx_retry.send(ChildCommand::Start);
                                            });
                                            continue;
                                        } else {
                                            auto_start_pending = false;
                                            auto_start_attempts = 0;
                                            let dialog_tx_clone = dialog_tx.clone();
                                            let _ = dialog_tx_clone.send(DialogCommand::RetryAutoStart);
                                            continue;
                                        }
                                    } else {
                                        let _ = icon_tx.send(IconCommand::StopRain);
                                        let _ = gui_tx.send(false);
                                        continue;
                                    }
                                }
                            }
                        }
                        ChildCommand::Stop => {
                            log_event("Stopping child process...");

                            // Start glitch animation
                            let _ = icon_tx.send(IconCommand::Glitch);

                            // Try graceful shutdown first
                            if let Some(pid) = child_pid {
                                let _ = Command::new("taskkill")
                                    .creation_flags(CREATE_NO_WINDOW)
                                    .args(["/PID", &pid.to_string()])
                                    .output()
                                    .await;
                                // Wait a few seconds for graceful exit
                                for _ in 0..10 {
                                    tokio::time::sleep(Duration::from_millis(250)).await;
                                    if !is_process_alive(pid) {
                                        break;
                                    }
                                }
                                // If still alive, force kill
                                if is_process_alive(pid) {
                                    let _ = Command::new("taskkill")
                                        .creation_flags(CREATE_NO_WINDOW)
                                        .args(["/F", "/PID", &pid.to_string()])
                                        .output()
                                        .await;
                                    tokio::time::sleep(Duration::from_secs(1)).await;
                                }
                            }

                            // Also kill via process group
                            if let Some(group) = process_group.take() {
                                drop(group);
                                log_event("ProcessGroup dropped");
                            }
                            if let Some(mut child) = child_handle.take() {
                                let _ = child.kill().await;
                                let _ = child.wait().await;
                            }

                            // Kill by name as fallback
                            log_event(&format!("Stop: killing all {} processes", current_app_name));
                            let _ = Command::new("taskkill")
                                .creation_flags(CREATE_NO_WINDOW)
                                .args(["/F", "/IM", &current_app_name])
                                .output()
                                .await;

                            // Wait for process to fully terminate
                            if let Some(pid) = child_pid {
                                for _ in 0..15 {
                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                    let sys = System::new_all();
                                    let still_running = sys.processes().values().any(|p| p.pid().as_u32() == pid);
                                    if !still_running { break; }
                                }
                            }

                            child_pid = None;
                            is_running_task.store(false, Ordering::SeqCst);
                            dialog_shown = false;
                            // Stop glitch and restore default icon
                            let _ = icon_tx.send(IconCommand::StopGlitch);
                            log_event("Child process stopped.");
                            let _ = gui_tx.send(false);
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if let Some(pid) = child_pid {
                        let sys = System::new_all();
                        let still_running = sys.processes().values().any(|p| p.pid().as_u32() == pid);
                        if !still_running {
                            log_event(&format!("Child process (PID {}) terminated unexpectedly", pid));
                            child_pid = None;
                            process_group.take();
                            child_handle.take();
                            is_running_task.store(false, Ordering::SeqCst);
                            let _ = gui_tx.send(false);

                            // Give system a moment to release resources before showing dialog
                            tokio::time::sleep(Duration::from_secs(1)).await;

                            if !dialog_shown {
                                dialog_shown = true;
                                let dialog_tx_clone = dialog_tx.clone();
                                tokio::task::spawn_blocking(move || {
                                    let choice = show_restart_dialog();
                                    let _ = dialog_tx_clone.send(choice);
                                });
                            }
                        }
                    }
                }
            }
        }
    });

    if autostart_initial && app_installed && cfg_exists {
        log_event("Auto-start enabled: starting child automatically");
        let _ = cmd_tx.send(ChildCommand::AutoStart);
    }

    // --- GUI event loop ---
    let mut event_loop = EventLoop::new()?;
    let cmd_tx_main = cmd_tx.clone();
    let cmd_tx_main_clone = cmd_tx_main.clone();
    let app_installed_flag = Arc::new(AtomicBool::new(app_installed));
    let _app_installed_flag_gui = app_installed_flag.clone();

    // Clones for use inside closure
    let anim_running_clone = anim_running.clone();
    let anim_handle_clone = anim_handle.clone();

    // Helper to stop animation thread (defined inside closure to capture clones)
    let stop_anim = |running: &Arc<AtomicBool>, handle: &Arc<Mutex<Option<std::thread::JoinHandle<()>>>>| {
        running.store(false, Ordering::SeqCst);
        if let Some(h) = handle.lock().unwrap().take() {
            let _ = h.join();
        }
    };

    event_loop.run_on_demand(move |_event, window_target| {
        // Use Wait to avoid CPU hogging – animation is handled by separate threads
        window_target.set_control_flow(ControlFlow::Wait);

        // Update Start/Stop button label from manager
        while let Ok(running) = gui_rx.try_recv() {
            let text = if running {
                format!("◼ Stop [{}]", &file_stem)
            } else {
                format!("▶ Start [{}]", &file_stem)
            };
            let _ = start_menu_item.set_text(text);
        }

        // Handle installation status updates
        while let Ok(status) = install_rx.try_recv() {
            match status {
                InstallStatus::Installing { app_name } => {
                    log_event(&format!("Installation started for {}", app_name));
                    let _ = start_menu_item.set_text(format!("Installing [{}]...", app_name));
                    let _ = start_menu_item.set_enabled(false);
                    let _ = icon_cmd_tx_gui.send(IconCommand::InstallProgress);
                }
                InstallStatus::Installed { path, app_name } => {
                    log_event(&format!("Installation completed: {} at {}", app_name, path));
                    stop_anim(&anim_running_clone, &anim_handle_clone);
                    let _ = icon_cmd_tx_gui.send(IconCommand::Restore);
                    let _ = start_menu_item.set_text(format!("▶ Start [{}]", app_name));
                    let _ = start_menu_item.set_enabled(true);
                    let _ = config_app_item.set_enabled(true);
                    let _ = autostart_item.set_enabled(true);
                    app_installed_flag.store(true, Ordering::SeqCst);
                    log_event(&format!("✅ App installed at: {}", path));
                }
                InstallStatus::Failed { app_name, error } => {
                    log_event(&format!("Installation failed for {}: {}", app_name, error));
                    stop_anim(&anim_running_clone, &anim_handle_clone);
                    let _ = icon_cmd_tx_gui.send(IconCommand::Restore);
                    let _ = start_menu_item.set_text(format!("⤵ Install [{}]", app_name));
                    let _ = start_menu_item.set_enabled(true);
                    log_event(&format!("❌ Installation failed: {}", error));
                }
            }
        }

        // Handle icon commands
        while let Ok(cmd) = icon_cmd_rx.try_recv() {
            match cmd {
                IconCommand::Restore => {
                    stop_anim(&anim_running_clone, &anim_handle_clone);
                    let _ = tray_icon.set_icon(Some(icon.clone()));
                }
                IconCommand::StartRain => {
                    // Stop previous animation
                    stop_anim(&anim_running_clone, &anim_handle_clone);
                    anim_running_clone.store(true, Ordering::SeqCst);
                    let running = anim_running_clone.clone();
                    let cmd_tx = icon_cmd_tx_gui.clone();
                    let orig_rgba_local = orig_rgba.clone();
                    let w = width;
                    let h = height;
                    let handle = std::thread::spawn(move || {
                        let mut toggle = false;
                        let green_icon = create_green_icon(&orig_rgba_local, w, h);
                        let original_icon = tray_icon::Icon::from_rgba(orig_rgba_local.clone(), w, h).unwrap();
                        while running.load(Ordering::SeqCst) {
                            let icon_to_send = if toggle { original_icon.clone() } else { green_icon.clone() };
                            let _ = cmd_tx.send(IconCommand::SetRainFrame(icon_to_send));
                            toggle = !toggle;
                            std::thread::sleep(Duration::from_millis(350));
                        }
                    });
                    *anim_handle_clone.lock().unwrap() = Some(handle);
                }
                IconCommand::StopRain => {
                    stop_anim(&anim_running_clone, &anim_handle_clone);
                    let green_icon_local = create_green_icon(&orig_rgba, width, height);
                    let _ = tray_icon.set_icon(Some(green_icon_local));
                }
                IconCommand::Glitch => {
                    stop_anim(&anim_running_clone, &anim_handle_clone);
                    anim_running_clone.store(true, Ordering::SeqCst);
                    let running = anim_running_clone.clone();
                    let cmd_tx = icon_cmd_tx_gui.clone();
                    let orig_rgba_local = orig_rgba.clone();
                    let w = width;
                    let h = height;
                    let handle = std::thread::spawn(move || {
                        let mut toggle = false;
                        let green_icon = create_green_icon(&orig_rgba_local, w, h);
                        let original_icon = tray_icon::Icon::from_rgba(orig_rgba_local.clone(), w, h).unwrap();
                        while running.load(Ordering::SeqCst) {
                            let icon_to_send = if toggle { original_icon.clone() } else { green_icon.clone() };
                            let _ = cmd_tx.send(IconCommand::SetGlitchFrame(icon_to_send));
                            toggle = !toggle;
                            std::thread::sleep(Duration::from_millis(350));
                        }
                    });
                    *anim_handle_clone.lock().unwrap() = Some(handle);
                }
                IconCommand::StopGlitch => {
                    stop_anim(&anim_running_clone, &anim_handle_clone);
                    let _ = tray_icon.set_icon(Some(icon.clone()));
                }
                IconCommand::InstallProgress => {
                    stop_anim(&anim_running_clone, &anim_handle_clone);
                    anim_running_clone.store(true, Ordering::SeqCst);
                    let running = anim_running_clone.clone();
                    let cmd_tx = icon_cmd_tx_gui.clone();
                    let orig_rgba_local = orig_rgba.clone();
                    let w = width;
                    let h = height;
                    let handle = std::thread::spawn(move || {
                        let mut toggle = false;
                        let blue_icon = create_blue_icon(&orig_rgba_local, w, h);
                        let original_icon = tray_icon::Icon::from_rgba(orig_rgba_local.clone(), w, h).unwrap();
                        while running.load(Ordering::SeqCst) {
                            let icon_to_send = if toggle { original_icon.clone() } else { blue_icon.clone() };
                            let _ = cmd_tx.send(IconCommand::SetInstallFrame(icon_to_send));
                            toggle = !toggle;
                            std::thread::sleep(Duration::from_millis(350));
                        }
                    });
                    *anim_handle_clone.lock().unwrap() = Some(handle);
                }
                IconCommand::SetRainFrame(icon_frame) => {
                    let _ = tray_icon.set_icon(Some(icon_frame));
                }
                IconCommand::SetGlitchFrame(icon_frame) => {
                    let _ = tray_icon.set_icon(Some(icon_frame));
                }
                IconCommand::SetInstallFrame(icon_frame) => {
                    let _ = tray_icon.set_icon(Some(icon_frame));
                }
            }
        }

        // Process menu events (unchanged)
        while let Ok(menu_event) = menu_event_rx.try_recv() {
            let id = menu_event.id;

            if id == start_menu_id {
                let installed = app_installed_flag.load(Ordering::SeqCst);
                let cfg_exists = Path::new(&*cfg_path_for_gui.lock().unwrap()).exists();
                if !installed {
                    let _ = cmd_tx_main.send(ChildCommand::Install);
                } else if installed && cfg_exists {
                    let running = is_running.load(Ordering::SeqCst);
                    if !running {
                        let _ = start_menu_item.set_text(format!("◼ Stop [{}]", &file_stem));
                        let _ = cmd_tx_main.send(ChildCommand::Start);
                    } else {
                        let _ = cmd_tx_main.send(ChildCommand::Stop);
                    }
                } else {
                    let msg = "Application installed, but config file missing. Please select config.";
                    log_event(msg);
                    #[cfg(windows)]
                    unsafe {
                        let msg_utf16: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
                        let title = "Rust Box - Info";
                        let title_utf16: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
                        MessageBoxW(
                            null_mut(),
                            msg_utf16.as_ptr(),
                            title_utf16.as_ptr(),
                            MB_OK | MB_ICONERROR,
                        );
                    }
                }
            } else if id == autostart_id {
                let current = get_autostart_state();
                let new_state = !current;
                match set_autostart_state(new_state) {
                    Ok(_) => {
                        let text = if new_state {
                            "Auto-start: [ON]-OFF"
                        } else {
                            "Auto-start: ON-[OFF]"
                        };
                        let _ = autostart_item.set_text(text);
                        log_event(&format!("Autostart set to {}", new_state));
                    }
                    Err(e) => {
                        log_event(&format!("Failed to change autostart: {}", e));
                        #[cfg(windows)]
                        unsafe {
                            let msg_utf16: Vec<u16> = e.to_string().encode_utf16().chain(Some(0)).collect();
                            let title = "Rust Box - Error";
                            let title_utf16: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
                            MessageBoxW(
                                null_mut(),
                                msg_utf16.as_ptr(),
                                title_utf16.as_ptr(),
                                MB_OK | MB_ICONERROR,
                            );
                        }
                    }
                }
            } else if id == config_rustbox_id {
                log_event("Opening rust-box config in Notepad");
                let _ = std::process::Command::new("notepad")
                    .arg(&"rust-box.cfg")
                    .spawn();
            } else if id == reload_config_id {
                log_event("Reload triggered: stopping child and restarting app");
                let _ = cmd_tx_main.send(ChildCommand::Stop);
                std::thread::sleep(Duration::from_secs(2));
                let exe = std::env::current_exe().expect("failed to get exe path");
                let args: Vec<String> = std::env::args().collect();
                let _ = std::process::Command::new(exe)
                    .creation_flags(CREATE_NO_WINDOW)
                    .args(&args[1..])
                    .spawn();
                window_target.exit();
            } else if id == config_app_id {
                let current_cfg = cfg_path_for_gui.lock().unwrap().clone();
                let cfg_exists = Path::new(&current_cfg).exists();
                if cfg_exists {
                    log_event(&format!("Opening json config in Notepad: {}", current_cfg));
                    let _ = std::process::Command::new("notepad")
                        .arg(&current_cfg)
                        .spawn();
                } else {
                    log_event("Config file not found, showing file dialog");
                    if let Some(selected) = show_config_file_dialog() {
                        log_event(&format!("User selected config file: {}", selected));
                        if let Err(e) = Config::update_cfg_path(&selected) {
                            log_event(&format!("Failed to update cfg_path: {}", e));
                            #[cfg(windows)]
                            unsafe {
                                let msg_utf16: Vec<u16> = e.to_string().encode_utf16().chain(Some(0)).collect();
                                let title = "Rust Box - Error";
                                let title_utf16: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
                                MessageBoxW(
                                    null_mut(),
                                    msg_utf16.as_ptr(),
                                    title_utf16.as_ptr(),
                                    MB_OK | MB_ICONERROR,
                                );
                            }
                        } else {
                            log_event(&format!("Config path updated to: {}", selected));
                            *cfg_path_for_gui.lock().unwrap() = selected.clone();
                            let _ = config_app_item.set_text(format!("Open json [{}]", &file_stem));
                            if app_installed_flag.load(Ordering::SeqCst) {
                                let _ = start_menu_item.set_enabled(true);
                            }
                            let _ = cmd_tx_main.send(ChildCommand::UpdateConfigPath(selected));
                        }
                    } else {
                        log_event("User cancelled file selection");
                    }
                }
            } else if id == quit_id {
                log_event("Quit requested, stopping child and exiting");
                let _ = cmd_tx_main.send(ChildCommand::Stop);
                std::thread::sleep(Duration::from_millis(500));
                window_target.exit();
            }
        }

        // Handle dialog responses (restart or exit)
        while let Ok(dialog_cmd) = dialog_rx.try_recv() {
            match dialog_cmd {
                DialogCommand::Restart => {
                    log_event("User chose to restart the child process after crash");
                    let _ = cmd_tx_main_clone.send(ChildCommand::Start);
                }
                DialogCommand::Exit => {
                    log_event("User chose to exit after child crash");
                    let _ = cmd_tx_main_clone.send(ChildCommand::Stop);
                    std::thread::sleep(Duration::from_millis(500));
                    window_target.exit();
                }
                DialogCommand::RetryAutoStart => {
                    #[cfg(windows)]
                    unsafe {
                        let msg = "Auto-start failed after 5 attempts. Retry?";
                        let title = "Rust Box - Auto-start";
                        let msg_utf16: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
                        let title_utf16: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
                        let result = MessageBoxW(
                            null_mut(),
                            msg_utf16.as_ptr(),
                            title_utf16.as_ptr(),
                            MB_YESNO | MB_ICONQUESTION,
                        );
                        if result == 6 {
                            let _ = cmd_tx_main_clone.send(ChildCommand::AutoStart);
                        }
                    }
                }
            }
        }

        // Left-click on tray icon shows the menu
        while let Ok(tray_event) = tray_event_rx.try_recv() {
            if let TrayIconEvent::Click { button, .. } = tray_event {
                if button == tray_icon::MouseButton::Left {
                    let _ = tray_icon.show_menu();
                }
            }
        }
    })?;

    // Final cleanup
    anim_running.store(false, Ordering::SeqCst);
    if let Some(h) = anim_handle.lock().unwrap().take() {
        let _ = h.join();
    }
    log_event("Main exit: sending stop command and aborting manager");
    let _ = cmd_tx.send(ChildCommand::Stop);
    tokio::time::sleep(Duration::from_millis(500)).await;
    manager_handle.abort();

    log_event("=== Rust Box terminated ===");
    Ok(())
}