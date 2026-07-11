use std::collections::HashMap;

use crossterm::event::KeyCode;

use crate::config::{DeviceEntry, HostConfig};

// ── Driver metadata ────────────────────────────────────────────

pub struct DriverInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub default_port: u16,
    pub fields: &'static [DriverField],
}

pub struct DriverField {
    pub key: &'static str,
    pub label: &'static str,
    pub default_value: &'static str,
}

pub fn builtin_drivers() -> Vec<DriverInfo> {
    vec![
        DriverInfo {
            name: "serial",
            description: "Serial port (RS-232/USB via pyserial)",
            default_port: 8080,
            fields: &[
                DriverField {
                    key: "device",
                    label: "Device Path",
                    default_value: "/dev/ttyUSB0",
                },
                DriverField {
                    key: "baud",
                    label: "Baud Rate",
                    default_value: "115200",
                },
            ],
        },
        DriverInfo {
            name: "keyboard",
            description: "Terminal keyboard (raw /dev/tty, no permissions)",
            default_port: 8081,
            fields: &[],
        },
    ]
}

// ── TUI state ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Navigating the driver list.
    Normal,
    /// Editing a specific field of the selected driver.
    /// Field index: 0 = port, 1+ = driver config fields.
    Editing(usize),
}

pub struct DriverRow {
    pub info: DriverInfo,
    pub enabled: bool,
    pub port_str: String,
    pub field_values: Vec<String>,
}

pub struct TuiState {
    pub rows: Vec<DriverRow>,
    pub selected: usize,
    pub mode: Mode,
    pub should_quit: bool,
    pub wants_save: bool,
}

impl TuiState {
    pub fn new() -> Self {
        let drivers = builtin_drivers();
        let rows: Vec<DriverRow> = drivers
            .into_iter()
            .enumerate()
            .map(|(i, info)| {
                let port = info.default_port + i as u16;
                DriverRow {
                    field_values: info
                        .fields
                        .iter()
                        .map(|f| f.default_value.to_string())
                        .collect(),
                    port_str: port.to_string(),
                    enabled: false,
                    info,
                }
            })
            .collect();

        Self {
            rows,
            selected: 0,
            mode: Mode::Normal,
            should_quit: false,
            wants_save: false,
        }
    }

    pub fn selected_row(&self) -> &DriverRow {
        &self.rows[self.selected]
    }

    fn selected_row_mut(&mut self) -> &mut DriverRow {
        &mut self.rows[self.selected]
    }

    fn field_count(&self) -> usize {
        1 + self.selected_row().info.fields.len()
    }

    fn get_field_value_mut(&mut self, idx: usize) -> &mut String {
        let row = self.selected_row_mut();
        if idx == 0 {
            &mut row.port_str
        } else {
            &mut row.field_values[idx - 1]
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
                    .info
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, f)| (f.key.to_string(), r.field_values[i].clone()))
                    .collect();
                DeviceEntry {
                    driver: r.info.name.to_string(),
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
                let row = self.selected_row_mut();
                row.enabled = !row.enabled;
            }
            KeyCode::Enter => {
                if self.selected_row().enabled {
                    self.mode = Mode::Editing(0);
                }
            }
            KeyCode::Tab => {
                if self.selected_row().enabled {
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

    /// Is a specific field of the selected row being edited?
    pub fn is_editing_field(&self, idx: usize) -> bool {
        self.editing_field() == Some(idx)
    }
}
