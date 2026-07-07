use std::collections::VecDeque;
use std::io::Cursor;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU8, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use libncd::frame::Frame;
use libncd::packet::Packet;
use libncd::{read_frame, read_packet, write_packet};

use crate::error::Error;

/// Connection lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ConnState {
    Disconnected = 0,
    Connecting = 1,
    Handshaking = 2,
    Connected = 3,
    Closing = 4,
    Closed = 5,
}

impl ConnState {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => ConnState::Disconnected,
            1 => ConnState::Connecting,
            2 => ConnState::Handshaking,
            3 => ConnState::Connected,
            4 => ConnState::Closing,
            5 => ConnState::Closed,
            _ => ConnState::Disconnected,
        }
    }
}

/// Mutable state protected by a single tokio Mutex.
struct Inner {
    stream: TcpStream,
    raw: Vec<u8>,
    frames: VecDeque<Frame>,
    /// Leftover bytes from a Data payload that was larger than the user's
    /// read buffer. Served first on the next read() call.
    leftover: Vec<u8>,
}

/// A connection to an NCD daemon.
///
/// Created via [`open()`](crate::open), consumed by [`close()`](crate::close).
/// `read()` and `write()` take `&Connection` — interior mutability is
/// handled by a tokio mutex internally.
pub struct Connection {
    inner: Mutex<Inner>,
    state: AtomicU8,
    peer_addr: SocketAddr,
}

// ── Inner helpers ──────────────────────────────────────────────

impl Inner {
    /// Encode and write a single packet to the stream.
    async fn send_packet(&mut self, packet: &Packet) -> Result<(), Error> {
        let mut cursor = Cursor::new(Vec::new());
        write_packet(&mut cursor, packet)?;
        self.stream.write_all(&cursor.into_inner()).await?;
        Ok(())
    }

    /// Read whatever bytes are available on the stream and decode them
    /// into frames. Returns `Ok(false)` on EOF.
    async fn fill_frames(&mut self) -> Result<bool, Error> {
        let mut tmp = [0u8; 8192];
        let n = self.stream.read(&mut tmp).await?;
        if n == 0 {
            return Ok(false); // EOF
        }

        self.raw.extend_from_slice(&tmp[..n]);

        let consumed = {
            let mut cursor = Cursor::new(&self.raw[..]);
            while let Some(frame) = read_frame(&mut cursor)? {
                self.frames.push_back(frame);
            }
            cursor.position() as usize
        };
        self.raw.drain(..consumed);

        Ok(true)
    }
}

// ── Connection ─────────────────────────────────────────────────

impl Connection {
    /// Establish a TCP connection and perform the NCD ControlHello handshake.
    pub(crate) async fn connect(addr: &str) -> Result<Self, Error> {
        let stream = TcpStream::connect(addr).await?;
        let peer_addr = stream.peer_addr()?;

        let conn = Connection {
            inner: Mutex::new(Inner {
                stream,
                raw: Vec::new(),
                frames: VecDeque::new(),
                leftover: Vec::new(),
            }),
            state: AtomicU8::new(ConnState::Connecting as u8),
            peer_addr,
        };

        // Phase 1: send ControlHello
        {
            let mut inner = conn.inner.lock().await;
            inner.send_packet(&Packet::ControlHello).await?;
            conn.state
                .store(ConnState::Handshaking as u8, Ordering::Release);
        }

        // Phase 2: wait for ControlHello reply
        {
            let mut inner = conn.inner.lock().await;
            loop {
                if let Some(packet) = read_packet(&mut inner.frames)? {
                    match packet {
                        Packet::ControlHello => break, // handshake complete
                        other => {
                            return Err(Error::HandshakeFailed(format!(
                                "expected ControlHello, got {:?}",
                                other
                            )));
                        }
                    }
                }

                if !inner.fill_frames().await? {
                    return Err(Error::HandshakeFailed(
                        "connection closed during handshake".into(),
                    ));
                }
            }
        }

        conn.state
            .store(ConnState::Connected as u8, Ordering::Release);
        Ok(conn)
    }

    /// Read data from the connection into `buf`.
    ///
    /// Returns the number of bytes copied. Handles control packets
    /// (Ping→Pong, Close, KeepAlive) transparently.
    pub(crate) async fn read_data(&self, buf: &mut [u8]) -> Result<usize, Error> {
        let mut inner = self.inner.lock().await;

        let current = self.state.load(Ordering::Acquire);
        if current != ConnState::Connected as u8 {
            return Err(Error::InvalidState {
                current: ConnState::from_u8(current),
                expected: "Connected",
            });
        }

        loop {
            // Serve leftover from a previous oversized read first.
            if !inner.leftover.is_empty() {
                let n = inner.leftover.len().min(buf.len());
                buf[..n].copy_from_slice(&inner.leftover[..n]);
                if n < inner.leftover.len() {
                    inner.leftover.drain(..n);
                } else {
                    inner.leftover.clear();
                }
                return Ok(n);
            }

            // Try to reassemble a complete packet.
            if let Some(packet) = read_packet(&mut inner.frames)? {
                match packet {
                    Packet::Data(payload) => {
                        let n = payload.len().min(buf.len());
                        buf[..n].copy_from_slice(&payload[..n]);
                        if n < payload.len() {
                            inner.leftover = payload[n..].to_vec();
                        }
                        return Ok(n);
                    }
                    Packet::ControlPing { id } => {
                        inner.send_packet(&Packet::ControlPong { id }).await?;
                        continue;
                    }
                    Packet::ControlClose => {
                        self.state
                            .store(ConnState::Closing as u8, Ordering::Release);
                        let _ = inner.send_packet(&Packet::ControlClose).await;
                        self.state.store(ConnState::Closed as u8, Ordering::Release);
                        return Err(Error::ConnectionClosed);
                    }
                    Packet::ControlKeepAlive => {
                        continue;
                    }
                    Packet::ControlHello | Packet::ControlPong { .. } => {
                        // Unexpected post-handshake; ignore for prototype.
                        continue;
                    }
                }
            }

            // Not enough data — read more from the stream.
            if !inner.fill_frames().await? {
                self.state.store(ConnState::Closed as u8, Ordering::Release);
                return Err(Error::ConnectionClosed);
            }
        }
    }

    /// Write `buf` as a Data packet to the connection.
    ///
    /// Returns the number of bytes accepted (always `buf.len()` on success).
    pub(crate) async fn write_data(&self, buf: &[u8]) -> Result<usize, Error> {
        let mut inner = self.inner.lock().await;

        let current = self.state.load(Ordering::Acquire);
        if current != ConnState::Connected as u8 {
            return Err(Error::InvalidState {
                current: ConnState::from_u8(current),
                expected: "Connected",
            });
        }

        inner.send_packet(&Packet::Data(buf.to_vec())).await?;
        Ok(buf.len())
    }

    /// Gracefully close the connection (send ControlClose + shutdown).
    pub(crate) async fn shutdown(&self) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;

        let current = self.state.load(Ordering::Acquire);
        if current == ConnState::Closed as u8 || current == ConnState::Closing as u8 {
            return Ok(());
        }

        self.state
            .store(ConnState::Closing as u8, Ordering::Release);

        if current == ConnState::Connected as u8 || current == ConnState::Handshaking as u8 {
            let _ = inner.send_packet(&Packet::ControlClose).await;
        }

        self.state.store(ConnState::Closed as u8, Ordering::Release);
        let _ = inner.stream.shutdown().await;

        Ok(())
    }

    pub(crate) fn state(&self) -> ConnState {
        ConnState::from_u8(self.state.load(Ordering::Acquire))
    }

    pub(crate) fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }
}
