//! What processes say to each other - a program to the host (`host`), the host, the command line and agents to whoever
//! holds the project (`mcp::link`): CBOR (RFC 8949, as `pe2_ext::cbor` sends values to extensions), one message a frame -
//! its length in four bytes, little-endian, then the message. No text: nothing reads it but the other process.

use std::io::{self, Read, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// The longest message read: past it, what comes is no frame of this protocol.
pub const LONGEST: usize = 1 << 30;

/// Send a message, as one frame.
///
/// # Errors
/// It does not encode, or cannot be written.
pub fn send<T: Serialize + ?Sized>(to: &mut impl Write, message: &T) -> io::Result<()> {
    let mut bytes = Vec::new();
    ciborium::into_writer(message, &mut bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    let len =
        u32::try_from(bytes.len()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a message too long"))?;
    let mut frame = Vec::with_capacity(bytes.len() + 4);
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&bytes);
    to.write_all(&frame)?;
    to.flush()
}

/// Receive a message: none where the other side closed the connection between frames. A frame read whole that is no
/// `T` is an error of kind `InvalidData`, after which the next frame can be read.
///
/// # Errors
/// The connection fails or closes within a frame, the frame is too long, or it is no `T`.
pub fn receive<T: DeserializeOwned>(from: &mut impl Read) -> io::Result<Option<T>> {
    let mut len = [0u8; 4];
    match from.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = usize::try_from(u32::from_le_bytes(len)).unwrap_or(usize::MAX);
    if len > LONGEST {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("a frame of {len} bytes")));
    }
    let mut bytes = vec![0; len];
    from.read_exact(&mut bytes)?;
    ciborium::from_reader(bytes.as_slice())
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{receive, send};
    use crate::host::{FromHost, ToHost};

    #[test]
    fn messages_go_and_come_back_one_frame_each() {
        let said = [
            ToHost::Keep { who: "a test".into() },
            ToHost::Lend { who: "a test".into() },
            ToHost::Status,
            ToHost::Stop,
        ];
        let mut bytes = Vec::new();
        for message in &said {
            send(&mut bytes, message).unwrap();
        }
        send(&mut bytes, &FromHost::Kept { at: "/a".into(), note: None }).unwrap();
        let mut reading = bytes.as_slice();
        for message in &said {
            assert_eq!(receive::<ToHost>(&mut reading).unwrap().as_ref(), Some(message));
        }
        // A frame that is no `ToHost`: said so, and the next is read.
        assert_eq!(receive::<ToHost>(&mut reading).unwrap_err().kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(receive::<ToHost>(&mut reading).unwrap(), None);
    }

    #[test]
    fn what_python_says_is_read() {
        // `tools/project_lock.py`'s frame of `{"say": "lend", "who": "x"}`, and what it reads back.
        let frame: Vec<u8> = (0..40)
            .step_by(2)
            .map(|i| u8::from_str_radix(&"10000000a263736179646c656e646377686f6178"[i..i + 2], 16).unwrap())
            .collect();
        assert_eq!(receive::<ToHost>(&mut frame.as_slice()).unwrap(), Some(ToHost::Lend { who: "x".into() }));
        let mut lent = Vec::new();
        send(&mut lent, &FromHost::Lent).unwrap();
        assert_eq!(&lent[4..], [0xa1, 0x63, b's', b'a', b'y', 0x64, b'l', b'e', b'n', b't']);
    }
}
