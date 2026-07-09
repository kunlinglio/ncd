use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use bytes::{Buf, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::{select, time};

use libncd::frame::{self, Frame};
use libncd::packet::Packet;
use libncd::{frames_to_packet, packet_to_frames};

use crate::error::{ConnectionClosed, ConnectionCreateError, ConnectionError};

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ConnRole {
    /// Host endpoint of NCD protocol, listening for TCP connections
    Host,
    /// Device endpoint of NCD protocol, connecting to a remote host
    Device,
}

#[derive(Debug, PartialEq, Eq, Clone)]
#[repr(u8)]
pub enum ConnState {
    /// TCP connection established, waiting for handshaking
    Connecting,
    /// Handshake complete, connection is fully established
    Connected,
    /// Connection is closed, you can still read Packets from the buffer
    Closed,
}

#[derive(Debug, Clone)]
pub struct ConnStatus {
    pub state: ConnState,
    pub latest_rtt: Option<Duration>,
    pub peer_addr: SocketAddr,
    pub role: ConnRole,
}

struct IoState {
    stream: TcpStream,
    byte_buffer: BytesMut,
    frame_buffer: VecDeque<Frame>,
    packet_buffer: VecDeque<Packet>,
}

pub(crate) struct Connection {
    io: IoState,
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
    // Channels
    request_rx: Receiver<Request>,
    packet_response_tx: Sender<Packet>,
    status_response_tx: Sender<ConnStatus>,
}

impl IoState {
    async fn send_packet(&mut self, packet: &Packet) -> Result<(), ConnectionError> {
        let frames = packet_to_frames(packet);
        let mut buf = vec![];
        for frame in frames {
            let bytes = frame.encode();
            buf.extend_from_slice(&bytes);
        }
        self.stream
            .write_all(&buf)
            .await
            .map_err(ConnectionError::from)?;
        Ok(())
    }

    /// Return Ok(None) if EOF is reached, otherwise return Ok(Some(packet)) if a packet is received.
    async fn recv_packet(&mut self) -> Result<Option<Packet>, ConnectionError> {
        loop {
            if let Some(packet) = self.packet_buffer.pop_front() {
                return Ok(Some(packet));
            }
            // Read bytes: TCP stream -> byte_buffer
            let n = self
                .stream
                .read_buf(&mut self.byte_buffer)
                .await
                .map_err(ConnectionError::from)?;
            if n == 0 {
                return Ok(None);
            }

            // Try to decode frames: byte_buffer -> frame_buffer
            loop {
                let available = &self.byte_buffer[..];
                let Some((_, _, payload_len)) =
                    Frame::peek_head(available).map_err(ConnectionError::ProtocolError)?
                else {
                    break;
                };
                let frame_len = frame::HEADER_SIZE + payload_len;
                if available.len() < frame_len {
                    break;
                }
                let frame = Frame::decode(&available[..frame_len])
                    .map_err(ConnectionError::ProtocolError)?;
                self.frame_buffer.push_back(frame);
                self.byte_buffer.advance(frame_len);
            }

            // Try to assemble packet: frame_buffer -> Packet
            loop {
                // TODO: Optimize O(n) memory copy
                let contiguous = self.frame_buffer.make_contiguous();
                let Some(res) =
                    frames_to_packet(contiguous).map_err(ConnectionError::ProtocolError)?
                else {
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
        buffer_size: usize,
    ) -> (Self, ConnHandler) {
        stream
            .set_nodelay(true)
            .expect("Expect set nodelay for TCP stream");
        let (handler, request_rx, packet_response_tx, status_response_tx) =
            ConnHandler::new(buffer_size);
        (
            Connection {
                io: IoState {
                    stream,
                    byte_buffer: BytesMut::new(),
                    frame_buffer: VecDeque::new(),
                    packet_buffer: VecDeque::new(),
                },
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
                request_rx,
                packet_response_tx,
                status_response_tx,
            },
            handler,
        )
    }

    pub(crate) async fn connect(
        to_addr: IpAddr,
        to_port: u16,
        connect_timeout: Duration,
        close_timeout_ms: u32,
        timeout_ms: u32,
        keep_alive_factor: f32,
        buffer_size: usize,
    ) -> Result<ConnHandler, ConnectionCreateError> {
        let peer_addr = SocketAddr::new(to_addr, to_port);
        let stream = tokio::time::timeout(connect_timeout, TcpStream::connect(peer_addr))
            .await
            .map_err(|_| ConnectionCreateError::ConnectTimeout {
                addr: peer_addr,
                timeout: connect_timeout,
            })?
            .map_err(ConnectionCreateError::from)?;
        let (mut conn, mut handler) = Self::new(
            stream,
            peer_addr,
            timeout_ms,
            close_timeout_ms,
            keep_alive_factor,
            ConnRole::Device,
            buffer_size,
        );
        conn.handshake().await?;
        handler.task_handle = TaskState::Running(tokio::spawn(async move { conn.run().await }));
        Ok(handler)
    }

    pub(crate) async fn listen(
        on_addr: IpAddr,
        on_port: u16,
        timeout_ms: u32,
        close_timeout_ms: u32,
        keep_alive_factor: f32,
        buffer_size: usize,
    ) -> Result<ConnHandler, ConnectionCreateError> {
        let listen_addr = SocketAddr::new(on_addr, on_port);
        let stream = tokio::net::TcpListener::bind(listen_addr)
            .await
            .map_err(ConnectionCreateError::from)?
            .accept()
            .await
            .map_err(ConnectionCreateError::from)?
            .0;
        let peer_addr = stream.peer_addr().map_err(ConnectionCreateError::from)?;
        let (mut conn, mut handler) = Self::new(
            stream,
            peer_addr,
            timeout_ms,
            close_timeout_ms,
            keep_alive_factor,
            ConnRole::Host,
            buffer_size,
        );
        conn.handshake().await?;
        handler.task_handle = TaskState::Running(tokio::spawn(async move { conn.run().await }));
        Ok(handler)
    }

    /// Main event loop for this connection
    async fn run(&mut self) -> Result<(), ConnectionError> {
        while self.state == ConnState::Connected {
            select! {
                // Handle timers
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
                    return Err(ConnectionError::PeerInactiveTimeout);
                }

                // Handle tcp stream events
                packet_res = self.io.recv_packet() => {
                    let packet = packet_res?.ok_or(ConnectionError::UnexpectedEOF)?;
                    self.handle_packet(packet).await?;
                }

                // Handle requests from user (channel)
                Some(request) = self.request_rx.recv() => {
                    match request {
                        Request::Close => {
                            self.close_inner(false).await?;
                            break;
                        }
                        Request::Send(packet) => {
                            self.io.send_packet(&packet).await?;
                        }
                        Request::GetStatus => {
                            let status = ConnStatus {
                                state: self.state.clone(),
                                latest_rtt: self.latest_rtt,
                                peer_addr: self.peer_addr,
                                role: self.role.clone(),
                            };
                            self.status_response_tx.send(status).await.expect("Status response channel should not be closed");
                        }
                    }
                }
            }
        }
        assert_eq!(self.state, ConnState::Closed);
        Ok(())
    }

    /// Send hello and announce timeout duration
    /// - Device endpoint: sends ControlHello with keep_alive_interval_ms
    /// - Host endpoint: sends ControlHello with keep_alive_interval_ms
    pub(crate) async fn handshake(&mut self) -> Result<(), ConnectionCreateError> {
        let keep_alive_interval_ms = (self.timeout_ms as f32 / self.keep_alive_factor) as u32;
        let peer_keep_alive_interval = match self.role {
            ConnRole::Device => {
                // Device need to send ControlHello
                self.io
                    .send_packet(&Packet::ControlHello {
                        keep_alive_interval_ms,
                    })
                    .await
                    .map_err(ConnectionCreateError::from)?;
                // Wait for ControlHelloAck from Host
                let packet = self
                    .io
                    .recv_packet()
                    .await?
                    .ok_or(ConnectionError::UnexpectedEOF)?;
                let Packet::ControlHelloAck {
                    keep_alive_interval_ms: peer_keep_alive_interval,
                } = packet
                else {
                    return Err(ConnectionCreateError::HandshakeError(
                        "Expected ControlHello packet, received different packet".into(),
                    ));
                };
                peer_keep_alive_interval
            }
            ConnRole::Host => {
                // Host need to wait for ControlHello from Device
                let packet = self
                    .io
                    .recv_packet()
                    .await?
                    .ok_or(ConnectionError::UnexpectedEOF)?;
                let Packet::ControlHello {
                    keep_alive_interval_ms: peer_keep_alive_interval,
                } = packet
                else {
                    return Err(ConnectionCreateError::HandshakeError(
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
        // Init timers.
        self.keep_alive_timer = Some(time::interval(Duration::from_millis(
            peer_keep_alive_interval as u64,
        )));
        self.peer_query_active_timer = Some(time::interval(Duration::from_millis(
            (keep_alive_interval_ms + self.timeout_ms) as u64 / 2,
        )));
        self.peer_active_timeout_timer = Some(time::interval(Duration::from_millis(
            self.timeout_ms as u64,
        )));
        // reset() each one so the first tick fires after the
        // interval, not immediately (tokio Interval defaults to immediate
        // first tick, which would close the connection instantly).
        self.keep_alive_timer.as_mut().unwrap().reset();
        self.peer_query_active_timer.as_mut().unwrap().reset();
        self.peer_active_timeout_timer.as_mut().unwrap().reset();
        self.switch_to(ConnState::Connected);
        Ok(())
    }

    async fn handle_packet(&mut self, packet: Packet) -> Result<(), ConnectionError> {
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
                return Err(ConnectionError::PeerClosed);
            }
            Packet::Data(data) => {
                self.handle_data_packet(data).await;
            }
            Packet::ControlHello { .. } | Packet::ControlHelloAck { .. } => {
                unreachable!("ControlHello should only be received during handshake");
            }
        }
        Ok(())
    }

    async fn handle_data_packet(&mut self, data: Vec<u8>) {
        self.packet_response_tx
            .send(Packet::Data(data))
            .await
            .expect("Packet response channel should not be closed");
    }

    async fn close_inner(&mut self, peer_closed: bool) -> Result<(), ConnectionError> {
        assert_eq!(self.state, ConnState::Connected);
        if peer_closed {
            // Peer closed
            // Assume that peer has no more data to send
            // We should send all data from channel
            loop {
                match self.request_rx.try_recv() {
                    Ok(Request::Send(packet)) => {
                        self.io.send_packet(&packet).await?;
                    }
                    Ok(Request::Close) => {
                        // Peer has already closed, we can ignore this request
                        break;
                    }
                    Ok(Request::GetStatus) => {
                        let _ = self.status_response_tx.try_send(ConnStatus {
                            state: self.state.clone(),
                            latest_rtt: self.latest_rtt,
                            peer_addr: self.peer_addr,
                            role: self.role.clone(),
                        });
                    }
                    Err(_) => break,
                }
            }
            self.io.send_packet(&Packet::ControlClose).await?;
            self.io
                .stream
                .flush()
                .await
                .map_err(ConnectionError::from)?;
            self.io
                .stream
                .shutdown()
                .await
                .map_err(ConnectionError::from)?;
            // TODO: Replace this guard with a more graceful check
            let buf = &mut [0u8; 8];
            let remaining = self.io.stream.read(buf).await?;
            if remaining == 0 {
                self.switch_to(ConnState::Closed);
            } else {
                panic!("Peer sent data after ControlClose, which is a protocol violation");
            }
        } else {
            // We initiated the close, so we should not have more data to send
            self.io.send_packet(&Packet::ControlClose).await?;
            self.io
                .stream
                .flush()
                .await
                .map_err(ConnectionError::from)?;
            self.io
                .stream
                .shutdown()
                .await
                .map_err(ConnectionError::from)?;
            time::timeout(
                time::Duration::from_millis(self.close_timeout_ms as u64),
                async {
                    loop {
                        let res = self.io.recv_packet().await;
                        match res {
                            Ok(Some(Packet::ControlClose)) => break, // received ControlClose assumed peer has send all the data
                            Ok(Some(Packet::Data(data))) => {
                                self.handle_data_packet(data).await;
                                continue;
                            }
                            Ok(Some(_)) => continue,
                            Ok(None) => return Err(ConnectionError::UnexpectedEOF),
                            Err(_) => break,
                        }
                    }
                    Ok(())
                },
            )
            .await
            .map_err(|_| ConnectionError::CloseTimeout)??;
            self.switch_to(ConnState::Closed);
        }
        Ok(())
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

#[derive(Debug)]
pub enum Request {
    Close,
    Send(Packet),
    GetStatus,
}

pub struct ConnHandler {
    request_tx: mpsc::Sender<Request>,
    packet_response_rx: mpsc::Receiver<Packet>,
    status_response_rx: mpsc::Receiver<ConnStatus>,
    task_handle: TaskState,
}

impl Drop for ConnHandler {
    fn drop(&mut self) {
        #[cfg(not(test))]
        {
            // TODO: Maybe we should handle drop more gracefully, but for now, we want to ensure that users explicitly call close() before dropping the handler.
            if !matches!(self.task_handle, TaskState::Finished(_)) {
                panic!(
                    "ConnHandler should be closed explicitly by calling close() before dropping"
                );
            }
        }
    }
}

/// TODO: Here we check task state only when user call methods, we may need to check task state in background and notify user when task is finished
enum TaskState {
    Uninitialized,
    Running(tokio::task::JoinHandle<Result<(), ConnectionError>>),
    Finished(Result<(), ConnectionError>),
}

impl ConnHandler {
    fn new(
        buffer_size: usize,
    ) -> (
        Self,
        mpsc::Receiver<Request>,
        mpsc::Sender<Packet>,
        mpsc::Sender<ConnStatus>,
    ) {
        let (request_tx, request_rx) = mpsc::channel(buffer_size);
        let (packet_response_tx, packet_response_rx) = mpsc::channel(buffer_size);
        let (status_response_tx, status_response_rx) = mpsc::channel(buffer_size);
        (
            ConnHandler {
                request_tx,
                packet_response_rx,
                status_response_rx,
                task_handle: TaskState::Uninitialized,
            },
            request_rx,
            packet_response_tx,
            status_response_tx,
        )
    }

    async fn check_task_state(&mut self) -> Option<ConnectionClosed> {
        match &mut self.task_handle {
            TaskState::Uninitialized => {
                unreachable!("Task should be initialized before calling check_task_state")
            }
            TaskState::Running(handle) => {
                if handle.is_finished() {
                    let res = handle.await.expect("Join should not be failed");
                    self.task_handle = TaskState::Finished(res.clone());
                    match res {
                        Ok(()) => Some(ConnectionClosed::Normal),
                        Err(e) => Some(ConnectionClosed::Error(e.clone())),
                    }
                } else {
                    None
                }
            }
            TaskState::Finished(res) => match res {
                Ok(()) => Some(ConnectionClosed::Normal),
                Err(e) => Some(ConnectionClosed::Error(e.clone())),
            },
        }
    }

    pub(crate) async fn write(&mut self, bytes: &[u8]) -> Result<(), ConnectionClosed> {
        let state = self.check_task_state().await;
        if let Some(closed) = state {
            return Err(closed);
        }
        let packet = Packet::Data(bytes.to_vec());
        self.request_tx
            .send(Request::Send(packet))
            .await
            .map_err(|e| ConnectionClosed::Unknown(e.to_string()))
    }

    pub(crate) async fn read(&mut self) -> Result<Vec<u8>, ConnectionClosed> {
        match self.packet_response_rx.recv().await {
            Some(packet) => match packet {
                Packet::Data(data) => Ok(data),
                _ => unreachable!("Only Data packets should be sent to packet_response_rx"),
            },
            None => Err(self.check_task_state().await.expect("Task should finished")),
        }
    }

    pub(crate) async fn get_status(&mut self) -> Result<ConnStatus, ConnectionClosed> {
        let state = self.check_task_state().await;
        if let Some(closed) = state {
            return Err(closed);
        }
        self.request_tx
            .send(Request::GetStatus)
            .await
            .map_err(|e| ConnectionClosed::Unknown(e.to_string()))?;
        match self.status_response_rx.recv().await {
            Some(status) => Ok(status),
            None => Err(self.check_task_state().await.expect("Task should finished")),
        }
    }

    pub(crate) async fn close(&mut self) -> Result<Result<(), ConnectionError>, ConnectionClosed> {
        let state = self.check_task_state().await;
        if let Some(closed) = state {
            return Err(closed);
        }
        self.request_tx
            .send(Request::Close)
            .await
            .map_err(|e| ConnectionClosed::Unknown(e.to_string()))?;
        // await task to finish
        match &mut self.task_handle {
            TaskState::Running(handle) => {
                let res = handle.await.expect("Join should not be failed");
                self.task_handle = TaskState::Finished(res.clone());
                Ok(res)
            }
            TaskState::Finished(res) => match res {
                Ok(()) => Ok(Ok(())),
                Err(e) => Ok(Err(e.clone())),
            },
            TaskState::Uninitialized => {
                unreachable!("Task should be initialized before calling close")
            }
        }
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

    /// Helper: spin up a host (accept + handshake), manually connect device
    /// (TCP connect + handshake).  Returns raw `Connection` objects for
    /// internal / white‑box tests.  Discards the `ConnHandler` half.
    async fn connected_pair() -> (Connection, Connection) {
        let (timeout_ms, close_timeout_ms, keep_alive_factor) = test_timeouts();
        let addr = IpAddr::V4(Ipv4Addr::LOCALHOST);

        let listener = tokio::net::TcpListener::bind(SocketAddr::new(addr, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();

        // Host: accept, new(), handshake
        let host = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            let (mut conn, _handler) = Connection::new(
                stream,
                peer,
                timeout_ms,
                close_timeout_ms,
                keep_alive_factor,
                ConnRole::Host,
                100,
            );
            conn.handshake().await.unwrap();
            conn
        });

        // Device: manual TCP connect + handshake (bypasses connect() so we
        // still get the raw Connection, not ConnHandler).
        let stream = TcpStream::connect(SocketAddr::new(addr, port))
            .await
            .unwrap();
        let peer_addr = stream.peer_addr().unwrap();
        let (mut device, _handler) = Connection::new(
            stream,
            peer_addr,
            timeout_ms,
            close_timeout_ms,
            keep_alive_factor,
            ConnRole::Device,
            100,
        );
        device.handshake().await.unwrap();

        let host = host.await.unwrap();
        (device, host)
    }

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

    #[tokio::test]
    async fn ping_pong_roundtrip() {
        let (mut device, mut host) = connected_pair().await;

        let ping_id = 42u32;
        device
            .io
            .send_packet(&Packet::ControlPing { id: ping_id })
            .await
            .unwrap();

        let ping = host.io.recv_packet().await.unwrap();
        assert_eq!(ping, Some(Packet::ControlPing { id: ping_id }));

        host.handle_packet(ping.unwrap()).await.unwrap();

        let pong = device.io.recv_packet().await.unwrap();
        assert_eq!(pong, Some(Packet::ControlPong { id: ping_id }));
    }

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

    /// Host initiates close, device receives ControlClose and ack‑closes.
    #[tokio::test]
    async fn peer_initiated_close() {
        let (mut device, mut host) = connected_pair().await;

        let host_closed = tokio::spawn(async move {
            host.state = ConnState::Connected;
            host.close_inner(false).await.unwrap();
            host
        });

        let received = device.io.recv_packet().await.unwrap();
        assert_eq!(received, Some(Packet::ControlClose));

        // handle_packet for ControlClose calls close_inner(true) then
        // returns Err(PeerClosed) — this is expected.
        let result = device.handle_packet(Packet::ControlClose).await;
        assert!(matches!(result, Err(ConnectionError::PeerClosed)));
        assert_eq!(device.state, ConnState::Closed);

        let host = host_closed.await.unwrap();
        assert_eq!(host.state, ConnState::Closed);
    }

    /// Device initiates close, host receives ControlClose and responds.
    #[tokio::test]
    async fn device_initiated_close() {
        let (mut device, mut host) = connected_pair().await;

        let device_closed = tokio::spawn(async move {
            device.close_inner(false).await.unwrap();
            device
        });

        let received = host.io.recv_packet().await.unwrap();
        assert_eq!(received, Some(Packet::ControlClose));

        let result = host.handle_packet(Packet::ControlClose).await;
        assert!(matches!(result, Err(ConnectionError::PeerClosed)));
        assert_eq!(host.state, ConnState::Closed);

        let device = device_closed.await.unwrap();
        assert_eq!(device.state, ConnState::Closed);
    }

    /// ConnStatus reflects the real connection state.
    #[tokio::test]
    async fn status_reflects_connection() {
        let (device, host) = connected_pair().await;

        assert_eq!(device.role, ConnRole::Device);
        assert_eq!(host.role, ConnRole::Host);
        assert_eq!(device.state, ConnState::Connected);
        assert_eq!(host.state, ConnState::Connected);
        assert_eq!(device.peer_addr.ip(), host.peer_addr.ip()); // both on localhost
        assert_ne!(device.peer_addr.port(), host.peer_addr.port()); // device connected from ephemeral port
    }

    /// Recv returns None when the peer closes the TCP connection abruptly
    /// (no ControlClose, just TCP FIN).
    #[tokio::test]
    async fn recv_returns_none_on_eof() {
        let (mut device, _host) = connected_pair().await;

        // Drop the host — its TcpStream is closed, device will see EOF
        drop(_host);

        // The device should eventually see EOF on its recv side
        // (the host dropped → kernel sends FIN → device reads 0 bytes)
        let result = device.io.recv_packet().await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
    }
}
