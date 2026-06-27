use std::fs::File;
use std::io::{BufRead, BufReader, Write, Error as IoError};
use std::env;
use base64::decode;

const DEFAULT_CONFIG_NAME: &str = "rust-box.cfg";

const DEFAULT_APP_PATH: &str = "c2luZy1ib3g=";
const DEFAULT_CFG_PATH: &str = "Y29uZmlnLmpzb24=";

fn decode_b64(s: &str) -> Result<String, String> {
    let bytes = decode(s).map_err(|e| format!("Base64 decode error: {}", e))?;
    String::from_utf8(bytes).map_err(|e| format!("UTF-8 decode error: {}", e))
}

struct DefaultConfig {
    app_path: String,
    cfg_path: String,
}

impl DefaultConfig {
    fn new() -> Result<Self, String> {
        Ok(Self {
            app_path: decode_b64(DEFAULT_APP_PATH)?,
            cfg_path: decode_b64(DEFAULT_CFG_PATH)?,
        })
    }

    fn to_string(&self) -> String {
        format!("app_path={}\ncfg_path={}\n", self.app_path, self.cfg_path)
    }
}

pub struct Config {
    pub app_path: Option<String>,
    pub cfg_path: Option<String>
}

#[derive(Debug)]
pub enum ConfigError {
    IoError(IoError),
    ParseError(String),
    MissingField(String),
}

impl Config {
    fn new(app_path: Option<String>, cfg_path: Option<String>) -> Self {
        Self { app_path, cfg_path }
    }

    pub fn load_from_path(path: &str) -> Result<Self, ConfigError> {
        Self::load_config(path)
    }

    pub fn load_or_create_default() -> Result<Self, ConfigError> {
        let config_path = Self::ensure_config_file().map_err(|e| ConfigError::IoError(IoError::new(std::io::ErrorKind::Other, e)))?;
        Self::load_from_path(&config_path.to_string_lossy())
    }

    pub fn ensure_config_file() -> Result<std::path::PathBuf, String> {
        let exe_path = env::current_exe().map_err(|e| format!("current_exe: {}", e))?;
        let exe_dir = exe_path.parent().ok_or("No exe dir")?;
        let config_path = exe_dir.join(DEFAULT_CONFIG_NAME);
        if !config_path.exists() {
            let default = DefaultConfig::new()?;
            let mut file = File::create(&config_path).map_err(|e| format!("create file: {}", e))?;
            file.write_all(default.to_string().as_bytes()).map_err(|e| format!("write file: {}", e))?;
            eprintln!("✅ Created default config file: {}", config_path.display());
        }
        Ok(config_path)
    }

    fn load_config(file_path: &str) -> Result<Self, ConfigError> {
        let file = File::open(file_path).map_err(ConfigError::IoError)?;
        let cfg = Self::from_reader(&mut BufReader::new(file))?;
        cfg.check_required()?;
        Ok(cfg)
    }

    fn from_reader<R: BufRead>(reader: R) -> Result<Self, ConfigError> {
        let mut app_path = None;
        let mut cfg_path = None;

        for line_result in reader.lines() {
            let line = line_result.map_err(ConfigError::IoError)?;

            if let Some((key, value)) = Self::parse_line(&line)? {
                match key.as_str() {
                    "app_path" => app_path = Some(value),
                    "cfg_path" => cfg_path = Some(value),
                    _ => eprintln!("Unknown config key: {key} = {value}"),
                }
            }
        }

        Ok(Config::new(app_path, cfg_path))
    }

    fn parse_line(line: &str) -> Result<Option<(String, String)>, ConfigError> {
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') {
            return Ok(None);
        }

        let (key, value) = line.split_once('=').ok_or_else(|| {
            ConfigError::ParseError(format!("Line '{line}' missing separator '='"))
        })?;

        let key = key.trim();
        let value = value.trim();

        if key.is_empty() {
            return Err(ConfigError::ParseError("Empty key".to_string()));
        }

        if value.is_empty() {
            return Err(ConfigError::ParseError(format!("Empty value for key '{key}'")));
        }

        Ok(Some((key.to_string(), value.to_string())))
    }

    pub fn check_required(&self) -> Result<(), ConfigError> {
        if self.app_path.is_none() {
            return Err(ConfigError::MissingField("app_path is required".to_string()));
        }
        if self.cfg_path.is_none() {
            return Err(ConfigError::MissingField("cfg_path is required".to_string()));
        }
        Ok(())
    }

    pub fn update_cfg_path(new_path: &str) -> Result<(), Box<dyn std::error::Error>> {
        let exe_path = env::current_exe()?;
        let exe_dir = exe_path.parent().ok_or("No exe dir")?;
        let config_path = exe_dir.join(DEFAULT_CONFIG_NAME);
        let content = std::fs::read_to_string(&config_path)?;
        let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
        let mut found = false;
        for line in &mut lines {
            if line.starts_with("cfg_path=") {
                *line = format!("cfg_path={}", new_path);
                found = true;
                break;
            }
        }
        if !found {
            lines.push(format!("cfg_path={}", new_path));
        }
        std::fs::write(&config_path, lines.join("\n"))?;
        Ok(())
    }
}