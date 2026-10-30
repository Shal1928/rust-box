#![cfg(windows)]
//! Windows-only utilities for installing applications via package managers.
//!
//! This module provides [`InstallMgr`], which run installation app, check app in system.

use winapi::um::winbase::CREATE_NO_WINDOW;
use std::path::Path;
use std::{env, process};
use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;
use tokio::process::Command;
use crate::logger::log_event;

pub struct InstallMgr {
    cache: Mutex<HashMap<String, Option<String>>>,
}

impl Default for InstallMgr {
    fn default() -> Self { Self::new() }
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("no package manager available")]
    NoManager,
    #[error("{0} install failed (exit code {1})")]
    ManagerFailed(&'static str, i32),
    #[error("binary not found after {0} install")]
    BinaryNotFound(&'static str),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("all available package managers failed (last exit code {0})")]
    AllFailed(i32),
}

#[derive(Debug, Clone, Copy)]
enum Mgr {
    Winget,
    Choco,
    Scoop
}

impl Mgr {
    const ALL: [Self; 3] = [
        Self::Winget,
        Self::Choco,
        Self::Scoop,
    ];

    fn name(&self) -> &'static str {
        match self {
            Mgr::Winget => "winget",
            Mgr::Choco  => "choco",
            Mgr::Scoop  => "scoop",
        }
    }

    fn install(&self, app_name: &str) -> Command {
        let mut cmd = Command::new(self.name());
        match self {
            Mgr::Winget => {
                cmd.args([
                    "install", "--id", app_name, "-e", "--silent",
                    "--source", "winget",
                    "--accept-source-agreements",
                    "--accept-package-agreements",
                    "--disable-interactivity",
                ]);
            }
            Mgr::Choco => {
                cmd.args(["install", app_name, "-y", "--accept-license"]);
            }
            Mgr::Scoop => {
                cmd.args(["install", app_name]);
            }
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW);
        cmd
    }

    fn is_available(&self) -> bool {
        process::Command::new(self.name())
            .creation_flags(CREATE_NO_WINDOW)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

impl InstallMgr {

    /// Creates a new installer with an empty binary-resolution cache.
    pub fn new() -> Self {
        Self { cache: Mutex::new(HashMap::new()) }
    }

    /// Installs an application using the first available package manager.
    ///
    /// # Returns
    /// On success, returns the resolved absolute path to the installed binary.
    ///
    /// # Errors
    /// - [`InstallError::NoManager`] - none of winget/choco/scoop is available.
    /// - [`InstallError::ManagerFailed`] - the manager returned a non-zero last exit code.
    /// - [`InstallError::AllFailed`] - all managers failed returned a non-zero last exit code.
    /// - [`InstallError::BinaryNotFound`] - the manager reported success, but the
    ///   binary could not be located by name. This can happen when the package
    ///   name differs from the executable name (e.g. `vscode` vs `Code.exe`)
    ///   or when the installation requires a reboot to finalize.
    ///
    /// The full stdout/stderr of the package manager is written to `app.log`
    /// via [`log_event`] to aid diagnosis.
    pub async fn install_app(&self, app_name: &str) -> Result<String, InstallError> {
        log_event(&format!("=== Install started for app: {} ===", app_name));

        let available: Vec<Mgr> = Mgr::ALL
            .iter()
            .copied()
            .filter(|m| m.is_available())
            .collect();

        log_event(&format!(
            "Available managers: {:?}",
            available.iter().map(|m| m.name()).collect::<Vec<_>>()
        ));

        if available.is_empty() {
            log_event("No package manager available.");
            return Err(InstallError::NoManager);
        }

        let mut last_code = -1;
        for mgr in available {
            log_event(&format!("Attempting install via {}", mgr.name()));
            let child = match mgr.install(app_name).kill_on_drop(true).spawn() {
                Ok(c) => c,
                Err(e) => {
                    log_event(&format!("Failed to spawn {}: {}", mgr.name(), e));
                    continue;
                }
            };

            log_event(&format!("{} spawned, waiting for completion...", mgr.name()));

            let output =
                match tokio::time::timeout(Duration::from_secs(60), child.wait_with_output()).await {
                    Ok(Ok(out)) => out,
                    Ok(Err(e)) => {
                        log_event(&format!("{} wait error: {}", mgr.name(), e));
                        continue;
                    }
                    Err(_) => {
                        log_event(&format!("{} timed out after 60 seconds, skipping to next method.", mgr.name()));
                        continue;
                    }
                };

            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            log_event(&format!("=== {} stdout ===\n{}", mgr.name(), stdout));
            log_event(&format!("=== {} stderr ===\n{}", mgr.name(), stderr));
            last_code = output.status.code().unwrap_or(-1);
            log_event(&format!("Exit code: {}", last_code));

            if output.status.success() {
                log_event(&format!("{} succeeded, searching for binary...", mgr.name()));
                return
                    if let Some(path) = self.find_app_binary(app_name) {
                        log_event(&format!("Found binary at: {}", path));
                        Ok(path)
                    } else {
                        log_event(&format!("Binary not found after {} install, aborting.", mgr.name()));
                        Err(InstallError::BinaryNotFound(mgr.name()))
                    }
            } else {
                log_event(&format!("{} failed with non-zero exit code.", mgr.name()));
            }
        }

        log_event("All available managers failed.");
        Err(InstallError::AllFailed(last_code))
    }

    fn resolve_app_path(&self, app_name: &str) -> Option<String> {
        if app_name.is_empty() {
            return None;
        }

        let is_exe = app_name.len() >= 4
            && app_name.as_bytes()[app_name.len() - 4..].eq_ignore_ascii_case(b".exe");

        if (app_name.contains('\\') || app_name.contains('/')) && is_exe && Path::new(app_name).exists() {
            return Some(app_name.to_string());
        }

        if let Some(path) = self.find_app_by_name(app_name) {
            return Some(path);
        }

        self.find_app_binary(app_name)
    }

    fn find_app_by_name_uncached(app_name: &str) -> Option<String> {
        let ps_command = format!(
            "Get-Command {} -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source",
            app_name
        );
        let output = process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &ps_command])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        if output.status.success() {
            let path = String::from_utf8(output.stdout).ok()?;
            let path = path.trim();
            if !path.is_empty() && Path::new(path).exists() {
                return Some(path.to_string());
            }
        }

        None
    }

    fn find_app_by_name(&self, app_name: &str) -> Option<String> {
        let mut cache = self.cache.lock().unwrap();
        if let Some(hit) = cache.get(app_name) {
            return hit.clone();
        }
        let result = Self::find_app_by_name_uncached(app_name);
        cache.insert(app_name.to_string(), result.clone());
        result
    }

    fn find_app_binary(&self, app_name: &str) -> Option<String> {
        if let Some(path) = self.find_app_by_name(app_name) {
            return Some(path);
        }

        let choco_shim = format!(r"C:\ProgramData\chocolatey\bin\{}.exe", app_name);
        if Path::new(&choco_shim).exists() {
            return Some(choco_shim);
        }

        let choco_base = format!(r"C:\ProgramData\chocolatey\lib\{}", app_name);
        if let Some(exe) = self.find_exe_in_mng_base(app_name, choco_base) {
            return Some(exe);
        }

        if let Ok(local_app_data) = env::var("LOCALAPPDATA") {
            let winget_base = format!(r"{}\Microsoft\WinGet\Packages", local_app_data);
            if let Some(exe) = self.find_exe_in_mng_base(app_name, winget_base) {
                return Some(exe);
            }
        }

        if let Ok(user) = env::var("USERNAME") {
            let scoop_path = format!(
                r"C:\Users\{}\scoop\apps\{}\current\{}.exe",
                user, app_name, app_name
            );
            if Path::new(&scoop_path).exists() {
                return Some(scoop_path);
            }
        }

        let common_paths = [
            format!(r"C:\Program Files\{}\{}.exe", app_name, app_name),
            format!(r"C:\Program Files (x86)\{}\{}.exe", app_name, app_name),
        ];
        for path in common_paths.iter() {
            if Path::new(path).exists() {
                return Some(path.clone());
            }
        }

        None
    }

    fn find_exe_in_mng_base(&self, app_name: &str, mng_base: String) -> Option<String> {
        if let Ok(entries) = std::fs::read_dir(mng_base) {
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(exe) = self.find_exe_recursive(&path, app_name, 0) {
                        return Some(exe);
                    }
                }
            }
        }

        None
    }

    fn find_exe_recursive(&self, dir: &Path, app_name: &str, depth: u32) -> Option<String> {
        if depth > 8 {
            return None;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(exe) = self.find_exe_recursive(&path, app_name, depth + 1) {
                        return Some(exe);
                    }
                } else if path.extension().is_some_and(|e| e == "exe") {
                    if let Some(stem) = path.file_stem() {
                        if stem.to_string_lossy().eq_ignore_ascii_case(app_name) {
                            return Some(path.to_string_lossy().into_owned());
                        }
                    }
                }
            }
        }
        None
    }
}