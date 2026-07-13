use std::collections::HashMap;

use crossterm::event::KeyCode;

use crate::adapter_loader::list::{DeviceInfo, get_all_devices};
use crate::config::{DeviceEntry, HostConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Navigating the device list.
    Normal,
    /// Editing a specific field of the selected device.
    /// Field index: 0 = port, 1+ = device option fields.
    Editing(usize),
}

pub struct DeviceRow {
    pub info: DeviceInfo,
    pub enabled: bool,
    pub port_str: String,
    /// Editable values for each option, in the same order as option_keys.
    pub field_values: Vec<String>,
    /// Option keys corresponding to each field value (for config reconstruction).
    pub option_keys: Vec<String>,
}

pub struct TuiState {
    pub rows: Vec<DeviceRow>,
    pub selected: usize,
    pub mode: Mode,
    pub should_quit: bool,
    pub wants_save: bool,
}

impl TuiState {
    pub fn new() -> Self {
        let devices = get_all_devices();
        let mut rows: Vec<DeviceRow> = devices
            .into_iter()
            .map(|info| {
                let option_keys: Vec<String> = info.options.keys().cloned().collect();
                let field_values: Vec<String> = info.options.values().cloned().collect();
                let port_str = info.default_port.to_string();
                DeviceRow {
                    info,
                    field_values,
                    option_keys,
                    port_str,
                    enabled: false,
                }
            })
            .collect();

        // Merge existing saved config if present.
        if let Some(cfg) = HostConfig::load() {
            for entry in &cfg.device {
                if let Some(row) = rows.iter_mut().find(|r| {
                    r.info.adapter_name == entry.driver
                        && r.info.identifier == entry.device_identifier
                }) {
                    row.enabled = true;
                    row.port_str = entry.port.to_string();
                    // Merge saved option values into the row's fields.
                    for (opt_key, opt_val) in &entry.options {
                        if let Some(pos) = row.option_keys.iter().position(|k| k == opt_key) {
                            row.field_values[pos] = opt_val.clone();
                        } else {
                            // Option from config not in default list — add it.
                            row.option_keys.push(opt_key.clone());
                            row.field_values.push(opt_val.clone());
                        }
                    }
                }
            }
        }

        Self {
            rows,
            selected: 0,
            mode: Mode::Normal,
            should_quit: false,
            wants_save: false,
        }
    }

    pub fn selected_row(&self) -> &DeviceRow {
        &self.rows[self.selected]
    }

    fn selected_row_mut(&mut self) -> &mut DeviceRow {
        &mut self.rows[self.selected]
    }

    /// Total number of editable fields for the selected row (port + options).
    fn field_count(&self) -> usize {
        if self.rows.is_empty() {
            return 0;
        }
        1 + self.selected_row().option_keys.len()
    }

    fn get_field_value_mut(&mut self, idx: usize) -> &mut String {
        let row = self.selected_row_mut();
        if idx == 0 {
            &mut row.port_str
        } else {
            &mut row.field_values[idx - 1]
        }
    }

    /// Get the display label for a field index.
    pub fn field_label(&self, idx: usize) -> &str {
        if idx == 0 {
            "Port"
        } else {
            self.selected_row()
                .option_keys
                .get(idx - 1)
                .map(|s| s.as_str())
                .unwrap_or("Unknown")
        }
    }

    /// Get the display value for a field index.
    pub fn field_value(&self, idx: usize) -> &str {
        if idx == 0 {
            &self.selected_row().port_str
        } else {
            self.selected_row()
                .field_values
                .get(idx - 1)
                .map(|s| s.as_str())
                .unwrap_or("")
        }
    }

    pub fn build_config(&self) -> HostConfig {
        let devices: Vec<DeviceEntry> = self
            .rows
            .iter()
            .filter(|r| r.enabled)
            .map(|r| {
                let port: u16 = r.port_str.parse().unwrap_or(r.info.default_port);
                let options: HashMap<String, String> = r
                    .option_keys
                    .iter()
                    .zip(r.field_values.iter())
                    .map(|(k, v)| (k.clone(), sanitize_option_value(k, v)))
                    .collect();
                DeviceEntry {
                    driver: r.info.adapter_name.clone(),
                    device_identifier: r.info.identifier.clone(),
                    device_name: r.info.name.clone(),
                    port,
                    options,
                }
            })
            .collect();

        HostConfig { device: devices }
    }

    pub fn handle_key(&mut self, key: KeyCode) {
        match self.mode {
            Mode::Normal => self.handle_normal_key(key),
            Mode::Editing(_) => self.handle_editing_key(key),
        }
    }

    pub fn handle_paste(&mut self, text: &str) {
        if let Mode::Editing(field_idx) = self.mode {
            self.get_field_value_mut(field_idx).push_str(text);
        }
    }

    fn handle_normal_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < self.rows.len() {
                    self.selected += 1;
                }
            }
            KeyCode::Char(' ') => {
                if !self.rows.is_empty() {
                    let row = self.selected_row_mut();
                    row.enabled = !row.enabled;
                }
            }
            KeyCode::Enter => {
                if !self.rows.is_empty() && self.selected_row().enabled {
                    self.mode = Mode::Editing(0);
                }
            }
            KeyCode::Tab => {
                if !self.rows.is_empty() && self.selected_row().enabled {
                    self.mode = Mode::Editing(0);
                }
            }
            KeyCode::Char('s') => {
                self.wants_save = true;
                self.should_quit = true;
            }
            KeyCode::Char('q') | KeyCode::Esc => {
                self.wants_save = false;
                self.should_quit = true;
            }
            _ => {}
        }
    }

    fn handle_editing_key(&mut self, key: KeyCode) {
        let field_idx = match self.mode {
            Mode::Editing(i) => i,
            _ => return,
        };

        match key {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
            }
            KeyCode::Enter | KeyCode::Tab => {
                let next = field_idx + 1;
                if next >= self.field_count() {
                    self.mode = Mode::Normal;
                } else {
                    self.mode = Mode::Editing(next);
                }
            }
            KeyCode::Up => {
                if field_idx > 0 {
                    self.mode = Mode::Editing(field_idx - 1);
                }
            }
            KeyCode::Down => {
                if field_idx + 1 < self.field_count() {
                    self.mode = Mode::Editing(field_idx + 1);
                }
            }
            KeyCode::Backspace => {
                self.get_field_value_mut(field_idx).pop();
            }
            KeyCode::Char(c) => {
                self.get_field_value_mut(field_idx).push(c);
            }
            _ => {}
        }
    }

    /// Which field index is being edited (if any).
    pub fn editing_field(&self) -> Option<usize> {
        match self.mode {
            Mode::Editing(i) => Some(i),
            Mode::Normal => None,
        }
    }
}

fn sanitize_option_value(key: &str, value: &str) -> String {
    if key != "file_path" {
        return value.to_string();
    }

    let trimmed = value.trim();
    let quoted = trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')));

    if quoted {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}
