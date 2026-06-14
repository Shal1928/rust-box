// main.rs - без Job Object, только taskkill
mod config;

use std::io::Cursor;
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use image::ImageReader;
use sysinfo::System;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tray_icon::{
    TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::platform::run_on_demand::EventLoopExtRunOnDemand;

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

    // Load icon
    let icon_bytes = include_bytes!("../rust-box.png");
    let img = ImageReader::new(Cursor::new(icon_bytes))
        .with_guessed_format()?
        .decode()?
        .into_rgba8();
    let (width, height) = img.dimensions();
    let icon = tray_icon::Icon::from_rgba(img.into_raw(), width, height)?;

    let is_running = Arc::new(AtomicBool::new(false));

    // Menu items
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
    let tray_event_rx = TrayIconEvent::receiver();
    let menu_event_rx = MenuEvent::receiver();

    let cmd_tx_clone = cmd_tx.clone();
    let app_path_clone = app_path;
    let cfg_path_clone = cfg_path;

    // Manager task – only taskkill, no job object
    let manager_handle = tokio::spawn(async move {
        let mut child_pid: Option<u32> = None;

        loop {
            tokio::select! {
                            Some(cmd) = cmd_rx.recv() => {
                                match cmd {
                                    ChildCommand::Start => {
                                        if child_pid.is_none() {
                                            let ps_script = format!(
                                                r#"$p = Start-Process -FilePath "{}" -ArgumentList 'run','-c','{}' -Verb RunAs -WindowStyle Hidden -PassThru; if ($p) {{ $p.Id }}"#,
                                                app_path_clone, cfg_path_clone
                                            );
                                            let mut child = match Command::new("powershell")
                                                .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps_script])
                                                .creation_flags(CREATE_NO_WINDOW)
                                                .stdout(std::process::Stdio::piped())
                                                .spawn()
                                            {
                                                Ok(c) => c,
                                                Err(e) => {
                                                    eprintln!("Failed to spawn: {}", e);
                                                    continue;
                                                }
                                            };

                                            let mut stdout = String::new();
                                            if let Some(mut out) = child.stdout.take() {
                                                let _ = out.read_to_string(&mut stdout).await;
                                            }
                                            let _ = child.wait().await;
                                            let pid_str = stdout.trim();
                                            if let Ok(pid) = pid_str.parse::<u32>() {
                                                child_pid = Some(pid);
                                                eprintln!("Started child PID: {}", pid);
                                            } else {
                                                eprintln!("Failed to parse PID from: '{}'", pid_str);
                                            }
                                        }
                                    }
                                    ChildCommand::Stop => {
                                        if let Some(pid) = child_pid.take() {
                                            // Сначала taskkill
                                            let _ = Command::new("taskkill")
                                                .args(["/F", "/T", "/PID", &pid.to_string()])
                                                .output()
                                                .await;

                                            // Подождать немного
                                            tokio::time::sleep(Duration::from_millis(200)).await;

                                            // Проверить, жив ли процесс
                                            let sys = System::new_all();
                                            let still_running = sys.processes()
                                                .values()
                                                .any(|p| p.pid().as_u32() == pid);

                                            if still_running {
                                                // Попробовать wmic
                                                let _ = Command::new("wmic")
                                                    .args(["process", "where", &format!("ProcessId={}", pid), "call", "terminate"])
                                                    .output()
                                                    .await;
                                            }
                                        }
                                    }
                                }
                            }
                            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                                if let Some(pid) = child_pid {
                                    let sys = System::new_all();
                                    let still_running = sys.processes()
                                        .values()
                                        .any(|p| p.pid().as_u32() == pid);
                                    if !still_running {
                                        eprintln!("Child {} terminated unexpectedly", pid);
                                        child_pid = None;
                                        let _ = cmd_tx_clone.send(ChildCommand::Stop);
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

        while let Ok(menu_event) = menu_event_rx.try_recv() {
            if menu_event.id == toggle_id {
                let running = is_running.load(Ordering::SeqCst);
                if !running {
                    let _ = cmd_tx_main.send(ChildCommand::Start);
                    is_running.store(true, Ordering::SeqCst);
                    let _ = toggle_item.set_text("Stop");
                } else {
                    let _ = cmd_tx_main.send(ChildCommand::Stop);
                    is_running.store(false, Ordering::SeqCst);
                    let _ = toggle_item.set_text("Start");
                }
            } else if menu_event.id == quit_id {
                let _ = cmd_tx_main.send(ChildCommand::Stop);
                std::thread::sleep(Duration::from_millis(300));
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
    tokio::time::sleep(Duration::from_millis(300)).await;
    manager_handle.abort();

    Ok(())
}
