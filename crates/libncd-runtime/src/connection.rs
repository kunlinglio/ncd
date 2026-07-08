use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use bytes::{Buf, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use libncd::frame::{self, Frame};
use libncd::packet::Packet;
use libncd::{frames_to_packet, packet_to_frames};

use crate::error::Error;

pub enum ConnRole {
    /// Host endpoint of NCD protocol, listening for TCP connections
    Host,
    /// Device endpoint of NCD protocol, connecting to a remote host
    Device,
}

#[repr(u8)]
enum ConnState {
    /// TCP connection established, waiting for handshaking
    Connecting,
    /// Handshake complete, connection is fully established
    Connected,
    /// Stop reading/writing, send ControlClose and shutdown the TCP stream
    Closing,
    /// Connection is closed, no further operations are allowed
    Closed,
}

pub struct Connection {
    stream: TcpStream,
    byte_buffer: BytesMut,
    frame_buffer: VecDeque<Frame>,
    packet_buffer: VecDeque<Packet>,
    state: ConnState,
    peer_addr: SocketAddr,
    role: ConnRole,
}

impl Connection {
    pub async fn connect(
        to_addr: IpAddr,
        to_port: u16,
        connect_timeout: Duration,
    ) -> Result<Self, Error> {
        let peer_addr = SocketAddr::new(to_addr, to_port);
        let stream = tokio::time::timeout(connect_timeout, TcpStream::connect(peer_addr))
            .await
            .map_err(|_| Error::ConnectTimeout {
                addr: peer_addr,
                timeout: connect_timeout,
            })?
            .map_err(Error::from)?;
        let mut conn = Self {
            stream,
            byte_buffer: BytesMut::new(),
            frame_buffer: VecDeque::new(),
            packet_buffer: VecDeque::new(),
            state: ConnState::Connecting,
            peer_addr,
            role: ConnRole::Device,
        };
        conn.handshake().await?;
        Ok(conn)
    }

    pub async fn listen(on_addr: IpAddr, on_port: u16) -> Result<Self, Error> {
        let peer_addr = SocketAddr::new(on_addr, on_port);
        let stream = tokio::net::TcpListener::bind(peer_addr)
            .await
            .map_err(Error::from)?
            .accept()
            .await
            .map_err(Error::from)?
            .0;
        let mut conn = Self {
            stream,
            byte_buffer: BytesMut::new(),
            frame_buffer: VecDeque::new(),
            packet_buffer: VecDeque::new(),
            state: ConnState::Connecting,
            peer_addr,
            role: ConnRole::Host,
        };
        conn.handshake().await?;
        Ok(conn)
    }

    /// Send hello and announce timeout duration
    /// - Device endpoint: sends ControlHello with keep_alive_interval_ms
    /// - Host endpoint: sends ControlHello with keep_alive_interval_ms
    async fn handshake(&mut self) -> Result<(), Error> {
        match self.role {
            ConnRole::Device => {
                // Device need to send ControlHello
                unimplemented!("Device handshake not implemented yet");
            }
            ConnRole::Host => {
                unimplemented!("Host handshake not implemented yet");
            }
        }
    }

    async fn send_packet_inner(&mut self, packet: Packet) -> Result<(), Error> {
        let frames = packet_to_frames(&packet);
        for frame in frames {
            let bytes = frame.encode();
            self.stream.write_all(&bytes).await.map_err(Error::from)?;
        }
        Ok(())
    }

    async fn recv_packet_inner(&mut self) -> Result<Packet, Error> {
        loop {
            if let Some(packet) = self.packet_buffer.pop_front() {
                return Ok(packet);
            }
            // Read bytes: TCP stream -> byte_buffer
            let n = self
                .stream
                .read_buf(&mut self.byte_buffer)
                .await
                .map_err(Error::from)?;
            if n == 0 {
                self.state = ConnState::Closed;
                return Err(Error::ConnectionClosed);
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
