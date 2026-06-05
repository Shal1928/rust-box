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
        Self {
            app_path,
            cfg_path
        }
    }

    /// Build Config with path's to sing-box and cfg.json
    ///
    /// # Arguments
    ///
    /// * `args`: application launch arguments
    ///
    /// returns: Result<Config, &str>
    ///
    /// # Examples
    ///
    /// ```
    ///
    /// ```
    pub fn build(mut args: impl Iterator<Item = String>) -> Result<Config, &'static str> {
        //первое значение в возвращаемых данных env::args - это имя программы
        args.next();

        let rb_cfg_path = args.next().unwrap_or_else(|| "rust-box.cfg".to_string());

        Ok(Self::load_config_or_panic(&rb_cfg_path.as_str()))
    }


    fn load_config_or_panic(file_path: &str) -> Self {
        let file = File::open(file_path);
        match file {
            Ok(file) => {
                let cfg = Self::from_reader(&mut BufReader::new(file)).unwrap_or_else(|error| {
                    panic!("Parsing trouble: {error:?}");
                });
                cfg.check_required().expect("Required fields expects!");
                cfg
            }
            Err(error) => panic!("File open trouble: {error:?}"),
        }
    }

    fn from_reader<R: BufRead>(reader: R) -> Result<Self, ConfigError> {
        let mut app_path = None;
        let mut cfg_path = None;

        for line_result in reader.lines() {
            match line_result {
                Ok(line) => {
                    //Ошибку парсинга выбрасываем дальше, клиенту
                    let key_value = Config::parse_line(line.as_str())?;

                    //Если парсер вернул кортеж ключ-значение
                    if let Some((key, value)) = key_value {
                        match key.as_str() {
                            "app_path" => app_path = Some(value),
                            "cfg_path" => cfg_path = Some(value),
                            //Библиотечный код не должен ничего выводить в stdout — это нарушает принцип разделения ответственности.
                            //Логируем неизвестные ключи
                            _ => println!("Undefined configuration key {key} = {value}"),
                        }
                    }
                }
                Err(error) => {
                    return Err(ConfigError::IoError(error));
                }
            }
        }

        //Возвращаем конфигурацию
        Ok(Config::new(app_path, cfg_path))
    }

    fn parse_line(line: &str) -> Result<Option<(String, String)>, ConfigError> {
        //Игнорирует пустые строки и комментарии (начинающиеся с `#`), возвращая `Ok(None)`
        if line.is_empty() || line.starts_with('#') {
            return Ok(None);
        }

        //Возрат результата
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            if key.is_empty() {
                return Err(ConfigError::ParseError(format!(
                    "Строка: '{line}' содержит пустой ключ!"
                )));
            }

            let value = value.trim();
            if value.is_empty() {
                return Err(ConfigError::ParseError(format!(
                    "Строка: '{line}' содержит пустое значение для ключа: '{key}'!"
                )));
            }

            //Все хорошо
            Ok(Some((key.to_string(), value.to_string())))
        } else {
            Err(ConfigError::ParseError(format!(
                "Строка: '{line}' не содержит разделитель: '='!"
            )))
        }
    }

    fn parse_bool(value: &str) -> Result<bool, ConfigError> {
        let parsed_val = value.parse();

        match parsed_val {
            Ok(b) => Ok(b),
            Err(error) => Err(ConfigError::ParseError(format!(
                "Ошибка: {error} при парсинге в bool значения: {value}"
            ))),
        }
    }

    fn parse_u16(value: &str) -> Result<u16, ConfigError> {
        let parsed_val = value.parse();

        match parsed_val {
            Ok(b) => Ok(b),
            Err(error) => Err(ConfigError::ParseError(format!(
                "Ошибка: {error} при парсинге в u16 значения: {value}"
            ))),
        }
    }

    ///проверяет наличие критически важных полей
    pub fn check_required(&self) -> Result<(), ConfigError> {
        if self.app_path.is_none() {
            return Err(ConfigError::MissingField(
                "Field app_path is required!".to_string(),
            ));
        }

        if self.cfg_path.is_none() {
            return Err(ConfigError::MissingField(
                "Field cfg_path is required!".to_string(),
            ));
        }

        Ok(())
    }
}