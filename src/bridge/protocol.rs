//! The slice of scrcpy's control protocol the clipboard bridge speaks, as of scrcpy 3.3.
//! Source: server/src/main/java/com/genymobile/scrcpy/control/ in the scrcpy repository.

use std::io::{self, Read};

const SET_CLIPBOARD: u8 = 9;
const DEVICE_CLIPBOARD: u8 = 0;
const DEVICE_ACK_CLIPBOARD: u8 = 1;
const DEVICE_UHID_OUTPUT: u8 = 2;
const MESSAGE_MAX_BYTES: usize = 1 << 18;
/// Type, sequence, paste flag and length take 14 bytes of a set-clipboard message.
pub const TEXT_MAX_BYTES: usize = MESSAGE_MAX_BYTES - 14;

#[derive(Debug, PartialEq, Eq)]
pub enum DeviceMessage {
    Clipboard(String),
    Other,
}

/// Encodes a set-clipboard message. Sequence 0 asks the device for no acknowledgement.
pub fn set_clipboard(text: &str) -> Vec<u8> {
    let text = truncate(text, TEXT_MAX_BYTES);
    let mut message = Vec::with_capacity(14 + text.len());
    message.push(SET_CLIPBOARD);
    message.extend_from_slice(&0u64.to_be_bytes());
    message.push(0);
    message.extend_from_slice(&(text.len() as u32).to_be_bytes());
    message.extend_from_slice(text.as_bytes());
    message
}

pub fn read_message(reader: &mut impl Read) -> io::Result<DeviceMessage> {
    let mut kind = [0u8; 1];
    reader.read_exact(&mut kind)?;
    match kind[0] {
        DEVICE_CLIPBOARD => {
            let length = read_u32(reader)? as usize;
            if length > MESSAGE_MAX_BYTES {
                return Err(invalid(format!("clipboard of {length} bytes")));
            }
            let mut text = vec![0u8; length];
            reader.read_exact(&mut text)?;
            Ok(DeviceMessage::Clipboard(
                String::from_utf8_lossy(&text).into_owned(),
            ))
        }
        DEVICE_ACK_CLIPBOARD => {
            reader.read_exact(&mut [0u8; 8])?;
            Ok(DeviceMessage::Other)
        }
        DEVICE_UHID_OUTPUT => {
            reader.read_exact(&mut [0u8; 2])?;
            let mut size = [0u8; 2];
            reader.read_exact(&mut size)?;
            io::copy(
                &mut reader.take(u16::from_be_bytes(size).into()),
                &mut io::sink(),
            )?;
            Ok(DeviceMessage::Other)
        }
        other => Err(invalid(format!("unknown device message type {other}"))),
    }
}

/// Cuts `text` to at most `max` bytes without splitting a character.
pub fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let end = (0..=max)
        .rev()
        .find(|index| text.is_char_boundary(*index))
        .unwrap_or(0);
    &text[..end]
}

fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_set_clipboard_like_scrcpy() {
        assert_eq!(
            set_clipboard("hi"),
            [9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, b'h', b'i']
        );
    }

    #[test]
    fn decodes_clipboard_and_skips_other_messages() {
        let mut stream: &[u8] = &[
            1, 0, 0, 0, 0, 0, 0, 0, 7, // ack for sequence 7
            2, 0, 1, 0, 3, 9, 9, 9, // uhid output with 3 bytes
            0, 0, 0, 0, 5, b'h', b'e', b'l', b'l', b'o',
        ];
        assert_eq!(read_message(&mut stream).unwrap(), DeviceMessage::Other);
        assert_eq!(read_message(&mut stream).unwrap(), DeviceMessage::Other);
        assert_eq!(
            read_message(&mut stream).unwrap(),
            DeviceMessage::Clipboard("hello".to_string())
        );
        assert!(read_message(&mut stream).is_err());
    }

    #[test]
    fn rejects_unknown_message_types() {
        let mut stream: &[u8] = &[42];
        assert!(read_message(&mut stream).is_err());
    }

    #[test]
    fn truncates_on_a_character_boundary() {
        assert_eq!(truncate("héllo", 2), "h");
        assert_eq!(truncate("héllo", 3), "hé");
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn caps_oversized_clipboards() {
        let huge = "x".repeat(TEXT_MAX_BYTES + 100);
        assert_eq!(set_clipboard(&huge).len(), MESSAGE_MAX_BYTES);
    }
}
