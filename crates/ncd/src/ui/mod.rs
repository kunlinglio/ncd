mod render;
mod state;

use std::io;

use crossterm::cursor;
use crossterm::event;
use crossterm::execute;
use crossterm::terminal;

use crate::config::HostConfig;
use state::TuiState;

/// Run the interactive configuration TUI.
/// Returns Some(HostConfig) if the user saved, None if they quit.
pub fn run_tui() -> Option<HostConfig> {
    let mut stdout = io::stdout();
    crossterm::terminal::enable_raw_mode().expect("Failed to enable raw mode");
    execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide).ok();

    let mut state = TuiState::new();
    // Main loop
    let result = loop {
        render::render(&mut stdout, &state).expect("Failed to render");

        if state.should_quit {
            break if state.wants_save {
                Some(state.build_config())
            } else {
                None
            };
        }

        match event::read() {
            Ok(event::Event::Key(key)) => {
                if key.kind == event::KeyEventKind::Press || key.kind == event::KeyEventKind::Repeat
                {
                    state.handle_key(key.code);
                }
            }
            Ok(event::Event::Resize(_, _)) => {
                // Redraw on resize
            }
            _ => {}
        }
    };

    // Restore terminal
    execute!(stdout, cursor::Show, terminal::LeaveAlternateScreen).ok();
    terminal::disable_raw_mode().ok();

    result
}
