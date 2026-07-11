use std::io::Write;

use crossterm::cursor;
use crossterm::queue;
use crossterm::style;
use crossterm::terminal;

use super::state::TuiState;

/// How many lines a driver row takes when collapsed (enabled or not).
const ROW_HEIGHT_COLLAPSED: u16 = 1;
/// Extra lines when the driver is expanded (showing fields).
const ROW_HEIGHT_EXPANDED: u16 = 1; // one line for enabled fields

pub fn render(stdout: &mut impl Write, state: &TuiState) -> std::io::Result<()> {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));

    // Clear
    queue!(
        stdout,
        terminal::Clear(terminal::ClearType::All),
        cursor::MoveTo(0, 0)
    )?;

    let mut cursor_y: u16 = 0;

    // Draw header
    let header_text = "NCD Configuration";
    queue!(
        stdout,
        cursor::MoveTo((cols - header_text.len() as u16) / 2, cursor_y), // align center
        style::SetAttribute(style::Attribute::Bold),
        style::Print(header_text),
        style::SetAttribute(style::Attribute::Reset)
    )?;
    cursor_y += 1;

    // Draw driver list
    draw_drivers(stdout, state, &mut cursor_y)?;

    // Draw footer
    cursor_y = rows.saturating_sub(1);
    let help = match state.editing_field() {
        Some(_) => " Esc:cancel  Enter/Tab:next  ↑↓:switch field  Backspace/type:edit",
        None => " s:save  q/ESC:quit  Space:toggle  Enter/Tab:edit  ↑↓/jk:navigate",
    };
    queue!(
        stdout,
        cursor::MoveTo(2, cursor_y),
        style::SetForegroundColor(style::Color::DarkGrey),
        style::Print(help),
        style::SetForegroundColor(style::Color::Reset)
    )?;

    // Place cursor appropriately
    if let Some(field_idx) = state.editing_field() {
        // Position cursor inside the editing field
        let (fx, fy) = field_cursor_position(state, field_idx);
        queue!(stdout, cursor::MoveTo(fx, fy))?;
    }

    stdout.flush()
}

/// Returns the cursor position for an editing field (column, row).
fn field_cursor_position(state: &TuiState, field_idx: usize) -> (u16, u16) {
    // This is a simplification — the actual position depends on the rendering.
    // We compute the row offset by counting lines before this driver.
    let mut y: u16 = 3; // top border + header + blank
    for (i, row) in state.rows.iter().enumerate() {
        if i == state.selected {
            // Found it — the field line is y+1 (after the driver name line)
            let field_y = y + 1;
            let prefix: String = if field_idx == 0 {
                "    Port: [".into()
            } else {
                let label = row.info.fields[field_idx - 1].label;
                format!("    {label}: [")
            };
            let x = prefix.len() as u16 + 2; // +2 for left border padding
            return (x, field_y);
        }
        y += ROW_HEIGHT_COLLAPSED;
        if row.enabled && i == state.selected {
            y += ROW_HEIGHT_EXPANDED;
        }
    }
    (0, 0)
}

fn draw_drivers(stdout: &mut impl Write, state: &TuiState, y: &mut u16) -> std::io::Result<()> {
    for (i, row) in state.rows.iter().enumerate() {
        let is_selected = i == state.selected;

        // Draw driver name
        queue!(stdout, cursor::MoveTo(2, *y))?;

        // Highlight selected row
        if is_selected {
            queue!(stdout, style::SetBackgroundColor(style::Color::White))?;
            queue!(stdout, style::SetForegroundColor(style::Color::Black))?;
        }

        // Draw text
        let checkbox = if row.enabled { "[x]" } else { "[ ]" };
        let name = row.info.name;
        queue!(
            stdout,
            style::Print(checkbox),
            style::Print(" "),
            style::Print(name),
            style::SetForegroundColor(style::Color::Reset)
        )?;

        queue!(stdout, style::SetAttribute(style::Attribute::Reset))?;

        *y += 1;

        draw_field(stdout)?;
    }

    Ok(())
}

fn draw_field(_stdout: &mut impl Write) -> std::io::Result<()> {
    // TODO: Support display and editing fields
    Ok(())
}
