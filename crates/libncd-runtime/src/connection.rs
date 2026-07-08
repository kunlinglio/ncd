use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use bytes::{Buf, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::{select, time};

use libncd::frame::{self, Frame};
use libncd::packet::Packet;
use libncd::{frames_to_packet, packet_to_frames};

use crate::error::Error::{self, HandshakeError};

#[derive(Debug, PartialEq, Eq)]
pub enum ConnRole {
    /// Host endpoint of NCD protocol, listening for TCP connections
    Host,
    /// Device endpoint of NCD protocol, connecting to a remote host
    Device,
}

#[derive(Debug, PartialEq, Eq)]
#[repr(u8)]
enum ConnState {
    /// TCP connection established, waiting for handshaking
    Connecting,
    /// Handshake complete, connection is fully established
    Connected,
    /// Connection is closed, you can still read Packets from the buffer
    Closed,
}

/// IO half of a connection — stream + decode buffers.
/// Extracted into a sub‑struct so `select!` branches can borrow `self.io`
/// and `self.xxx_timer` independently without conflicting on `&mut self`.
struct IoState {
    stream: TcpStream,
    byte_buffer: BytesMut,
    frame_buffer: VecDeque<Frame>,
    packet_buffer: VecDeque<Packet>,
}
pub struct Connection {
    io: IoState,
    packets_for_read: VecDeque<Packet>,
    // Timers
    peer_query_active_timer: Option<time::Interval>,
    peer_active_timeout_timer: Option<time::Interval>,
    keep_alive_timer: Option<time::Interval>, // Timer for sending ControlKeepAlive packets
    ping_timers: HashMap<u32, Instant>,
    // Connect statistics
    latest_rtt: Option<Duration>,
    // Config items
    timeout_ms: u32,
    keep_alive_factor: f32,
    close_timeout_ms: u32,

    state: ConnState,
    peer_addr: SocketAddr,
    role: ConnRole,
}

impl IoState {
    async fn send_packet(&mut self, packet: &Packet) -> Result<(), Error> {
        let frames = packet_to_frames(packet);
        let mut buf = vec![];
        for frame in frames {
            let bytes = frame.encode();
            buf.extend_from_slice(&bytes);
        }
        self.stream.write_all(&buf).await.map_err(Error::from)?;
        Ok(())
    }

    /// Return Ok(None) if EOF is reached, otherwise return Ok(Some(packet)) if a packet is received.
    async fn recv_packet(&mut self) -> Result<Option<Packet>, Error> {
        loop {
            if let Some(packet) = self.packet_buffer.pop_front() {
                return Ok(Some(packet));
            }
            // Read bytes: TCP stream -> byte_buffer
            let n = self
                .stream
                .read_buf(&mut self.byte_buffer)
                .await
                .map_err(Error::from)?;
            if n == 0 {
                return Ok(None);
            }

            // Try to decode frames: byte_buffer -> frame_buffer
            loop {
                let available = &self.byte_buffer[..];
                let Some((_, _, payload_len)) =
                    Frame::peek_head(available).map_err(Error::ProtocolError)?
                else {
                    break;
                };
                let frame_len = frame::HEADER_SIZE + payload_len;
                if available.len() < frame_len {
                    break;
                }
                let frame = Frame::decode(&available[..frame_len]).map_err(Error::ProtocolError)?;
                self.frame_buffer.push_back(frame);
                self.byte_buffer.advance(frame_len);
            }

            // Try to assemble packet: frame_buffer -> Packet
            loop {
                // TODO: Optimize O(n) memory copy
                let contiguous = self.frame_buffer.make_contiguous();
                let Some(res) = frames_to_packet(contiguous).map_err(Error::ProtocolError)? else {
                    break;
                };
                let (consumed, packet) = res;
                self.frame_buffer.drain(..consumed);
                self.packet_buffer.push_back(packet);
            }
        }
    }
}

impl Connection {
    pub(crate) fn new(
        stream: TcpStream,
        peer_addr: SocketAddr,
        timeout_ms: u32,
        close_timeout_ms: u32,
        keep_alive_factor: f32,
        role: ConnRole,
    ) -> Self {
        stream
            .set_nodelay(true)
            .expect("Expect set nodelay for TCP stream");
        Connection {
            io: IoState {
                stream,
                byte_buffer: BytesMut::new(),
                frame_buffer: VecDeque::new(),
                packet_buffer: VecDeque::new(),
            },
            packets_for_read: VecDeque::new(),
            peer_query_active_timer: None,
            peer_active_timeout_timer: None,
            keep_alive_timer: None,
            ping_timers: HashMap::new(),
            latest_rtt: None,
            timeout_ms,
            keep_alive_factor,
            close_timeout_ms,
            state: ConnState::Connecting,
            peer_addr,
            role,
        }
    }

    pub async fn connect(
        to_addr: IpAddr,
        to_port: u16,
        connect_timeout: Duration,
        close_timeout_ms: u32,
        timeout_ms: u32,
        keep_alive_factor: f32,
    ) -> Result<Self, Error> {
        let peer_addr = SocketAddr::new(to_addr, to_port);
        let stream = tokio::time::timeout(connect_timeout, TcpStream::connect(peer_addr))
            .await
            .map_err(|_| Error::ConnectTimeout {
                addr: peer_addr,
                timeout: connect_timeout,
            })?
            .map_err(Error::from)?;
        let mut conn = Self::new(
            stream,
            peer_addr,
            timeout_ms,
            close_timeout_ms,
            keep_alive_factor,
            ConnRole::Device,
        );
        conn.handshake().await?;
        Ok(conn)
    }

    pub async fn listen(
        on_addr: IpAddr,
        on_port: u16,
        timeout_ms: u32,
        close_timeout_ms: u32,
        keep_alive_factor: f32,
    ) -> Result<Self, Error> {
        let listen_addr = SocketAddr::new(on_addr, on_port);
        let stream = tokio::net::TcpListener::bind(listen_addr)
            .await
            .map_err(Error::from)?
            .accept()
            .await
            .map_err(Error::from)?
            .0;
        let peer_addr = stream.peer_addr().map_err(Error::from)?;
        let mut conn = Self::new(
            stream,
            peer_addr,
            timeout_ms,
            close_timeout_ms,
            keep_alive_factor,
            ConnRole::Host,
        );
        conn.handshake().await?;
        Ok(conn)
    }

    /// Main event loop for this connection
    pub async fn run(&mut self) -> Result<(), Error> {
        while self.state == ConnState::Connected {
            select! {
                Some(ref mut timer) = async { self.keep_alive_timer.as_mut() }, if self.keep_alive_timer.is_some() => {
                    timer.tick().await;
                    self.io.send_packet(&Packet::ControlKeepAlive).await?;
                }

                Some(ref mut timer) = async { self.peer_query_active_timer.as_mut() }, if self.peer_query_active_timer.is_some() => {
                    timer.tick().await;
                    let id = rand::random::<u32>();
                    self.ping_timers.insert(id, Instant::now());
                    self.io.send_packet(&Packet::ControlPing { id }).await?;
                }

                Some(ref mut timer) = async { self.peer_active_timeout_timer.as_mut() }, if self.peer_active_timeout_timer.is_some() => {
                    timer.tick().await;
                    // TODO: Implement retry logic
                    self.close_inner(false).await?;
                    return Err(Error::PeerInactiveTimeout);
                }

                packet_res = self.io.recv_packet() => {
                    let packet = packet_res?.ok_or(Error::UnexpectedEOF)?;
                    self.handle_packet(packet).await?;
                }
            }
        }
        Ok(())
    }

    /// Send hello and announce timeout duration
    /// - Device endpoint: sends ControlHello with keep_alive_interval_ms
    /// - Host endpoint: sends ControlHello with keep_alive_interval_ms
    async fn handshake(&mut self) -> Result<(), Error> {
        let keep_alive_interval_ms = (self.timeout_ms as f32 / self.keep_alive_factor) as u32;
        let peer_keep_alive_interval = match self.role {
            ConnRole::Device => {
                // Device need to send ControlHello
                self.io
                    .send_packet(&Packet::ControlHello {
                        keep_alive_interval_ms,
                    })
                    .await?;
                // Wait for ControlHelloAck from Host
                let packet = self.io.recv_packet().await?.ok_or(Error::UnexpectedEOF)?;
                let Packet::ControlHelloAck {
                    keep_alive_interval_ms: peer_keep_alive_interval,
                } = packet
                else {
                    return Err(HandshakeError(
                        "Expected ControlHello packet, received different packet".into(),
                    ));
                };
                peer_keep_alive_interval
            }
            ConnRole::Host => {
                // Host need to wait for ControlHello from Device
                let packet = self.io.recv_packet().await?.ok_or(Error::UnexpectedEOF)?;
                let Packet::ControlHello {
                    keep_alive_interval_ms: peer_keep_alive_interval,
                } = packet
                else {
                    return Err(HandshakeError(
                        "Expected ControlHello packet, received different packet".into(),
                    ));
                };
                // Send ControlHelloAck back to Device
                self.io
                    .send_packet(&Packet::ControlHelloAck {
                        keep_alive_interval_ms,
                    })
                    .await?;
                peer_keep_alive_interval
            }
        };
        // Init timers
        self.keep_alive_timer = Some(time::interval(Duration::from_millis(
            peer_keep_alive_interval as u64,
        )));
        self.peer_query_active_timer = Some(time::interval(Duration::from_millis(
            (keep_alive_interval_ms + self.timeout_ms) as u64 / 2,
        )));
        self.peer_active_timeout_timer = Some(time::interval(Duration::from_millis(
            self.timeout_ms as u64,
        )));
        self.switch_to(ConnState::Connected);
        Ok(())
    }

    async fn handle_packet(&mut self, packet: Packet) -> Result<(), Error> {
        self.peer_active_timeout_timer
            .as_mut()
            .map(|timer| timer.reset());
        self.peer_query_active_timer
            .as_mut()
            .map(|timer| timer.reset());
        match packet {
            Packet::ControlPing { id } => {
                self.io.send_packet(&Packet::ControlPong { id }).await?;
            }
            Packet::ControlPong { id } => {
                if let Some(sent_time) = self.ping_timers.remove(&id) {
                    let rtt = sent_time.elapsed();
                    self.latest_rtt = Some(rtt);
                }
            }
            Packet::ControlKeepAlive => {
                // Handled before the match
                // pass
            }
            Packet::ControlClose => {
                self.close_inner(true).await?;
            }
            Packet::Data(data) => {
                self.handle_data_packet(data);
            }
            Packet::ControlHello { .. } | Packet::ControlHelloAck { .. } => {
                unreachable!("ControlHello should only be received during handshake");
            }
        }
        Ok(())
    }

    fn handle_data_packet(&mut self, data: Vec<u8>) {
        // TODO: Implement some throttling mechanism to avoid unbounded memory growth
        self.packets_for_read.push_back(Packet::Data(data));
    }

    async fn close_inner(&mut self, peer_closed: bool) -> Result<(), Error> {
        assert_eq!(self.state, ConnState::Connected);
        if peer_closed {
            // Peer closed
            // Assume that peer has no more data to send
            self.io.send_packet(&Packet::ControlClose).await?;
            self.io.stream.flush().await.map_err(Error::from)?;
            self.io.stream.shutdown().await.map_err(Error::from)?;
            // TODO: Replace this guard with a more graceful check
            let buf = &mut [0u8; 8];
            let remaining = self.io.stream.read(buf).await?;
            if remaining == 0 {
                self.switch_to(ConnState::Closed);
            } else {
                panic!("Peer sent data after ControlClose, which is a protocol violation");
            }
        } else {
            self.io.send_packet(&Packet::ControlClose).await?;
            self.io.stream.flush().await.map_err(Error::from)?;
            self.io.stream.shutdown().await.map_err(Error::from)?;
            time::timeout(
                time::Duration::from_millis(self.close_timeout_ms as u64),
                async {
                    loop {
                        let res = self.io.recv_packet().await;
                        match res {
                            Ok(Some(Packet::ControlClose)) => break, // received ControlClose assumed peer has send all the data
                            Ok(Some(Packet::Data(data))) => {
                                self.handle_data_packet(data);
                                continue;
                            }
                            Ok(Some(_)) => continue,
                            Ok(None) => return Err(Error::UnexpectedEOF),
                            Err(_) => break,
                        }
                    }
                    Ok(())
                },
            )
            .await
            .map_err(|_| Error::CloseTimeout)??;
            self.switch_to(ConnState::Closed);
        }
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.state == ConnState::Connected
    }

    fn switch_to(&mut self, state: ConnState) {
        match state {
            ConnState::Connecting => {
                unreachable!(
                    "switch_to(Connecting) should never be called since its the default state"
                );
            }
            ConnState::Connected => {
                assert!(self.keep_alive_timer.is_some());
                assert!(self.peer_query_active_timer.is_some());
                assert!(self.peer_active_timeout_timer.is_some());
            }
            ConnState::Closed => {
                self.keep_alive_timer = None;
                self.peer_query_active_timer = None;
                self.peer_active_timeout_timer = None;
            }
        }
        self.state = state;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn test_timeouts() -> (u32, u32, f32) {
        (
            5000, // timeout_ms
            2000, // close_timeout_ms
            2.0,  // keep_alive_factor
        )
    }

    /// Device connects to Host on localhost, both complete handshake,
    /// then verify both ends are Connected with correct roles.
    #[tokio::test]
    async fn loopback_handshake_device_to_host() {
        let (timeout_ms, close_timeout_ms, keep_alive_factor) = test_timeouts();
        let addr = IpAddr::V4(Ipv4Addr::LOCALHOST);

        // Bind with port 0 so the OS assigns a free one
        let listener = tokio::net::TcpListener::bind(SocketAddr::new(addr, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();

        // Spawn host: accept one connection, handshake
        let host = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            let mut conn = Connection::new(
                stream,
                peer,
                timeout_ms,
                close_timeout_ms,
                keep_alive_factor,
                ConnRole::Host,
            );
            conn.handshake().await.unwrap();
            conn
        });

        // Device connects to the host
        let device = Connection::connect(
            addr,
            port,
            Duration::from_secs(3),
            close_timeout_ms,
            timeout_ms,
            keep_alive_factor,
        )
        .await
        .unwrap();

        let host = host.await.unwrap();

        assert_eq!(device.state, ConnState::Connected);
        assert!(device.keep_alive_timer.is_some());
        assert_eq!(host.state, ConnState::Connected);
        assert_eq!(device.role, ConnRole::Device);
        assert_eq!(host.role, ConnRole::Host);
        assert_eq!(device.peer_addr.port(), port);
    }

    /// Helper: spin up a host, let device connect, handshake both sides.
    async fn connected_pair() -> (Connection, Connection) {
        let (timeout_ms, close_timeout_ms, keep_alive_factor) = test_timeouts();
        let addr = IpAddr::V4(Ipv4Addr::LOCALHOST);

        let listener = tokio::net::TcpListener::bind(SocketAddr::new(addr, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();

        let host = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            let mut conn = Connection::new(
                stream,
                peer,
                timeout_ms,
                close_timeout_ms,
                keep_alive_factor,
                ConnRole::Host,
            );
            conn.handshake().await.unwrap();
            conn
        });

        let device = Connection::connect(
            addr,
            port,
            Duration::from_secs(3),
            close_timeout_ms,
            timeout_ms,
            keep_alive_factor,
        )
        .await
        .unwrap();

        let host = host.await.unwrap();
        (device, host)
    }

    /// Device sends a Data packet, host receives it and reads the payload.
    #[tokio::test]
    async fn send_recv_data_packet() {
        let (mut device, mut host) = connected_pair().await;

        let payload = b"hello ncd";
        device
            .io
            .send_packet(&Packet::Data(payload.to_vec()))
            .await
            .unwrap();

        let received = host.io.recv_packet().await.unwrap();
        assert_eq!(received, Some(Packet::Data(payload.to_vec())));
    }

    /// Device sends a Ping, host handles it and replies with Pong.
    #[tokio::test]
    async fn ping_pong_roundtrip() {
        let (mut device, mut host) = connected_pair().await;

        let ping_id = 42u32;
        device
            .io
            .send_packet(&Packet::ControlPing { id: ping_id })
            .await
            .unwrap();

        // Host receives the ping
        let ping = host.io.recv_packet().await.unwrap();
        assert_eq!(ping, Some(Packet::ControlPing { id: ping_id }));

        // Host handles it (should reply with Pong)
        host.handle_packet(ping.unwrap()).await.unwrap();

        // Device receives the pong
        let pong = device.io.recv_packet().await.unwrap();
        assert_eq!(pong, Some(Packet::ControlPong { id: ping_id }));
    }

    /// Host sends multiple Data packets, device receives them in order.
    #[tokio::test]
    async fn multiple_packets_in_order() {
        let (mut device, mut host) = connected_pair().await;

        let packets: Vec<Packet> = (0..5)
            .map(|i| Packet::Data(format!("msg-{}", i).into_bytes()))
            .collect();

        for pkt in &packets {
            host.io.send_packet(pkt).await.unwrap();
        }

        for expected in &packets {
            let received = device.io.recv_packet().await.unwrap();
            assert_eq!(received.as_ref(), Some(expected));
        }
    }

    /// Device sends ControlKeepAlive, host receives it (no reply expected).
    #[tokio::test]
    async fn keepalive_is_received() {
        let (mut device, mut host) = connected_pair().await;

        device
            .io
            .send_packet(&Packet::ControlKeepAlive)
            .await
            .unwrap();

        let received = host.io.recv_packet().await.unwrap();
        assert_eq!(received, Some(Packet::ControlKeepAlive));
    }

    /// Host initiates close, device receives ControlClose and ack-closes.
    #[tokio::test]
    async fn peer_initiated_close() {
        let (mut device, mut host) = connected_pair().await;

        // Spawn host close: it will send ControlClose then wait for the
        // device's ack.  Must be spawned so the main thread can drive device.
        let host_closed = tokio::spawn(async move {
            host.state = ConnState::Connected;
            host.close_inner(false).await.unwrap();
            host
        });

        // Device receives ControlClose from the host
        let received = device.io.recv_packet().await.unwrap();
        assert_eq!(received, Some(Packet::ControlClose));

        // Device handles the close — sends ControlClose back to host
        device.handle_packet(Packet::ControlClose).await.unwrap();

        let host = host_closed.await.unwrap();
        assert_eq!(host.state, ConnState::Closed);
        assert_eq!(device.state, ConnState::Closed);
    }
}
