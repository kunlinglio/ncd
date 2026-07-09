use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::raw::c_void;
use std::ptr;

use tokio::io::unix::AsyncFd;

const NETLINK_USER: i32 = 31;
pub const NCD_MSG_REGISTER: u16 = 0; // daemon→driver:  send daemon PID
pub const NCD_MSG_OPEN_REQ: u16 = 1; // driver→daemon:  request to open device (and wait for connection)
pub const NCD_MSG_CONN_RES: u16 = 2; // daemon→driver:  connection result (success/fail)
pub const NCD_MSG_DATA: u16 = 3; // bi-directional: data transfer
pub const NCD_MSG_CLOSE_REQ: u16 = 4; // driver→daemon:  request to close device
pub const NCD_MSG_CREATE_DEV: u16 = 5; // daemon→driver:  create device
pub const NCD_MSG_DESTROY_DEV: u16 = 6; // daemon→driver:  destroy device

#[repr(C)]
struct NlMsgHdr {
    nlmsg_len: u32,
    nlmsg_type: u16,
    nlmsg_flags: u16,
    nlmsg_seq: u32,
    nlmsg_pid: u32, // sender PID
}

#[repr(C)]
struct SockAddrNl {
    nl_family: u16,
    nl_pad: u16,
    nl_pid: u32,
    nl_groups: u32,
}

const NLMSG_HDRLEN: usize = 16; // mem::size_of::<NlMsgHdr>()
const RECV_BUF_SIZE: usize = 4096;
fn nlmsg_align(len: usize) -> usize {
    (len + 3) & !3
} // align to 4 bytes

pub struct NetlinkSocket {
    fd: AsyncFd<OwnedFd>, // tokio async file descriptor
    pid: u32,             // daemon PID (assigned by kernel)
}

impl NetlinkSocket {
    /// Create and bind a Netlink socket (AF_NETLINK, NETLINK_USER).
    /// Sets O_NONBLOCK so tokio can drive it via AsyncFd.
    pub fn new() -> io::Result<Self> {
        // 1. socket(AF_NETLINK, SOCK_RAW, NETLINK_USER)
        let raw_fd = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW, NETLINK_USER) };
        if raw_fd < 0 {
            return Err(io::Error::last_os_error());
        }

        // 2. Set non-blocking (required by tokio AsyncFd)
        unsafe {
            let flags = libc::fcntl(raw_fd, libc::F_GETFL, 0);
            if flags < 0 {
                libc::close(raw_fd);
                return Err(io::Error::last_os_error());
            }
            if libc::fcntl(raw_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                libc::close(raw_fd);
                return Err(io::Error::last_os_error());
            }
        }

        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };

        // 3. bind(fd, addr) — pid=0 lets kernel assign our PID
        let mut addr = SockAddrNl {
            nl_family: libc::AF_NETLINK as u16,
            nl_pad: 0,
            nl_pid: 0,    // 0 = let kernel assign
            nl_groups: 0, // no multicast
        };
        let ret = unsafe {
            libc::bind(
                raw_fd,
                &addr as *const SockAddrNl as *const libc::sockaddr,
                mem::size_of::<SockAddrNl>() as libc::socklen_t,
            )
        };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }

        // 4. getsockname → learn assigned PID
        let mut addr_len = mem::size_of::<SockAddrNl>() as libc::socklen_t;
        let ret = unsafe {
            libc::getsockname(
                raw_fd,
                &mut addr as *mut SockAddrNl as *mut libc::sockaddr,
                &mut addr_len,
            )
        };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }

        // 5. Wrap in AsyncFd for tokio integration
        let async_fd = AsyncFd::new(fd)?;

        Ok(NetlinkSocket {
            fd: async_fd,
            pid: addr.nl_pid,
        })
    }

    /// Return the kernel-assigned PID of this socket.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    // ─────────────────── Low-level send / recv ─────────────────────

    /// Send a Netlink message to the kernel (dst PID = 0).
    pub async fn send_to_kernel(&self, msg_type: u16, payload: &[u8]) -> io::Result<()> {
        loop {
            let mut guard = self.fd.writable().await?;
            match guard.try_io(|_| self.try_send(msg_type, payload)) {
                Ok(Ok(())) => return Ok(()),
                Ok(Err(e)) => return Err(e), // real error
                Err(_) => continue,          // WouldBlock → retry
            }
        }
    }

    /// Receive a Netlink message from the kernel.
    /// Returns (message_type, payload).
    pub async fn recv_from_kernel(&self) -> io::Result<(u16, Vec<u8>)> {
        loop {
            let mut guard = self.fd.readable().await?;
            match guard.try_io(|_| self.try_recv()) {
                Ok(Ok(result)) => return Ok(result),
                Ok(Err(e)) => return Err(e),
                Err(_) => continue, // WouldBlock → retry
            }
        }
    }

    // ────── Convenience methods (thin wrappers around send) ────────

    /// Register this daemon with the kernel driver.
    /// Must be called first, before any other communication.
    pub async fn register(&self) -> io::Result<()> {
        self.send_to_kernel(NCD_MSG_REGISTER, Vec::<u8>::new().as_slice())
            .await
    }

    /// Ask the driver to create a device node /dev/<name>.
    pub async fn create_device(&self, name: &str) -> io::Result<()> {
        self.send_to_kernel(NCD_MSG_CREATE_DEV, name.as_bytes())
            .await
    }

    /// Ask the driver to destroy a device node identified by `minor`.
    pub async fn destroy_device(&self, minor: u8) -> io::Result<()> {
        self.send_to_kernel(NCD_MSG_DESTROY_DEV, [minor].as_slice())
            .await
    }

    /// Tell the driver whether the TCP connection for `minor` succeeded.
    /// Called after the daemon attempts to connect to the remote server.
    pub async fn send_conn_result_to_kernel(&self, minor: u8, success: bool) -> io::Result<()> {
        let payload = vec![minor, if success { b'1' } else { b'0' }];
        self.send_to_kernel(NCD_MSG_CONN_RES, &payload).await
    }

    /// Forward data received from a TCP peer to the driver.
    /// The driver will put it into the kfifo for the user process to read().
    pub async fn send_data_to_kernel(&self, minor: u8, data: &[u8]) -> io::Result<()> {
        let mut payload = Vec::with_capacity(1 + data.len());
        payload.push(minor);
        payload.extend_from_slice(data);
        self.send_to_kernel(NCD_MSG_DATA, &payload).await
    }

    // ─────────────────── Internal helpers ──────────────────────────

    /// Build a Netlink message and send it via sendmsg(2).
    /// Called inside try_io; must not block.
    fn try_send(&self, msg_type: u16, payload: &[u8]) -> io::Result<()> {
        let raw_fd = self.fd.get_ref().as_raw_fd();

        let data_len = NLMSG_HDRLEN + payload.len();
        let aligned_len = nlmsg_align(data_len);

        // Build header
        let nlh = NlMsgHdr {
            nlmsg_len: data_len as u32,
            nlmsg_type: msg_type,
            nlmsg_flags: 0,
            nlmsg_seq: 0,
            nlmsg_pid: self.pid,
        };

        // Assemble buffer: [header] + [payload] + [padding if needed]
        let mut buf = vec![0u8; aligned_len];
        unsafe {
            ptr::copy_nonoverlapping(
                &nlh as *const NlMsgHdr as *const u8,
                buf.as_mut_ptr(),
                NLMSG_HDRLEN,
            );
        }
        if !payload.is_empty() {
            buf[NLMSG_HDRLEN..NLMSG_HDRLEN + payload.len()].copy_from_slice(payload);
        }

        // Destination: kernel (PID=0)
        let mut addr = SockAddrNl {
            nl_family: libc::AF_NETLINK as u16,
            nl_pad: 0,
            nl_pid: 0, // kernel
            nl_groups: 0,
        };

        let iov = libc::iovec {
            iov_base: buf.as_mut_ptr() as *mut c_void,
            iov_len: aligned_len,
        };

        let mut msg: libc::msghdr = unsafe { mem::zeroed() };
        msg.msg_name = &mut addr as *mut SockAddrNl as *mut c_void;
        msg.msg_namelen = mem::size_of::<SockAddrNl>() as u32;
        msg.msg_iov = &iov as *const libc::iovec as *mut libc::iovec;
        msg.msg_iovlen = 1;

        let ret = unsafe { libc::sendmsg(raw_fd, &msg, 0) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Receive a Netlink message via recvmsg(2).
    /// Called inside try_io; must not block.
    fn try_recv(&self) -> io::Result<(u16, Vec<u8>)> {
        let raw_fd = self.fd.get_ref().as_raw_fd();

        let mut buf = vec![0u8; RECV_BUF_SIZE];

        let mut addr = SockAddrNl {
            nl_family: 0,
            nl_pad: 0,
            nl_pid: 0,
            nl_groups: 0,
        };

        let iov = libc::iovec {
            iov_base: buf.as_mut_ptr() as *mut c_void,
            iov_len: RECV_BUF_SIZE,
        };

        let mut msg: libc::msghdr = unsafe { mem::zeroed() };
        msg.msg_name = &mut addr as *mut SockAddrNl as *mut c_void;
        msg.msg_namelen = mem::size_of::<SockAddrNl>() as u32;
        msg.msg_iov = &iov as *const libc::iovec as *mut libc::iovec;
        msg.msg_iovlen = 1;

        let ret = unsafe { libc::recvmsg(raw_fd, &mut msg, 0) };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }

        let recv_len = ret as usize;
        if recv_len < NLMSG_HDRLEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "short netlink message",
            ));
        }

        // Parse nlmsghdr from the buffer
        let nlh: NlMsgHdr = unsafe { ptr::read(buf.as_ptr() as *const NlMsgHdr) };
        if nlh.nlmsg_len < NLMSG_HDRLEN as u32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bogus nlmsg_len",
            ));
        }
        let payload_len = nlh.nlmsg_len as usize - NLMSG_HDRLEN;

        let payload = if payload_len > 0 && recv_len >= NLMSG_HDRLEN + payload_len {
            buf[NLMSG_HDRLEN..NLMSG_HDRLEN + payload_len].to_vec()
        } else {
            Vec::new()
        };

        Ok((nlh.nlmsg_type, payload))
    }
}
