mod config;
mod build;

use tray_icon::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    MouseButton, TrayIconBuilder, TrayIconEvent};
use tokio::io::AsyncReadExt;
use std::{env, process, thread};
use std::io::Cursor;
use tokio::process::Command;
use tokio::io::{AsyncWriteExt, AsyncBufReadExt};
use std::time::Duration;
use image::ImageReader;
use processkit::ProcessGroup;
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

    // include_bytes! встраивает файл иконки прямо в бинарник.
    // Путь указан относительно корня вашего крейта (там, где лежит Cargo.toml).
    let png_bytes : &'static [u8] = include_bytes!("../rust-box.png");

    // Декодируем PNG в RGBA
    let img = ImageReader::new(Cursor::new(png_bytes))
        .with_guessed_format()
        .expect("Не удалось определить формат")
        .decode()
        .expect("Не удалось декодировать PNG")
        .into_rgba8();  // конвертируем в RGBA8

    let (width, height) = img.dimensions();
    let rgba_data = img.into_raw();

    // Создаём иконку
    let icon = tray_icon::Icon::from_rgba(rgba_data, width, height)
        .expect("Не удалось создать иконку из RGBA-данных");

    // Состояние переключателя
    let mut is_on = true;

    let toggle_item = MenuItem::new("Off", true, None);
    let quit_item = MenuItem::new("Exit", true, None);

    let tray_menu = Menu::new();
    tray_menu.append(&toggle_item).unwrap();
    tray_menu.append(&PredefinedMenuItem::separator()).unwrap();
    tray_menu.append(&quit_item).unwrap();

    // Запоминаем id пунктов (они клонируются, т.к. MenuId внутри Rc, но сам id можно сравнить)
    let toggle_id = toggle_item.id().clone();
    let quit_id = quit_item.id().clone();

    let tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_tooltip(format!("rust-box for {}", &app_path))
        .with_icon(icon)
        .build()
        .unwrap();

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

    let event_receiver = TrayIconEvent::receiver();

    // В цикле
    loop {
        // Проверяем, нет ли новых событий от иконки в трее.
        while let Ok(event) = event_receiver.try_recv() {
            println!("menu event");
            match event {
                // Обрабатываем обычный клик.
                TrayIconEvent::Click { button, .. } => {
                    if let MouseButton::Left = button {
                        tray_icon.show_menu(); // <- просто вызываем, без if let Err
                    }
                }
                // Обрабатываем двойной клик, если нужно.
                TrayIconEvent::DoubleClick { button: MouseButton::Left, .. } => {
                    println!("Двойной клик левой кнопкой");
                    // Например: сразу запустить/остановить VPN.
                }
                // Игнорируем другие события.
                _ => {}
            }
        }

        let status = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .output()
            .await?;
        let stdout = String::from_utf8_lossy(&status.stdout);
        if !stdout.contains(&pid.to_string()) {
            println!("{}", &stdout);
            break; // процесс завершился
        }

        thread::sleep(Duration::from_millis(50));
    }

    Ok(())
}
