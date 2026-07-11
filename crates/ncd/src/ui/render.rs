use std::io::Write;

use crossterm::cursor;
use crossterm::queue;
use crossterm::style;
use crossterm::terminal;

use super::state::{Mode, TuiState};

/// Left margin for content.
const MARGIN: u16 = 4;
/// Indent for field lines under a row.
const FIELD_INDENT: u16 = 4;

pub fn render(stdout: &mut impl Write, state: &TuiState) -> std::io::Result<()> {
    let (cols, rows) = terminal::size().unwrap_or((80, 24));

    // Clear screen
    queue!(
        stdout,
        terminal::Clear(terminal::ClearType::All),
        style::SetForegroundColor(style::Color::White),
        cursor::MoveTo(0, 0)
    )?;

    let mut cursor_y: u16 = 0;

    // Draw Header
    let header = "NCD Configuration";
    queue!(
        stdout,
        cursor::MoveTo((cols.saturating_sub(header.len() as u16)) / 2, cursor_y),
        style::SetAttribute(style::Attribute::Bold),
        style::Print(header),
        style::SetAttribute(style::Attribute::Reset)
    )?;
    cursor_y += 1;

    // Draw Separator line
    let sep = "─".repeat(cols as usize);
    queue!(
        stdout,
        cursor::MoveTo(0, cursor_y),
        style::SetForegroundColor(style::Color::DarkGrey),
        style::Print(&sep),
        style::SetForegroundColor(style::Color::White)
    )?;
    cursor_y += 1;

    // Draw Adapter list
    if state.rows.is_empty() {
        queue!(
            stdout,
            cursor::MoveTo(MARGIN, cursor_y),
            style::SetForegroundColor(style::Color::DarkGrey),
            style::Print("No devices found."),
            style::SetForegroundColor(style::Color::White)
        )?;
    } else {
        for (i, row) in state.rows.iter().enumerate() {
            let is_selected = i == state.selected;

            // Draw row header: [x] name  (port: XXXX)
            draw_row_header(stdout, row, is_selected, cursor_y)?;
            cursor_y += 1;

            // Draw fields only when selected AND enabled
            if is_selected && row.enabled {
                cursor_y = draw_fields(stdout, state, cursor_y, cols)?;
            }
        }
    }

    // Draw Footer
    let footer_y = rows.saturating_sub(1);
    let help = match state.mode {
        Mode::Normal => {
            if state.rows.is_empty() {
                " q/ESC:quit"
            } else {
                " s:save  q/ESC:quit  Space:toggle  Enter/Tab:edit  ↑↓/jk:navigate"
            }
        }
        Mode::Editing(_) => " Esc:cancel  Enter/Tab:next  ↑↓:switch field  Backspace/type:edit",
    };
    // Truncate help text to fit terminal width
    let help_display = if help.len() > cols as usize {
        &help[..cols as usize]
    } else {
        help
    };
    queue!(
        stdout,
        cursor::MoveTo(0, footer_y),
        style::SetForegroundColor(style::Color::DarkGrey),
        style::Print(format!("{:width$}", help_display, width = cols as usize)),
        style::SetForegroundColor(style::Color::White)
    )?;

    // Place back cursor for editing
    if let Some(field_idx) = state.editing_field() {
        let (fx, fy) = field_cursor_position(state, field_idx);
        queue!(stdout, cursor::MoveTo(fx, fy), cursor::Show)?;
    } else {
        queue!(stdout, cursor::Hide)?;
    }

    stdout.flush()
}

/// Draw one device row: `  [x] device_name  — description`
fn draw_row_header(
    stdout: &mut impl Write,
    row: &super::state::DeviceRow,
    is_selected: bool,
    y: u16,
) -> std::io::Result<()> {
    queue!(stdout, cursor::MoveTo(0, y))?;

    if is_selected {
        // White background, black text for selected row
        queue!(
            stdout,
            style::SetBackgroundColor(style::Color::White),
            style::SetForegroundColor(style::Color::Black)
        )?;
    }

    let checkbox = if row.enabled { "[x]" } else { "[ ]" };
    let desc = if row.info.description.is_empty() {
        String::new()
    } else {
        format!("  — {}", row.info.description)
    };
    let line = format!("  {} {}{}", checkbox, row.info.name, desc);

    // Pad to full width with the current background color
    let (cols, _) = terminal::size().unwrap_or((80, 24));
    let padded = format!("{:width$}", line, width = cols as usize);
    queue!(stdout, style::Print(padded))?;

    // Reset to default background, white text
    queue!(
        stdout,
        style::SetBackgroundColor(style::Color::Reset),
        style::SetForegroundColor(style::Color::White)
    )?;

    Ok(())
}

/// Draw the editable fields for the selected row. Returns the new cursor_y.
fn draw_fields(
    stdout: &mut impl Write,
    state: &TuiState,
    mut y: u16,
    _cols: u16,
) -> std::io::Result<u16> {
    let row = state.selected_row();
    let field_count = 1 + row.option_keys.len(); // port + options
    let editing = state.editing_field();

    for idx in 0..field_count {
        let label = state.field_label(idx);
        let value = state.field_value(idx);
        let is_editing_this = editing == Some(idx);

        queue!(stdout, cursor::MoveTo(FIELD_INDENT, y))?;

        if is_editing_this {
            // Highlight the field being edited
            queue!(
                stdout,
                style::SetAttribute(style::Attribute::Bold),
                style::SetForegroundColor(style::Color::White)
            )?;
        } else {
            queue!(stdout, style::SetForegroundColor(style::Color::Grey))?;
        }

        let display = if is_editing_this {
            format!("{}: {}▌", label, value)
        } else {
            format!("{}: {}", label, value)
        };

        queue!(stdout, style::Print(display))?;

        // Reset
        queue!(
            stdout,
            style::SetAttribute(style::Attribute::Reset),
            style::SetForegroundColor(style::Color::White)
        )?;

        y += 1;
    }

    Ok(y)
}

/// Returns the cursor position (column, row) for an editing field.
fn field_cursor_position(state: &TuiState, field_idx: usize) -> (u16, u16) {
    // Layout:
    //   y=0: header
    //   y=1: separator
    //   y=2: first row header (or "No adapters")
    //   y=3: first field of selected row (if selected=0)
    //
    // For selected=N:
    //   y=2 + N: row N header
    //   y=3 + N: first field of row N, then +field_idx for subsequent fields
    let y: u16 = 3 + state.selected as u16 + field_idx as u16;

    // X position: indent + label + ": " + value length
    let label = state.field_label(field_idx);
    let value = state.field_value(field_idx);
    let x = FIELD_INDENT + label.len() as u16 + 2 + value.len() as u16;

    (x, y)
}
