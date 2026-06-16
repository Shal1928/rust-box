mod config;

use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::io::Cursor;
use std::time::Duration;
use std::path::Path;

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

use crate::config::Config;

const CREATE_NO_WINDOW: u32 = 0x08000000;

enum ChildCommand {
    Start,
    Stop,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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

    // Cleanup orphaned processes on startup
    eprintln!("Startup: killing any existing {} processes", app_name);
    let _ = Command::new("taskkill")
        .args(["/F", "/IM", &app_name])
        .output()
        .await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Load icon
    let icon_bytes = include_bytes!("../rust-box.png");
    let img = ImageReader::new(Cursor::new(icon_bytes))
        .with_guessed_format()?
        .decode()?
        .into_rgba8();
    let (width, height) = img.dimensions();
    let icon = tray_icon::Icon::from_rgba(img.into_raw(), width, height)?;

    let is_running = Arc::new(AtomicBool::new(false));
    let is_running_task = is_running.clone();

    let toggle_item = MenuItem::new("Start", true, None);
    let quit_item = MenuItem::new("Exit", true, None);
    let toggle_id = toggle_item.id().clone();
    let quit_id = quit_item.id().clone();

    let menu = Menu::new();
    menu.append(&toggle_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit_item)?;

    let tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Rust Box")
        .with_icon(icon)
        .build()?;

    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<ChildCommand>();
    let (gui_tx, mut gui_rx) = mpsc::unbounded_channel::<bool>();
    let tray_event_rx = TrayIconEvent::receiver();
    let menu_event_rx = MenuEvent::receiver();

    let cmd_tx_clone = cmd_tx.clone();
    let app_path_clone = app_path.clone();
    let cfg_path_clone = cfg_path.clone();
    let app_name_clone = app_name.clone();

    let manager_handle = tokio::spawn(async move {
        let mut child_pid: Option<u32> = None;
        let mut process_group: Option<ProcessGroup> = None;
        let mut child_handle: Option<tokio::process::Child> = None;

        loop {
            tokio::select! {
                Some(cmd) = cmd_rx.recv() => {
                    match cmd {
                        ChildCommand::Start => {
                            if child_pid.is_none() {
                                // Kill all existing processes with same name
                                eprintln!("Start: killing all {} processes", app_name_clone);
                                let _ = Command::new("taskkill")
                                    .args(["/F", "/IM", &app_name_clone])
                                    .output()
                                    .await;
                                tokio::time::sleep(Duration::from_secs(2)).await;

                                // PowerShell script (no redirection to avoid conflicts)
                                let ps_script = format!(
                                    r#"$p = Start-Process -FilePath "{}" -ArgumentList "run -c {}" -Verb RunAs -WindowStyle Hidden -PassThru; if ($p) {{ $p.Id }}"#,
                                    app_path_clone, cfg_path_clone
                                );
                                eprintln!("PowerShell command: {}", ps_script);

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
                                    // Check if process is still running
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
                                    eprintln!("Started child PID: {}", pid);
                                    let _ = gui_tx.send(true);
                                } else {
                                    eprintln!("Failed to parse PID from stdout: '{}'", pid_str);
                                }
                            }
                        }
                        ChildCommand::Stop => {
                            // Kill via ProcessGroup
                            if let Some(group) = process_group.take() {
                                drop(group);
                                eprintln!("ProcessGroup dropped");
                            }
                            if let Some(mut child) = child_handle.take() {
                                let _ = child.kill().await;
                                let _ = child.wait().await;
                            }
                            // Additional cleanup: kill by name
                            eprintln!("Stop: killing all {} processes", app_name_clone);
                            let _ = Command::new("taskkill")
                                .args(["/F", "/IM", &app_name_clone])
                                .output()
                                .await;
                            tokio::time::sleep(Duration::from_secs(2)).await;

                            // Wait for process to fully terminate
                            if let Some(pid) = child_pid {
                                for _ in 0..20 {
                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                    let sys = System::new_all();
                                    let still_running = sys.processes().values().any(|p| p.pid().as_u32() == pid);
                                    if !still_running {
                                        break;
                                    }
                                }
                            }

                            // Extra delay to ensure OS releases all resources (TUN interface)
                            tokio::time::sleep(Duration::from_secs(3)).await;

                            child_pid = None;
                            is_running_task.store(false, Ordering::SeqCst);
                            eprintln!("Stop completed, all processes cleaned up");
                            let _ = gui_tx.send(false);
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if let Some(pid) = child_pid {
                        let sys = System::new_all();
                        let still_running = sys.processes().values().any(|p| p.pid().as_u32() == pid);
                        if !still_running {
                            eprintln!("Child {} terminated unexpectedly", pid);
                            process_group.take();
                            child_handle.take();
                            child_pid = None;
                            is_running_task.store(false, Ordering::SeqCst);
                            let _ = gui_tx.send(false);
                        }
                    }
                }
            }
        }
    });

    let mut event_loop = EventLoop::new()?;
    let cmd_tx_main = cmd_tx.clone();

    event_loop.run_on_demand(move |_event, window_target| {
        window_target.set_control_flow(ControlFlow::Wait);

        while let Ok(running) = gui_rx.try_recv() {
            let _ = toggle_item.set_text(if running { "Stop" } else { "Start" });
        }

        while let Ok(menu_event) = menu_event_rx.try_recv() {
            if menu_event.id == toggle_id {
                let running = is_running.load(Ordering::SeqCst);
                if !running {
                    let _ = cmd_tx_main.send(ChildCommand::Start);
                } else {
                    let _ = cmd_tx_main.send(ChildCommand::Stop);
                }
            } else if menu_event.id == quit_id {
                let _ = cmd_tx_main.send(ChildCommand::Stop);
                std::thread::sleep(Duration::from_millis(500));
                window_target.exit();
            }
        }

        while let Ok(tray_event) = tray_event_rx.try_recv() {
            if let TrayIconEvent::Click { button, .. } = tray_event {
                if button == tray_icon::MouseButton::Left {
                    let _ = tray_icon.show_menu();
                }
            }
        }
    })?;

    let _ = cmd_tx.send(ChildCommand::Stop);
    tokio::time::sleep(Duration::from_millis(500)).await;
    manager_handle.abort();

    Ok(())
}