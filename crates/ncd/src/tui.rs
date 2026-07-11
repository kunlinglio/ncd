use std::collections::HashMap;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::config::{DeviceEntry, HostConfig};

/// Metadata about a built-in driver, used to render the TUI.
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

/// Return the list of built-in drivers available for selection.
fn builtin_drivers() -> Vec<DriverInfo> {
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
            description: "Keyboard input capture (via pynput)",
            default_port: 8081,
            fields: &[DriverField {
                key: "capture",
                label: "Capture Mode",
                default_value: "all",
            }],
        },
    ]
}

/// Which part of the UI has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Navigating the driver list.
    Normal,
    /// Editing a specific field of the selected driver.
    /// Field index: 0 = port, 1+ = driver config fields.
    Editing(usize),
}

/// Runtime state for one driver row in the TUI.
struct DriverRow {
    info: DriverInfo,
    enabled: bool,
    port_str: String,
    field_values: Vec<String>,
}

struct TuiState {
    rows: Vec<DriverRow>,
    selected: usize,
    mode: Mode,
    status: String,
    should_quit: bool,
}

impl TuiState {
    fn new() -> Self {
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
            status: String::from("↑↓ navigate  Enter toggle/edit  Tab edit  F1 save  Esc quit"),
            should_quit: false,
        }
    }

    fn selected_row(&self) -> &DriverRow {
        &self.rows[self.selected]
    }

    fn selected_row_mut(&mut self) -> &mut DriverRow {
        &mut self.rows[self.selected]
    }

    fn field_count(&self) -> usize {
        1 + self.selected_row().info.fields.len() // port + driver fields
    }

    /// Returns a mutable reference to the field value at the given editing index.
    fn get_field_value_mut(&mut self, idx: usize) -> &mut String {
        let row = self.selected_row_mut();
        if idx == 0 {
            &mut row.port_str
        } else {
            &mut row.field_values[idx - 1]
        }
    }

    fn build_config(&self) -> HostConfig {
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

    fn handle_key(&mut self, key: KeyCode) {
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
            KeyCode::Enter => {
                let row = self.selected_row_mut();
                if !row.enabled {
                    row.enabled = true;
                    self.status =
                        format!("Enabled '{}'. Press Enter to edit fields.", row.info.name);
                } else {
                    self.mode = Mode::Editing(0);
                    self.status =
                        String::from("Editing port. Tab/↑↓ to switch field. Esc to finish.");
                }
            }
            KeyCode::Tab => {
                if self.selected_row().enabled {
                    self.mode = Mode::Editing(0);
                    self.status =
                        String::from("Editing port. Tab/↑↓ to switch field. Esc to finish.");
                }
            }
            KeyCode::Char(' ') => {
                let row = self.selected_row_mut();
                row.enabled = !row.enabled;
                self.status = format!(
                    "{} '{}'",
                    if row.enabled { "Enabled" } else { "Disabled" },
                    row.info.name
                );
            }
            KeyCode::F(1) | KeyCode::Char('s') => {
                self.should_quit = true;
                self.status = String::from("Saving configuration...");
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.should_quit = true;
                self.status = String::from("Quit without saving.");
            }
            _ => {}
        }
    }

    fn handle_editing_key(&mut self, key: KeyCode) {
        let field_idx = match self.mode {
            Mode::Editing(i) => i,
            _ => unreachable!(),
        };

        match key {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.status =
                    String::from("↑↓ navigate  Enter toggle/edit  Tab edit  F1 save  Esc quit");
            }
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                self.status =
                    String::from("↑↓ navigate  Enter toggle/edit  Tab edit  F1 save  Esc quit");
            }
            KeyCode::Tab => {
                let next = (field_idx + 1) % self.field_count();
                self.mode = Mode::Editing(next);
                self.status = format!("Editing: {}", self.field_label(next));
            }
            KeyCode::Up => {
                if field_idx > 0 {
                    self.mode = Mode::Editing(field_idx - 1);
                } else {
                    self.mode = Mode::Editing(self.field_count() - 1);
                }
                self.status = format!(
                    "Editing: {}",
                    self.field_label(match self.mode {
                        Mode::Editing(i) => i,
                        _ => unreachable!(),
                    })
                );
            }
            KeyCode::Down => {
                let next = (field_idx + 1) % self.field_count();
                self.mode = Mode::Editing(next);
                self.status = format!("Editing: {}", self.field_label(next));
            }
            KeyCode::Backspace => {
                let val = self.get_field_value_mut(field_idx);
                val.pop();
            }
            KeyCode::Char(c) => {
                let val = self.get_field_value_mut(field_idx);
                val.push(c);
            }
            _ => {}
        }
    }

    fn field_label(&self, idx: usize) -> String {
        let row = self.selected_row();
        if idx == 0 {
            "Port".to_string()
        } else {
            row.info.fields[idx - 1].label.to_string()
        }
    }
}

// ── Rendering ────────────────────────────────────────────────────

fn render(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>, state: &TuiState) {
    terminal
        .draw(|frame| {
            let area = frame.area();

            // Layout: header, main list, footer
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3), // header
                    Constraint::Min(1),    // driver list
                    Constraint::Length(2), // footer / status
                ])
                .split(area);

            // ── Header ──
            let header = Paragraph::new("NCD Host Configuration")
                .block(Block::default().borders(Borders::ALL).title("ncd config"))
                .style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                );
            frame.render_widget(header, chunks[0]);

            // ── Driver list ──
            let list_area = chunks[1];
            let row_height = 4; // each driver row: 1 border + 2 content + 1 border
            let visible_rows = (list_area.height as usize).saturating_sub(2) / row_height;

            // Build lines for each driver
            let mut lines: Vec<Line> = Vec::new();
            for (i, row) in state.rows.iter().enumerate() {
                let is_selected = i == state.selected;
                let style = if is_selected {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default()
                };

                // Checkbox
                let checkbox = if row.enabled { "[x]" } else { "[ ]" };
                let name_style = if is_selected {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else if row.enabled {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::DarkGray)
                };

                lines.push(Line::from(vec![
                    Span::styled(checkbox, style),
                    Span::raw(" "),
                    Span::styled(row.info.name, name_style),
                    Span::raw("  "),
                    Span::styled(row.info.description, Style::default().fg(Color::DarkGray)),
                ]));

                if row.enabled {
                    // Port field
                    let port_text = format!("Port: [{}]", row.port_str);
                    let port_highlight = is_selected && matches!(state.mode, Mode::Editing(0));
                    let port_style = if port_highlight {
                        Style::default().fg(Color::White).bg(Color::DarkGray)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    lines.push(Line::from(vec![
                        Span::raw("    "),
                        Span::styled(port_text, port_style),
                    ]));

                    // Driver-specific fields
                    for (fi, field) in row.info.fields.iter().enumerate() {
                        let edit_idx = fi + 1; // 0 is port
                        let field_text = format!("{}: [{}]", field.label, row.field_values[fi]);
                        let field_highlight =
                            is_selected && matches!(state.mode, Mode::Editing(e) if e == edit_idx);
                        let field_style = if field_highlight {
                            Style::default().fg(Color::White).bg(Color::DarkGray)
                        } else {
                            Style::default()
                        };
                        lines.push(Line::from(vec![
                            Span::raw("    "),
                            Span::styled(field_text, field_style),
                        ]));
                    }
                }

                // Separator between drivers
                if i + 1 < state.rows.len() {
                    lines.push(Line::from(Span::styled(
                        "─".repeat(list_area.width as usize),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }

            let list = Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL))
                .scroll(((state.selected.saturating_sub(visible_rows / 2)) as u16, 0));
            frame.render_widget(list, list_area);

            // ── Footer / Status ──
            let status_style = if state.status.contains("error") || state.status.contains("Error") {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let footer =
                Paragraph::new(Line::from(vec![Span::styled(&state.status, status_style)]));
            frame.render_widget(footer, chunks[2]);
        })
        .unwrap();
}

// ── Public API ───────────────────────────────────────────────────

/// Run the TUI and return the configured HostConfig.
/// Returns None if the user cancelled (Esc/Q without saving).
pub fn run_tui() -> Option<HostConfig> {
    let mut stdout = std::io::stdout();
    enable_raw_mode().expect("Failed to enable raw mode");
    execute!(stdout, crossterm::terminal::EnterAlternateScreen).ok();

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).expect("Failed to create terminal");

    let mut state = TuiState::new();
    let result = loop {
        render(&mut terminal, &state);

        if state.should_quit {
            break if state.status.contains("Saving") {
                Some(state.build_config())
            } else {
                None
            };
        }

        // Wait for next key event
        if let Ok(event) = event::read() {
            if let Event::Key(key) = event {
                if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat {
                    state.handle_key(key.code);
                }
            }
        }
    };

    // Restore terminal
    disable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )
    .ok();

    result
}
