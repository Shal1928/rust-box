use std::fs::File;
use std::io::{BufRead, BufReader, Error as IoError};

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

    pub fn build(mut args: impl Iterator<Item = String>) -> Result<Config, &'static str> {
        args.next();
        let rb_cfg_path = args.next().unwrap_or_else(|| "rust-box.cfg".to_string());
        Ok(Self::load_config_or_panic(&rb_cfg_path))
    }

    fn load_config_or_panic(file_path: &str) -> Self {
        let file = File::open(file_path).unwrap_or_else(|error| {
            panic!("File open trouble: {error:?}");
        });

        let cfg = Self::from_reader(&mut BufReader::new(file)).unwrap_or_else(|error| {
            panic!("Parsing trouble: {error:?}");
        });

        cfg.check_required().expect("Required fields expects!");
        cfg
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
}