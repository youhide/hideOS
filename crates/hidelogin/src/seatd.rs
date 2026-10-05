//! seatd's wire protocol, which libseat's seatd backend speaks and hidelogin
//! serves: how cosmic-comp asks for its GPU and input devices.
//!
//! A message is a header — opcode and body size, each a `u16` — then the
//! body. Integers are the machine's own byte order and `int` is 32 bits: the
//! protocol is between two processes on one machine, as seatd's
//! `include/protocol.h` defines it. A device's file descriptor travels
//! beside its `DeviceOpened` message, as ancillary data.

use thiserror::Error;

/// The longest device path seatd accepts, its terminating NUL included.
pub const MAX_PATH_LEN: usize = 256;

const HEADER: usize = 4;
const SERVER: u16 = 1 << 15;

/// What a client sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    OpenSeat,
    CloseSeat,
    OpenDevice(String),
    CloseDevice(i32),
    DisableSeat,
    SwitchSession(i32),
    Ping,
}

/// What the server sends: replies, and the two events — `EnableSeat` and
/// `DisableSeat` — that arrive unasked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    SeatOpened(String),
    SeatClosed,
    /// The device's id; its descriptor goes with the message.
    DeviceOpened(i32),
    DeviceClosed,
    DisableSeat,
    EnableSeat,
    Pong,
    SessionSwitched,
    SeatDisabled,
    /// An errno.
    Error(i32),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("unknown opcode {0}")]
    Opcode(u16),
    #[error("opcode {opcode} with a body of {size} bytes")]
    Size { opcode: u16, size: usize },
    #[error("a device path that is empty, too long, or not NUL-terminated")]
    Path,
}

/// The first whole request in `buffer`, and how many bytes it took; `None`
/// while the request is still arriving.
pub fn decode(buffer: &[u8]) -> Result<Option<(Request, usize)>, ProtocolError> {
    let (Some(opcode), Some(size)) = (u16_at(buffer, 0), u16_at(buffer, 2)) else {
        return Ok(None);
    };
    let size = usize::from(size);
    let Some(body) = buffer.get(HEADER..HEADER + size) else {
        return Ok(None);
    };
    let empty = |request: Request| {
        if size == 0 {
            Ok(request)
        } else {
            Err(ProtocolError::Size { opcode, size })
        }
    };
    let int = || {
        if size == 4 {
            i32_at(body, 0).ok_or(ProtocolError::Size { opcode, size })
        } else {
            Err(ProtocolError::Size { opcode, size })
        }
    };
    let request = match opcode {
        1 => empty(Request::OpenSeat)?,
        2 => empty(Request::CloseSeat)?,
        3 => {
            let len = u16_at(body, 0).ok_or(ProtocolError::Size { opcode, size })?;
            let len = usize::from(len);
            if len + 2 != size {
                return Err(ProtocolError::Size { opcode, size });
            }
            let bytes = body.get(2..).ok_or(ProtocolError::Path)?;
            // NUL-terminated, with no NUL before the end.
            let Some((&0, path)) = bytes.split_last() else {
                return Err(ProtocolError::Path);
            };
            if len > MAX_PATH_LEN || path.is_empty() || path.contains(&0) {
                return Err(ProtocolError::Path);
            }
            let path = std::str::from_utf8(path).map_err(|_| ProtocolError::Path)?;
            Request::OpenDevice(path.to_owned())
        }
        4 => Request::CloseDevice(int()?),
        5 => empty(Request::DisableSeat)?,
        6 => Request::SwitchSession(int()?),
        7 => empty(Request::Ping)?,
        other => return Err(ProtocolError::Opcode(other)),
    };
    Ok(Some((request, HEADER + size)))
}

/// `reply` as it goes on the wire.
pub fn encode(reply: &Reply) -> Vec<u8> {
    let (opcode, body): (u16, Vec<u8>) = match reply {
        Reply::SeatOpened(name) => {
            let mut body = Vec::new();
            // The name and its NUL.
            let len = u16::try_from(name.len() + 1).unwrap_or(u16::MAX);
            body.extend_from_slice(&len.to_ne_bytes());
            body.extend_from_slice(name.as_bytes());
            body.push(0);
            (1, body)
        }
        Reply::SeatClosed => (2, Vec::new()),
        Reply::DeviceOpened(id) => (3, id.to_ne_bytes().to_vec()),
        Reply::DeviceClosed => (4, Vec::new()),
        Reply::DisableSeat => (5, Vec::new()),
        Reply::EnableSeat => (6, Vec::new()),
        Reply::Pong => (7, Vec::new()),
        Reply::SessionSwitched => (8, Vec::new()),
        Reply::SeatDisabled => (9, Vec::new()),
        Reply::Error(errno) => (0x7FFF, errno.to_ne_bytes().to_vec()),
    };
    let mut message = Vec::with_capacity(HEADER + body.len());
    message.extend_from_slice(&(SERVER + opcode).to_ne_bytes());
    let size = u16::try_from(body.len()).unwrap_or(u16::MAX);
    message.extend_from_slice(&size.to_ne_bytes());
    message.extend_from_slice(&body);
    message
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    let pair = bytes.get(at..at + 2)?;
    Some(u16::from_ne_bytes([*pair.first()?, *pair.get(1)?]))
}

fn i32_at(bytes: &[u8], at: usize) -> Option<i32> {
    let four: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
    Some(i32::from_ne_bytes(four))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(opcode: u16, body: &[u8]) -> Vec<u8> {
        let mut bytes = opcode.to_ne_bytes().to_vec();
        bytes.extend_from_slice(&u16::try_from(body.len()).unwrap().to_ne_bytes());
        bytes.extend_from_slice(body);
        bytes
    }

    fn open_device(path: &str) -> Vec<u8> {
        let mut body = u16::try_from(path.len() + 1)
            .unwrap()
            .to_ne_bytes()
            .to_vec();
        body.extend_from_slice(path.as_bytes());
        body.push(0);
        request(3, &body)
    }

    #[test]
    fn requests_decode_as_libseat_sends_them() {
        assert_eq!(decode(&request(1, &[])), Ok(Some((Request::OpenSeat, 4))));
        let bytes = open_device("/dev/dri/card0");
        assert_eq!(
            decode(&bytes),
            Ok(Some((
                Request::OpenDevice("/dev/dri/card0".into()),
                bytes.len()
            )))
        );
        assert_eq!(
            decode(&request(4, &7i32.to_ne_bytes())),
            Ok(Some((Request::CloseDevice(7), 8)))
        );
        assert_eq!(
            decode(&request(6, &2i32.to_ne_bytes())),
            Ok(Some((Request::SwitchSession(2), 8)))
        );
    }

    #[test]
    fn a_request_still_arriving_is_waited_for() {
        let bytes = open_device("/dev/input/event3");
        for cut in 0..bytes.len() {
            assert_eq!(decode(&bytes[..cut]), Ok(None), "cut at {cut}");
        }
        // Two in a row: the first, and how far it went.
        let mut two = request(7, &[]);
        two.extend(request(5, &[]));
        assert_eq!(decode(&two), Ok(Some((Request::Ping, 4))));
        assert_eq!(decode(&two[4..]), Ok(Some((Request::DisableSeat, 4))));
    }

    #[test]
    fn malformed_requests_are_refused() {
        assert_eq!(
            decode(&request(1, &[0])),
            Err(ProtocolError::Size { opcode: 1, size: 1 })
        );
        assert_eq!(decode(&request(99, &[])), Err(ProtocolError::Opcode(99)));
        assert_eq!(
            decode(&request(4, &[1, 2])),
            Err(ProtocolError::Size { opcode: 4, size: 2 })
        );
        // No NUL at the end; a NUL inside; empty; a length that disagrees.
        let mut body = 3u16.to_ne_bytes().to_vec();
        body.extend_from_slice(b"abc");
        assert_eq!(decode(&request(3, &body)), Err(ProtocolError::Path));
        let mut body = 4u16.to_ne_bytes().to_vec();
        body.extend_from_slice(b"a\0b\0");
        assert_eq!(decode(&request(3, &body)), Err(ProtocolError::Path));
        let mut body = 1u16.to_ne_bytes().to_vec();
        body.push(0);
        assert_eq!(decode(&request(3, &body)), Err(ProtocolError::Path));
        let mut body = 9u16.to_ne_bytes().to_vec();
        body.extend_from_slice(b"ab\0");
        assert!(decode(&request(3, &body)).is_err());
        let long = "x".repeat(MAX_PATH_LEN);
        assert_eq!(decode(&open_device(&long)), Err(ProtocolError::Path));
    }

    #[test]
    fn replies_encode_as_libseat_reads_them() {
        let opened = encode(&Reply::SeatOpened("seat0".into()));
        assert_eq!(&opened[..2], &(SERVER + 1).to_ne_bytes());
        assert_eq!(&opened[2..4], &8u16.to_ne_bytes());
        assert_eq!(&opened[4..6], &6u16.to_ne_bytes());
        assert_eq!(&opened[6..], b"seat0\0");
        let error = encode(&Reply::Error(13));
        assert_eq!(&error[..2], &(SERVER + 0x7FFF).to_ne_bytes());
        assert_eq!(&error[4..], &13i32.to_ne_bytes());
        assert_eq!(
            encode(&Reply::EnableSeat),
            [&(SERVER + 6).to_ne_bytes()[..], &[0, 0]].concat()
        );
    }
}
