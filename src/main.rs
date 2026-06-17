#![windows_subsystem = "windows"]

mod config;

use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::io::Cursor;
use std::time::Duration;
use std::path::Path;
use std::ptr::null_mut;
use std::env;

use tray_icon::{
    menu::{Menu, MenuItem, PredefinedMenuItem, MenuEvent},
    TrayIconBuilder, TrayIconEvent,
};
use winit::event_loop::{EventLoop, ControlFlow};
use winit::platform::run_on_demand::EventLoopExtRunOnDemand;
use image::ImageReader;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::io::AsyncReadExt;
use sysinfo::System;
use processkit::ProcessGroup;

#[cfg(windows)]
use winapi::um::winuser::{MessageBoxW, MB_YESNO, MB_ICONERROR, MB_DEFBUTTON1, MB_OK};

use crate::config::Config;

const CREATE_NO_WINDOW: u32 = 0x08000000;

enum ChildCommand {
    Start,
    Stop,
}

enum DialogCommand {
    Restart,
    Exit,
}

// ===== Auto-start via Task Scheduler (interactive user session) =====
const TASK_NAME: &str = "RustBox";

/// Get current Windows username for task scheduler.
fn get_username() -> String {
    // Try environment variable first
    if let Ok(user) = std::env::var("USERNAME") {
        return user;
    }
    // Fallback: use `whoami` command
    let output = std::process::Command::new("whoami")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok());
    output.unwrap_or_else(|| "".to_string())
}

/// Check if the scheduled task exists.
fn get_autostart_state() -> bool {
    let output = std::process::Command::new("schtasks")
        .args(["/query", "/tn", TASK_NAME])
        .output();
    match output {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

/// Create or delete a scheduled task for auto‑start with interactive session.
fn set_autostart_state(enable: bool) -> Result<(), Box<dyn std::error::Error>> {
    let exe_path = env::current_exe()?.display().to_string();
    let quoted_path = format!("\"{}\"", exe_path);

    if enable {
        let username = get_username();
        if username.is_empty() {
            return Err("Could not determine username for task creation".into());
        }

        let output = std::process::Command::new("schtasks")
            .args([
                "/create",
                "/tn", TASK_NAME,
                "/tr", &quoted_path,
                "/sc", "onlogon",
                "/ru", &username,
                "/rl", "HIGHEST",
                "/it",
                "/f",
            ])
            .output()?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(format!("schtasks create failed: {}", err).into());
        }
        eprintln!("✅ Autostart enabled (task as user {} with interactive session)", username);
    } else {
        let _ = std::process::Command::new("schtasks")
            .args(["/delete", "/tn", TASK_NAME, "/f"])
            .output();
        eprintln!("✅ Autostart disabled");
    }
    Ok(())
}

// ===== Restart dialog =====
/// Show a Windows message box asking whether to restart the crashed child process.
#[cfg(windows)]
fn show_restart_dialog() -> DialogCommand {
    let message = "sing-box has crashed or terminated unexpectedly.\nDo you want to restart it?";
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
    if result == 6 { DialogCommand::Restart } else { DialogCommand::Exit }
}

#[cfg(not(windows))]
fn show_restart_dialog() -> DialogCommand { DialogCommand::Exit }

// ===== Main entry point =====
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // --- Set current directory to the executable's directory (release only) ---
    if cfg!(not(debug_assertions)) {
        if let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
            if let Err(e) = std::env::set_current_dir(&exe_dir) {
                eprintln!("Warning: Could not set CWD: {}", e);
            } else {
                eprintln!("CWD set to: {:?}", exe_dir);
            }
        }
    }

    // Load configuration
    let config = Config::build(std::env::args()).unwrap_or_else(|err| {
        eprintln!("Config error: {err}");
        process::exit(1);
    });

    let app_path = config.app_path.as_ref().unwrap().clone();
    let cfg_path = config.cfg_path.as_ref().unwrap().clone();

    let app_name = Path::new(&app_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let app_stem = Path::new(&app_path)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    // Path to the rust-box config file (default if not provided)
    let config_path = env::args().nth(1).unwrap_or_else(|| "rust-box.cfg".to_string());

    // Clone for GUI use (opening the child config)
    let cfg_path_for_gui = cfg_path.clone();


    // Diagnostic logging (now uses the correct CWD)
    // let log_path = std::env::current_dir()
    //     .unwrap_or_else(|_| Path::new(".").to_path_buf())
    //     .join("startup.log");
    // if let Ok(mut file) = std::fs::OpenOptions::new()
    //     .create(true)
    //     .append(true)
    //     .open(&log_path)
    // {
    //     use std::io::Write;
    //     let _ = writeln!(file, "Started at {:?}", std::time::SystemTime::now());
    //     let _ = writeln!(file, "Args: {:?}", std::env::args().collect::<Vec<_>>());
    //     let _ = writeln!(file, "CWD: {:?}", std::env::current_dir());
    //     let _ = writeln!(file, "Exe: {:?}", std::env::current_exe());
    //     let _ = writeln!(file, "---");
    // }


    // Cleanup orphaned processes on startup
    eprintln!("Startup: killing any existing {} processes", app_name);
    let _ = Command::new("taskkill")
        .args(["/F", "/IM", &app_name])
        .output()
        .await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Load tray icon from embedded PNG
    let icon_bytes = include_bytes!("../rust-box.png");
    let img = ImageReader::new(Cursor::new(icon_bytes))
        .with_guessed_format()?
        .decode()?
        .into_rgba8();
    let (width, height) = img.dimensions();
    let icon = tray_icon::Icon::from_rgba(img.into_raw(), width, height)?;

    // Shared state for Start/Stop toggle
    let is_running = Arc::new(AtomicBool::new(false));
    let is_running_task = is_running.clone();

    // Build tray menu
    let start_menu_item = MenuItem::new(format!("Start [{}]", app_stem), true, None);
    let autostart_initial = get_autostart_state();
    let autostart_item = MenuItem::new(
        if autostart_initial { "Auto-start: [ON]-OFF" } else { "Auto-start: ON-[OFF]" },
        true,
        None,
    );
    let config_rustbox_item = MenuItem::new("Config", true, None);
    let reload_config_item = MenuItem::new("Reload", true, None);
    let config_app_item = MenuItem::new(format!("Config [{}]", app_stem), true, None);
    let quit_item = MenuItem::new("Exit", true, None);

    let start_menu_id = start_menu_item.id().clone();
    let autostart_id = autostart_item.id().clone();
    let config_rustbox_id = config_rustbox_item.id().clone();
    let config_app_id = config_app_item.id().clone();
    let reload_config_id = reload_config_item.id().clone();
    let quit_id = quit_item.id().clone();

    let menu = Menu::new();
    menu.append(&start_menu_item)?;
    menu.append(&config_app_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&autostart_item)?;
    menu.append(&config_rustbox_item)?;
    menu.append(&reload_config_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit_item)?;

    let tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Rust Box")
        .with_icon(icon)
        .build()?;

    // Channels for inter-thread communication
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<ChildCommand>();
    let (gui_tx, mut gui_rx) = mpsc::unbounded_channel::<bool>();
    let (dialog_tx, mut dialog_rx) = mpsc::unbounded_channel::<DialogCommand>();
    let tray_event_rx = TrayIconEvent::receiver();
    let menu_event_rx = MenuEvent::receiver();

    // Clones for the manager task
    let cmd_tx_clone = cmd_tx.clone();
    let app_path_clone = app_path.clone();
    let cfg_path_clone = cfg_path.clone();
    let app_name_clone = app_name.clone();

    // --- Manager task: controls the child process ---
    let manager_handle = tokio::spawn(async move {
        let mut child_pid: Option<u32> = None;
        let mut process_group: Option<ProcessGroup> = None;
        let mut child_handle: Option<tokio::process::Child> = None;
        let mut dialog_shown = false;

        loop {
            tokio::select! {
                Some(cmd) = cmd_rx.recv() => {
                    match cmd {
                        ChildCommand::Start => {
                            if child_pid.is_none() {
                                // Kill any leftover processes
                                eprintln!("Start: killing all {} processes", app_name_clone);
                                let _ = Command::new("taskkill")
                                    .args(["/F", "/IM", &app_name_clone])
                                    .output()
                                    .await;
                                tokio::time::sleep(Duration::from_secs(1)).await;

                                // PowerShell script to launch the child with elevation
                                let ps_script = format!(
                                    r#"$p = Start-Process -FilePath "{}" -ArgumentList "run -c {}" -Verb RunAs -WindowStyle Hidden -PassThru; if ($p) {{ $p.Id }}"#,
                                    app_path_clone, cfg_path_clone
                                );
                                eprintln!("PowerShell command: {}", ps_script);

                                // Create a process group to ensure termination
                                let group = match ProcessGroup::new() {
                                    Ok(g) => g,
                                    Err(e) => {
                                        eprintln!("Failed to create ProcessGroup: {}", e);
                                        continue;
                                    }
                                };

                                let mut cmd = Command::new("powershell");
                                cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps_script])
                                    .creation_flags(CREATE_NO_WINDOW)
                                    .stdout(std::process::Stdio::piped())
                                    .stderr(std::process::Stdio::piped());

                                let mut child = match group.spawn(&mut cmd) {
                                    Ok(c) => c,
                                    Err(e) => {
                                        eprintln!("Failed to spawn child in group: {}", e);
                                        continue;
                                    }
                                };

                                tokio::time::sleep(Duration::from_millis(500)).await;

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
                                    eprintln!("PowerShell stderr: {}", stderr);
                                }

                                let pid_str = stdout.trim();
                                eprintln!("PowerShell stdout: '{}'", pid_str);

                                if let Ok(pid) = pid_str.parse::<u32>() {
                                    // Verify the process is still running
                                    let sys = System::new_all();
                                    let still_running = sys.processes().values().any(|p| p.pid().as_u32() == pid);
                                    if !still_running {
                                        eprintln!("Child process {} died immediately", pid);
                                        continue;
                                    }

                                    child_pid = Some(pid);
                                    process_group = Some(group);
                                    child_handle = Some(child);
                                    is_running_task.store(true, Ordering::SeqCst);
                                    dialog_shown = false;
                                    eprintln!("Started child PID: {}", pid);
                                    let _ = gui_tx.send(true);
                                } else {
                                    eprintln!("Failed to parse PID from stdout: '{}'", pid_str);
                                }
                            }
                        }
                        ChildCommand::Stop => {
                            // Drop the process group – this kills all processes in the group
                            if let Some(group) = process_group.take() {
                                drop(group);
                                eprintln!("ProcessGroup dropped");
                            }
                            if let Some(mut child) = child_handle.take() {
                                let _ = child.kill().await;
                                let _ = child.wait().await;
                            }
                            // Additional cleanup by name
                            eprintln!("Stop: killing all {} processes", app_name_clone);
                            let _ = Command::new("taskkill")
                                .args(["/F", "/IM", &app_name_clone])
                                .output()
                                .await;
                            tokio::time::sleep(Duration::from_secs(1)).await;

                            // Wait for process to fully terminate
                            if let Some(pid) = child_pid {
                                for _ in 0..10 {
                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                    let sys = System::new_all();
                                    let still_running = sys.processes().values().any(|p| p.pid().as_u32() == pid);
                                    if !still_running { break; }
                                }
                            }
                            // Extra delay to release system resources
                            tokio::time::sleep(Duration::from_secs(2)).await;

                            child_pid = None;
                            is_running_task.store(false, Ordering::SeqCst);
                            dialog_shown = false;
                            eprintln!("Stop completed, all processes cleaned up");
                            let _ = gui_tx.send(false);
                        }
                    }
                }
                // Periodic monitoring: check if child process unexpectedly died
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if let Some(pid) = child_pid {
                        let sys = System::new_all();
                        let still_running = sys.processes().values().any(|p| p.pid().as_u32() == pid);
                        if !still_running {
                            eprintln!("Child {} terminated unexpectedly", pid);
                            child_pid = None;
                            process_group.take();
                            child_handle.take();
                            is_running_task.store(false, Ordering::SeqCst);
                            let _ = gui_tx.send(false);

                            // Show restart dialog only once per crash
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

    // --- GUI event loop (winit) ---
    let mut event_loop = EventLoop::new()?;
    let cmd_tx_main = cmd_tx.clone();
    let cmd_tx_main_clone = cmd_tx_main.clone();

    event_loop.run_on_demand(move |_event, window_target| {
        window_target.set_control_flow(ControlFlow::Wait);

        // Update Start/Stop button label from manager
        while let Ok(running) = gui_rx.try_recv() {
            let _ = start_menu_item.set_text(if running { "Stop" } else { "Start" });
        }

        // Process menu events
        while let Ok(menu_event) = menu_event_rx.try_recv() {
            let id = menu_event.id;

            if id == start_menu_id {
                let running = is_running.load(Ordering::SeqCst);
                let _ = if !running {
                    cmd_tx_main.send(ChildCommand::Start)
                } else {
                    cmd_tx_main.send(ChildCommand::Stop)
                };
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
                        eprintln!("Autostart set to {}", new_state);
                    }
                    Err(e) => {
                        eprintln!("Failed to change autostart: {}", e);
                        #[cfg(windows)]
                        unsafe {
                            let msg = format!("Failed to set autostart: {}", e);
                            let msg_utf16: Vec<u16> = msg.encode_utf16().chain(Some(0)).collect();
                            let title = "Rust Box - Error";
                            let title_utf16: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
                            MessageBoxW(null_mut(), msg_utf16.as_ptr(), title_utf16.as_ptr(), MB_OK | MB_ICONERROR);
                        }
                    }
                }
            } else if id == config_rustbox_id {
                // Open rust-box config in Notepad
                let _ = std::process::Command::new("notepad")
                    .arg(&config_path)
                    .spawn();
            } else if id == reload_config_id {
                // Reload rust-box
                // Stop child process if running
                let _ = cmd_tx_main.send(ChildCommand::Stop);
                // Give it time to terminate (2 seconds should be enough)
                std::thread::sleep(std::time::Duration::from_secs(2));

                // Restart the application
                let exe = std::env::current_exe().expect("failed to get exe path");
                let args: Vec<String> = std::env::args().collect();
                // Start new process, skipping the first argument (which is the exe path)
                let _ = std::process::Command::new(exe)
                    .args(&args[1..])
                    .spawn();
                // Exit current process
                window_target.exit(); // or std::process::exit(0)
            }else if id == config_app_id {
                // Open child app config in Notepad
                let _ = std::process::Command::new("notepad")
                    .arg(&cfg_path_for_gui)
                    .spawn();
            } else if id == quit_id {
                // Stop child and exit
                let _ = cmd_tx_main.send(ChildCommand::Stop);
                std::thread::sleep(Duration::from_millis(500));
                window_target.exit();
            }
        }

        // Handle dialog responses (restart or exit)
        while let Ok(dialog_cmd) = dialog_rx.try_recv() {
            match dialog_cmd {
                DialogCommand::Restart => {
                    let _ = cmd_tx_main_clone.send(ChildCommand::Start);
                }
                DialogCommand::Exit => {
                    let _ = cmd_tx_main_clone.send(ChildCommand::Stop);
                    std::thread::sleep(Duration::from_millis(500));
                    window_target.exit();
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

    // Final cleanup on normal exit
    let _ = cmd_tx.send(ChildCommand::Stop);
    tokio::time::sleep(Duration::from_millis(500)).await;
    manager_handle.abort();

    Ok(())
}