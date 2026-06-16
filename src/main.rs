// main.rs
mod config;

use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::io::Cursor;
use std::time::Duration;
use std::ptr::null_mut;
use std::thread;

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

#[cfg(windows)]
use winapi::um::{
    handleapi::{CloseHandle, DuplicateHandle},
    jobapi2::{AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject},
    processthreadsapi::{GetCurrentProcess, OpenProcess, TerminateProcess},
    synchapi::WaitForSingleObject,
    winnt::{
        HANDLE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        PROCESS_TERMINATE, SYNCHRONIZE, DUPLICATE_SAME_ACCESS,
    },
};

use crate::config::Config;

const CREATE_NO_WINDOW: u32 = 0x08000000;
const INFINITE: u32 = 0xFFFFFFFF;

enum ChildCommand {
    Start,
    Stop,
}

#[cfg(windows)]
struct JobObject {
    handle: HANDLE,
}

#[cfg(windows)]
unsafe impl Send for JobObject {}

#[cfg(windows)]
impl JobObject {
    fn new() -> Option<Self> {
        unsafe {
            let handle = CreateJobObjectW(null_mut(), null_mut());
            if handle.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let result = SetInformationJobObject(
                handle,
                9,
                &mut info as *mut _ as _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if result == 0 {
                CloseHandle(handle);
                return None;
            }
            Some(JobObject { handle })
        }
    }

    fn assign_process(&self, pid: u32) -> bool {
        unsafe {
            let process_handle = OpenProcess(PROCESS_TERMINATE | SYNCHRONIZE, 0, pid);
            if process_handle.is_null() {
                return false;
            }
            let success = AssignProcessToJobObject(self.handle, process_handle) != 0;
            CloseHandle(process_handle);
            success
        }
    }
}

#[cfg(windows)]
impl Drop for JobObject {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle); }
    }
}

#[cfg(not(windows))]
struct JobObject;

#[cfg(not(windows))]
impl JobObject {
    fn new() -> Option<Self> { None }
    fn assign_process(&self, _pid: u32) -> bool { false }
}

#[cfg(not(windows))]
impl Drop for JobObject {
    fn drop(&mut self) {}
}

async fn cleanup_orphan_processes(app_path: &str) {
    let sys = System::new_all();
    let target_name = std::path::Path::new(app_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();

    for proc in sys.processes().values() {
        if let Some(exe) = proc.exe() {
            if let Some(name) = exe.file_name() {
                if name.to_string_lossy().to_lowercase() == target_name {
                    let pid = proc.pid().as_u32();
                    eprintln!("Cleanup: killing orphaned child PID {}", pid);
                    let _ = Command::new("taskkill")
                        .args(["/F", "/T", "/PID", &pid.to_string()])
                        .output()
                        .await;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
}

#[cfg(windows)]
fn spawn_watchdog(parent_handle: HANDLE, child_pid_flag: Arc<AtomicBool>, pid_value: u32) {
    let handle_value = parent_handle as usize;
    thread::spawn(move || {
        let handle = handle_value as HANDLE;
        unsafe {
            WaitForSingleObject(handle, INFINITE);
        }
        if child_pid_flag.load(Ordering::SeqCst) {
            eprintln!("Watchdog: main process died, killing child {}", pid_value);
            let _ = std::process::Command::new("taskkill")
                .args(["/F", "/T", "/PID", &pid_value.to_string()])
                .status();
        }
        unsafe { CloseHandle(handle); }
    });
}

#[cfg(not(windows))]
fn spawn_watchdog(_parent_handle: (), _child_pid_flag: Arc<AtomicBool>, _pid_value: u32) {}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::build(std::env::args()).unwrap_or_else(|err| {
        eprintln!("Config error: {err}");
        process::exit(1);
    });

    let app_path = config.app_path.as_ref().unwrap().clone();
    let cfg_path = config.cfg_path.as_ref().unwrap().clone();

    cleanup_orphan_processes(&app_path).await;

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
    let (gui_tx, mut gui_rx) = mpsc::unbounded_channel::<bool>();
    let tray_event_rx = TrayIconEvent::receiver();
    let menu_event_rx = MenuEvent::receiver();

    let cmd_tx_clone = cmd_tx.clone();
    let app_path_clone = app_path;
    let cfg_path_clone = cfg_path;

    let manager_handle = tokio::spawn(async move {
        let mut child_pid: Option<u32> = None;
        let mut _job_object: Option<JobObject> = None;
        let child_pid_flag = Arc::new(AtomicBool::new(false));

        loop {
            tokio::select! {
                Some(cmd) = cmd_rx.recv() => {
                    match cmd {
                        ChildCommand::Start => {
                            if child_pid.is_none() {
                                // Simple PowerShell script that returns PID of elevated process
                                let ps_script = format!(
                                    r#"$p = Start-Process -FilePath "{}" -ArgumentList "run","-c","{}" -Verb RunAs -WindowStyle Hidden -PassThru; if ($p) {{ $p.Id }}"#,
                                    app_path_clone, cfg_path_clone
                                );
                                let mut child = match Command::new("powershell")
                                    .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps_script])
                                    .creation_flags(CREATE_NO_WINDOW)
                                    .stdout(std::process::Stdio::piped())
                                    .stderr(std::process::Stdio::piped())
                                    .spawn()
                                {
                                    Ok(c) => c,
                                    Err(e) => {
                                        eprintln!("Failed to spawn: {}", e);
                                        continue;
                                    }
                                };

                                tokio::time::sleep(Duration::from_millis(300)).await;

                                let mut stdout = String::new();
                                let mut stderr = String::new();
                                if let Some(mut out) = child.stdout.take() {
                                    let _ = out.read_to_string(&mut stdout).await;
                                }
                                if let Some(mut err) = child.stderr.take() {
                                    let _ = err.read_to_string(&mut stderr).await;
                                }
                                let _ = child.wait().await;

                                eprintln!("PowerShell stdout: '{}'", stdout);
                                if !stderr.is_empty() {
                                    eprintln!("PowerShell stderr: '{}'", stderr);
                                }

                                let pid_str = stdout.trim();
                                if let Ok(pid) = pid_str.parse::<u32>() {
                                    if let Some(job) = JobObject::new() {
                                        if job.assign_process(pid) {
                                            _job_object = Some(job);
                                            eprintln!("Started child PID: {} with job object", pid);
                                        } else {
                                            eprintln!("Job object assign failed, using watchdog");
                                        }
                                    } else {
                                        eprintln!("Failed to create job object, using watchdog");
                                    }
                                    child_pid = Some(pid);
                                    child_pid_flag.store(true, Ordering::SeqCst);

                                    #[cfg(windows)]
                                    {
                                        let current_handle = unsafe { GetCurrentProcess() };
                                        let mut dup_handle = null_mut();
                                        unsafe {
                                            DuplicateHandle(
                                                current_handle,
                                                current_handle,
                                                current_handle,
                                                &mut dup_handle,
                                                0,
                                                0,
                                                DUPLICATE_SAME_ACCESS,
                                            );
                                        }
                                        if !dup_handle.is_null() {
                                            let flag_clone = child_pid_flag.clone();
                                            spawn_watchdog(dup_handle, flag_clone, pid);
                                        }
                                    }

                                    let _ = gui_tx.send(true);
                                } else {
                                    eprintln!("Failed to parse PID from: '{}'", pid_str);
                                }
                            }
                        }
                        ChildCommand::Stop => {
                            if let Some(pid) = child_pid.take() {
                                child_pid_flag.store(false, Ordering::SeqCst);
                                // Kill entire process tree
                                let _ = Command::new("taskkill")
                                    .args(["/F", "/T", "/PID", &pid.to_string()])
                                    .output()
                                    .await;
                                // Ensure termination via WinAPI
                                #[cfg(windows)]
                                unsafe {
                                    let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
                                    if !handle.is_null() {
                                        TerminateProcess(handle, 1);
                                        CloseHandle(handle);
                                    }
                                }
                                // Wait for process to actually terminate
                                for _ in 0..10 {
                                    tokio::time::sleep(Duration::from_millis(100)).await;
                                    let sys = System::new_all();
                                    let still_running = sys.processes().values().any(|p| p.pid().as_u32() == pid);
                                    if !still_running {
                                        break;
                                    }
                                }
                                eprintln!("Killed child PID: {}", pid);
                                _job_object = None;
                                let _ = gui_tx.send(false);
                            }
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(500)) => {
                    if let Some(pid) = child_pid {
                        let sys = System::new_all();
                        let still_running = sys.processes().values().any(|p| p.pid().as_u32() == pid);
                        if !still_running {
                            eprintln!("Child {} terminated unexpectedly", pid);
                            child_pid = None;
                            _job_object = None;
                            child_pid_flag.store(false, Ordering::SeqCst);
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
            is_running.store(running, Ordering::SeqCst);
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