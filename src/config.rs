//! config.toml next to the executable, merged over built-in defaults. Settings changed in the app are
//! written back line by line so comments and layout survive.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Result;
use regex::Regex;
use toml::{Table, Value};

pub const DEFAULT_CONFIG: &str = include_str!("default_config.toml");

/// Folder holding config.toml and logs/: the executable's folder, or `SPACEWIZ_HOME` if set.
pub fn root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        if let Some(home) = std::env::var_os("SPACEWIZ_HOME") {
            return PathBuf::from(home);
        }
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."))
    })
}

fn config_path() -> PathBuf {
    root().join("config.toml")
}

#[derive(Clone)]
pub struct Config {
    table: Table,
}

impl Config {
    pub fn load() -> Config {
        let mut table: Table = DEFAULT_CONFIG.parse().expect("built-in default config is valid TOML");
        let path = config_path();
        if !path.exists() {
            if let Err(e) = std::fs::write(&path, DEFAULT_CONFIG) {
                log::warn!("Could not create {}: {e}", path.display());
            }
        }
        match std::fs::read_to_string(&path).map(|s| s.parse::<Table>()) {
            Ok(Ok(user)) => {
                for (section, values) in user {
                    match (table.get_mut(&section), values) {
                        (Some(Value::Table(base)), Value::Table(values)) => base.extend(values),
                        (_, values) => {
                            table.insert(section, values);
                        }
                    }
                }
            }
            Ok(Err(e)) => log::error!("config.toml is not valid TOML, using defaults: {e}"),
            Err(e) => log::warn!("Could not read config.toml: {e}"),
        }
        Config { table }
    }

    pub fn get(&self, section: &str, key: &str) -> Option<&Value> {
        self.table.get(section)?.get(key)
    }

    pub fn str(&self, section: &str, key: &str) -> String {
        match self.get(section, key) {
            Some(Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
            None => String::new(),
        }
    }

    pub fn bool(&self, section: &str, key: &str) -> bool {
        matches!(self.get(section, key), Some(Value::Boolean(true)))
    }


    pub fn str_list(&self, section: &str, key: &str) -> Vec<String> {
        match self.get(section, key) {
            Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            _ => Vec::new(),
        }
    }

    pub fn i64(&self, section: &str, key: &str) -> i64 {
        match self.get(section, key) {
            Some(Value::Integer(i)) => *i,
            Some(Value::Float(f)) => *f as i64,
            _ => 0,
        }
    }


}

fn toml_value(value: &Value) -> String {
    match value {
        Value::Boolean(b) => b.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => format!("{f:?}"),
        Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        other => other.to_string(),
    }
}

/// Rewrites one setting in config.toml, keeping comments and layout; appends it if missing.
pub fn save_setting(section: &str, key: &str, value: &Value) -> Result<()> {
    let path = config_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_string).collect();
    let pattern = Regex::new(&format!(
        r#"(?s)^(\s*{}\s*=\s*)("(?:[^"\\]|\\.)*"|[^#\s]+)(.*)$"#,
        regex::escape(key)
    ))?;
    let header = Regex::new(r"^\s*\[([^\]]+)\]")?;
    let mut current: Option<String> = None;
    let mut section_end: Option<usize> = None;
    for i in 0..lines.len() {
        if let Some(h) = header.captures(&lines[i]) {
            if current.as_deref() == Some(section) {
                section_end = Some(i);
                break;
            }
            current = Some(h[1].trim().to_string());
            continue;
        }
        if current.as_deref() == Some(section) {
            if let Some(m) = pattern.captures(&lines[i]) {
                lines[i] = format!("{}{}{}", &m[1], toml_value(value), &m[3]);
                std::fs::write(&path, lines.concat())?;
                return Ok(());
            }
        }
    }
    let new_line = format!("{key} = {}\n", toml_value(value));
    if current.as_deref() == Some(section) && section_end.is_none() {
        section_end = Some(lines.len()); // the section is the last one in the file
        if lines.last().is_some_and(|l| !l.ends_with('\n')) {
            lines.last_mut().unwrap().push('\n');
        }
    }
    match section_end {
        Some(i) => lines.insert(i, new_line),
        None => lines.push(format!("\n[{section}]\n{new_line}")),
    }
    std::fs::write(&path, lines.concat())?;
    Ok(())
}
