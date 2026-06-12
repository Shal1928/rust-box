// main.rs
mod config;

use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::io::Cursor;
use std::time::Duration;

use tray_icon::{
    menu::{Menu, MenuItem, PredefinedMenuItem, MenuEvent},
    TrayIconBuilder, TrayIconEvent,
};
use winit::event_loop::{EventLoop, ControlFlow, EventLoopWindowTarget};
use winit::platform::run_on_demand::EventLoopExtRunOnDemand;
use image::ImageReader;
use sysinfo::System;
use tokio::process::Command;
use tokio::sync::mpsc;

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

    let icon_bytes = include_bytes!("../rust-box.png");
    let img = ImageReader::new(Cursor::new(icon_bytes))
        .with_guessed_format()?
        .decode()?
        .into_rgba8();
    let (width, height) = img.dimensions();
    let icon = tray_icon::Icon::from_rgba(img.into_raw(), width, height)?;

    let is_running = Arc::new(AtomicBool::new(false));

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

    let manager_handle = tokio::spawn(async move {
        let mut child_handle: Option<tokio::process::Child> = None;

        loop {
            tokio::select! {
                Some(cmd) = cmd_rx.recv() => {
                    match cmd {
                        ChildCommand::Start => {
                            if child_handle.is_none() {
                                let ps_script = format!(
                                    r#"Start-Process -FilePath "{}" -ArgumentList "run","-c","{}" -Verb RunAs -WindowStyle Hidden -Wait"#,
                                    app_path_clone, cfg_path_clone
                                );
                                if let Ok(child) = Command::new("powershell")
                                    .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps_script])
                                    .creation_flags(CREATE_NO_WINDOW)
                                    .spawn()
                                {
                                    child_handle = Some(child);
                                }
                            }
                        }
                        ChildCommand::Stop => {
                            if let Some(mut child) = child_handle.take() {
                                let _ = child.kill().await;
                                let _ = child.wait().await;
                            }
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(500)) => {
                    if let Some(child) = &child_handle {
                        let sys = System::new_all();
                        let still_running = sys.processes()
                            .values()
                            .any(|p| p.pid().as_u32() == child.id().unwrap_or(0));
                        if !still_running {
                            child_handle = None;
                            let _ = cmd_tx_clone.send(ChildCommand::Stop);
                        }
                    }
                }
            }
        }
    });

    let mut event_loop = EventLoop::new()?;

    // Correct closure signature: (Event<()>, &EventLoopWindowTarget<()>)
    event_loop.run_on_demand(move |_event, window_target| {
        // Set control flow via window_target
        window_target.set_control_flow(ControlFlow::Wait);

        while let Ok(menu_event) = menu_event_rx.try_recv() {
            if menu_event.id == toggle_id {
                let current = is_running.load(Ordering::SeqCst);
                if !current {
                    let _ = cmd_tx.send(ChildCommand::Start);
                    is_running.store(true, Ordering::SeqCst);
                    let _ = toggle_item.set_text("Stop");
                } else {
                    let _ = cmd_tx.send(ChildCommand::Stop);
                    is_running.store(false, Ordering::SeqCst);
                    let _ = toggle_item.set_text("Start");
                }
            } else if menu_event.id == quit_id {
                let _ = cmd_tx.send(ChildCommand::Stop);
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

    manager_handle.abort();
    Ok(())
}