
use tokio::io::AsyncReadExt;
use tray_icon::{TrayIconBuilder, menu::Menu, Icon};

// fn main() {
//     let icon = Icon::from_rgba(vec![255; 64 * 64 * 4], 64, 64).expect("Failed to create icon");
//
//
//     let tray_menu = Menu::new();
//     let tray_icon = TrayIconBuilder::new()
//         .with_menu(Box::new(tray_menu))
//         .with_tooltip("system-tray - tray icon library!")
//         .with_icon(icon)
//         .build()
//         .unwrap();
//
//     loop {
//         std::thread::sleep(std::time::Duration::from_millis(100));
//     }
// }

mod config;
mod build;

use std::{env, process};
use sysinfo::{Pid, System};
use std::io::Error;
use tokio::process::Command;
use tokio::io::{AsyncWriteExt, AsyncBufReadExt, BufReader};
use std::process::Stdio;
use std::time::Duration;
use processkit::ProcessGroup;
use tokio::time::sleep;
use crate::config::Config;

// Константа для скрытия окна
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {

    let config = Config::build(env::args()).unwrap_or_else(|err| {
        eprintln!("Problem parsing arguments: {err}");
        process::exit(1);
    });

    let app_path = &config.app_path.unwrap();

    // PowerShell скрипт для запуска от администратора без окна
    let ps_script = format!(
        r#"$p = Start-Process -FilePath "{}" -ArgumentList "run","-c","{}" -Verb RunAs -WindowStyle Hidden -PassThru -Wait; if ($p) {{ $p.Id }} else {{ exit 1 }}"#,
        &app_path,
        &config.cfg_path.clone().unwrap()
    );

    // 1. Сначала запускаем процесс стандартным способом, со всеми нужными флагами
    let mut child = tokio::process::Command::new("powershell")
        .args(&["-Command", &ps_script])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW) // ваш флаг остаётся
        .spawn()?;

    // 2. Создаём группу и усыновляем уже запущенный процесс
    let group = ProcessGroup::new()?;
    group.adopt(&mut child)?;

    // Читаем PID
    let mut stdout = String::new();
    if let Some(mut out) = child.stdout.take() {
        out.read_to_string(&mut stdout).await?;
    }
    let mut stderr = String::new();
    if let Some(mut err) = child.stderr.take() {
        err.read_to_string(&mut stderr).await?;
    }

    println!("stdout: '{}'", stdout);
    println!("stderr: '{}'", stderr);

    let pid_str = stdout.trim();
    if pid_str.is_empty() {
        eprintln!("❌ Can't get PID!");
        process::exit(1);
    }

    let pid = pid_str.parse::<u32>().map_err(|e| {
        format!("Parse error PID '{}': {}", pid_str, e)
    }).unwrap_or_else(|err| {
        eprintln!("❌ {}", err);
        process::exit(1);
    });

    println!("{}", &pid);

    // В цикле
    loop {
        let status = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .output()
            .await?;
        let stdout = String::from_utf8_lossy(&status.stdout);
        if !stdout.contains(&pid.to_string()) {
            break; // процесс завершился
        }
        sleep(Duration::from_secs(1)).await;
    }

    Ok(())
}
