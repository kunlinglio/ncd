use std::net::SocketAddr;

use crate::connection::ConnState;

#[derive(Debug, Clone)]
pub struct Status {
    pub state: ConnState,
    pub peer_addr: Option<SocketAddr>,
}

impl Status {
    pub(crate) fn new(state: ConnState, peer_addr: SocketAddr) -> Self {
        Status {
            state,
            peer_addr: Some(peer_addr),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.state == ConnState::Connected
    }

    pub fn is_closed(&self) -> bool {
        matches!(self.state, ConnState::Closed)
    }
}
