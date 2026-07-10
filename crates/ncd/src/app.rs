//! Application layer — CLI parsing, device-selection TUI, and command
//! dispatch.  Everything above the library crates lives here.

use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

use crate::connection::NcdConnection;
use crate::device::NcdDeviceOperations;
use crate::drivers::{CameraDriver, KeyboardDriver, SerialDriver, SshDriver};
use crate::registry::{DeviceInfo, DeviceKind, DevicesRegistry};
use crate::session::NcdSession;

// ── Config ────────────────────────────────────────────────────────

const BASE_PORT: u16 = 10000;

// ── CLI ───────────────────────────────────────────────────────────

const USAGE: &str = "\
ncd — Network Character Device (local host)

Expose local devices (camera, serial, keyboard, SSH) to a remote
Linux machine over TCP using the NCD protocol.

Usage:
  ncd run [--verbose]   Select devices via TUI and start listening
  ncd list              List all detected devices
  ncd help              Show this message

Options:
  --verbose, -v   Show data flowing in both directions in real time

After starting, Linux connects to <host-ip>:<port> using ncdd or
libncd-runtime.  Each port exposes one device.

Devices and their data flow:
  camera    →  Linux reads raw image frames (read-only)
  serial    ↔  bidirectional raw bytes (read + write)
  keyboard  ↔  Linux reads captured keys, writes to inject keys
  ssh       ↔  Linux reads stdout, writes to stdin (WIP)
";

pub struct Args {
    pub command: Command,
    pub verbose: bool,
}

pub enum Command {
    Run,
    List,
}

pub fn parse() -> Option<Args> {
    let mut args = std::env::args().skip(1);
    let mut verbose = false;
    let mut command = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "help" | "--help" | "-h" => {
                println!("{USAGE}");
                return None;
            }
            "--verbose" | "-v" => verbose = true,
            "run" => command = Some(Command::Run),
            "list" => command = Some(Command::List),
            other => {
                eprintln!("ncd: unknown option '{other}'\n{USAGE}");
                return None;
            }
        }
    }

    command.map(|c| Args { command: c, verbose })
}

// ── Helpers ───────────────────────────────────────────────────────

async fn find_free_ports(start: u16, count: usize) -> Vec<u16> {
    let mut ports = Vec::with_capacity(count);
    let mut port = start;
    while ports.len() < count {
        match tokio::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).await {
            Ok(l) => {
                drop(l);
                ports.push(port);
            }
            Err(_) => eprintln!("  port {port} occupied — skipping"),
        }
        port = port.wrapping_add(1);
    }
    ports
}

fn make_driver(
    kind: DeviceKind,
    path: &str,
    notify: Arc<Notify>,
) -> Option<Arc<Mutex<Box<dyn NcdDeviceOperations>>>> {
    let driver: Box<dyn NcdDeviceOperations> = match kind {
        DeviceKind::Camera => Box::new(CameraDriver::new(path, notify)),
        DeviceKind::Keyboard => Box::new(KeyboardDriver::new(path, notify)),
        DeviceKind::Serial => Box::new(SerialDriver::new(path, notify)),
        DeviceKind::Ssh => Box::new(SshDriver::new(path, notify)),
        DeviceKind::Unknown => return None,
    };
    Some(Arc::new(Mutex::new(driver)))
}

// ── TUI: device selection ─────────────────────────────────────────

pub fn select_devices(all: &[DeviceInfo]) -> Vec<DeviceInfo> {
    if all.is_empty() {
        return vec![];
    }
    if crossterm::terminal::enable_raw_mode().is_ok() {
        let sel = run_tui(all);
        let _ = crossterm::terminal::disable_raw_mode();
        sel
    } else {
        run_plain(all)
    }
}

fn run_tui(all: &[DeviceInfo]) -> Vec<DeviceInfo> {
    use crossterm::{
        cursor,
        event::{self, Event, KeyCode, KeyEventKind},
        execute,
        style::{Print, PrintStyledContent, Stylize},
        terminal::{self, Clear, ClearType},
    };

    let mut selected = vec![false; all.len()];
    let mut cursor: usize = 0;

    let _ = execute!(io::stdout(), terminal::EnterAlternateScreen, cursor::Hide);

    struct TermGuard;
    impl Drop for TermGuard {
        fn drop(&mut self) {
            let _ = execute!(io::stdout(), cursor::Show, terminal::LeaveAlternateScreen);
        }
    }
    let _guard = TermGuard;

    loop {
        let _ = execute!(
            io::stdout(),
            cursor::MoveTo(0, 0),
            Clear(ClearType::All),
            Print("Select devices to expose to Linux\n".bold().underlined()),
            Print("  ↑↓: move   Space: toggle   Enter: confirm   Esc: quit\n"),
            Print("  Each selected device gets its own port, starting at 10000\n\n"),
        );

        for (i, d) in all.iter().enumerate() {
            let chk = if selected[i] { '◼' } else { '◻' };
            let line = format!(
                "  {chk}  {kind:<8}  ←  {path}",
                kind = format!("{}", d.kind).to_lowercase(),
                path = d.path,
            );
            if i == cursor {
                let _ = execute!(io::stdout(), PrintStyledContent(line.reverse()));
            } else {
                let _ = execute!(io::stdout(), Print(line));
            }
            let _ = execute!(io::stdout(), Print("\n"));
        }

        let count = selected.iter().filter(|&&s| s).count();
        let footer = if count > 0 {
            format!(
                "\n  {count} device(s) selected  →  ports {}-{}\n",
                BASE_PORT,
                BASE_PORT + count.saturating_sub(1) as u16,
            )
        } else {
            "\n  0 devices selected\n".into()
        };
        let _ = execute!(io::stdout(), Print(footer));
        let _ = io::stdout().flush();

        match event::read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Up if cursor > 0 => cursor -= 1,
                KeyCode::Down if cursor + 1 < all.len() => cursor += 1,
                KeyCode::Char(' ') => selected[cursor] = !selected[cursor],
                KeyCode::Enter | KeyCode::Char('\r') | KeyCode::Char('\n') => {
                    // If nothing is checked, select the current item.
                    let any = selected.iter().any(|&s| s);
                    if !any {
                        selected[cursor] = true;
                    }
                    break;
                }
                KeyCode::Esc => {
                    selected.fill(false);
                    break;
                }
                _ => {}
            },
            _ => {}
        }
    }

    all.iter()
        .enumerate()
        .filter(|(i, _)| selected[*i])
        .map(|(_, d)| d.clone())
        .collect()
}

fn run_plain(all: &[DeviceInfo]) -> Vec<DeviceInfo> {
    println!("\nDetected devices:");
    for (i, d) in all.iter().enumerate() {
        let kind = format!("{}", d.kind).to_lowercase();
        println!("  [{i}] {kind}  ←  {}", d.path);
    }
    println!("\nSelect devices (numbers / 'all' / empty to quit):");
    print!("> ");
    io::stdout().flush().ok();

    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return vec![];
    }
    let line = line.trim().to_lowercase();
    if line.is_empty() {
        return vec![];
    }
    if line == "all" {
        return all.to_vec();
    }

    let mut selected = Vec::new();
    for part in line.split(',') {
        if let Ok(idx) = part.trim().parse::<usize>()
            && idx < all.len()
        {
            selected.push(all[idx].clone());
        }
    }
    selected
}

fn print_device_hint(kind: DeviceKind, verbose: bool) {
    let hint = match kind {
        DeviceKind::Camera => "\
  ┌─ Camera (read-only) ─────────────────────────────────┐
  │  Linux will receive raw image frames via read().      │
  │  Write is not supported — cameras don't accept data.  │
  └───────────────────────────────────────────────────────┘",
        DeviceKind::Serial => "\
  ┌─ Serial (read + write) ──────────────────────────────┐
  │  Linux read()  ←  serial port RX data                │
  │  Linux write() →  serial port TX data                │
  │  Baud rate: 115200                                   │
  └──────────────────────────────────────────────────────┘",
        DeviceKind::Keyboard => "\
  ┌─ Keyboard (read + write) ────────────────────────────┐
  │  Linux read()  ←  captured keystrokes on this host   │
  │  Linux write() →  inject keystrokes on this host     │
  │  Try: echo 'notepad' | ncdd --write <port>           │
  └──────────────────────────────────────────────────────┘",
        DeviceKind::Ssh => "\
  ┌─ SSH (read + write) ─────────────────────────────────┐
  │  Linux read()  ←  remote command output              │
  │  Linux write() →  send commands to remote shell      │
  │  (not yet implemented — file-backed test only)       │
  └──────────────────────────────────────────────────────┘",
        DeviceKind::Unknown => "",
    };
    if !hint.is_empty() {
        println!("{hint}");
    }
    if verbose {
        println!(
            "  [verbose on — data flow will be shown below]\n  \
             → host = data received from Linux\n  \
             host → = data sent to Linux\n"
        );
    } else {
        println!("  [add --verbose to see data flow in real time]");
    }
}

// ── Commands ──────────────────────────────────────────────────────

/// `ncd list` — print detected devices and exit.
pub fn cmd_list() {
    let all = DevicesRegistry::new().detect_all();
    if all.is_empty() {
        println!("No devices detected.");
    } else {
        println!("\nDetected devices:\n");
        for d in &all {
            println!(
                "  {kind:<8}  ←  {path}",
                kind = format!("{}", d.kind).to_lowercase(),
                path = d.path,
            );
        }
        println!();
    }
}

/// `ncd run` — select devices via TUI, then start listening.
pub async fn cmd_run(verbose: bool) {
    let all = DevicesRegistry::new().detect_all();
    if all.is_empty() {
        eprintln!("No devices detected — nothing to run");
        return;
    }

    let selected = select_devices(&all);
    if selected.is_empty() {
        println!("No devices selected, exiting.");
        return;
    }

    let ports = find_free_ports(BASE_PORT, selected.len()).await;
    let mapping: Vec<(u16, DeviceInfo)> = ports
        .iter()
        .zip(selected.iter())
        .map(|(&p, d)| (p, d.clone()))
        .collect();

    let mut registry = DevicesRegistry::new();
    registry.register_with_ports(&mapping);

    println!("\nExposing:\n");
    println!("  {:<6} {:<8}  {:<20}  Data flow", "Port", "Kind", "Device");
    println!("  ------  --------  --------------------  -------------------");
    for (port, info) in registry.get_all_devices() {
        let (flow, dir) = match info.kind {
            DeviceKind::Camera => ("raw frames → Linux", "(read-only)"),
            DeviceKind::Serial => ("raw bytes ↔ Linux", "(read + write)"),
            DeviceKind::Keyboard => ("keystrokes ↔ Linux", "(read + write)"),
            DeviceKind::Ssh => ("terminal ↔ Linux", "(read + write)"),
            DeviceKind::Unknown => ("—", ""),
        };
        println!(
            "  {port:<6} {kind:<8}  {path:<20}  {flow} {dir}",
            kind = format!("{}", info.kind).to_lowercase(),
            path = info.path,
        );
    }
    println!();
    for (port, info) in registry.get_all_devices() {
        let tip = match info.kind {
            DeviceKind::Camera => "Linux read() receives raw frames",
            DeviceKind::Serial => "Linux read()/write() = serial RX/TX",
            DeviceKind::Keyboard => "Linux read()=captured keys, write()=inject keys",
            DeviceKind::Ssh => "Linux read()=stdout, write()=stdin",
            _ => "",
        };
        println!("  {port} → <windows-ip>:{port}  |  {tip}");
    }
    println!("\nWaiting for Linux to connect … (Ctrl+C to stop)\n");

    let mut handles = vec![];
    for (port, info) in registry.get_all_devices() {
        let path = info.path.clone();
        let kind = info.kind;
        let notify = Arc::new(Notify::new());
        let Some(driver) = make_driver(kind, &path, notify.clone()) else {
            eprintln!("[port {port}] Unknown device — skipped");
            continue;
        };

        handles.push(tokio::spawn(async move {
            loop {
                // Wait for a Linux client to connect.
                let mut conn = match NcdConnection::create_connection(
                    IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                    port,
                )
                .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("[port {port}] bind error: {e}");
                        return;
                    }
                };

                // Show who connected.
                let peer = conn
                    .get_connection_status()
                    .await
                    .map(|s| s.peer_addr.to_string())
                    .unwrap_or_else(|_| "?".into());
                println!(
                    "[port {port}] {kind:?} ← {path}  connected from {peer}"
                );

                // ── device-specific hints ──────────────────────
                print_device_hint(kind, verbose);

                let mut session =
                    NcdSession::new(conn, driver.clone(), notify.clone());
                session.set_verbose(verbose);
                let result = session.run().await;
                drop(session);

                // The remote side closed the connection.
                match result {
                    Ok(()) => {
                        println!("[port {port}] disconnected (peer closed)");
                    }
                    Err(ref e) => {
                        eprintln!("[port {port}] disconnected: {e}");
                    }
                }
                println!("[port {port}] waiting for next connection …");
            }
        }));
    }

    for handle in handles {
        let _ = handle.await;
    }
}
