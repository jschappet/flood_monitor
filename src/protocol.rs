use serde_json::{json, Value};
use std::io::{self, Read, Write};

pub const MAX_FRAME: usize = 176;

pub fn send(writer: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    if payload.is_empty() || payload.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid command length",
        ));
    }
    writer.write_all(&[b'<', payload.len() as u8, 0])?;
    writer.write_all(payload)?;
    writer.flush()
}

/// Retain partial headers/payloads across serial read timeouts.
#[derive(Default)]
pub struct Frames {
    buffer: Vec<u8>,
}
impl Frames {
    pub fn read(&mut self, reader: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
        loop {
            if let Some(start) = self.buffer.iter().position(|b| *b == b'>') {
                self.buffer.drain(..start);
                if self.buffer.len() >= 3 {
                    let len = u16::from_le_bytes([self.buffer[1], self.buffer[2]]) as usize;
                    if len == 0 || len > MAX_FRAME {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "invalid MeshCore frame length",
                        ));
                    }
                    if self.buffer.len() >= len + 3 {
                        let frame = self.buffer[3..len + 3].to_vec();
                        self.buffer.drain(..len + 3);
                        return Ok(Some(frame));
                    }
                }
            } else {
                self.buffer.clear();
            }
            let mut bytes = [0u8; 512];
            match reader.read(&mut bytes) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "radio disconnected",
                    ));
                }
                Ok(n) => self.buffer.extend_from_slice(&bytes[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    return Ok(None);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes.split(|b| *b == 0).next().unwrap_or_default()).into_owned()
}
fn u32_at(p: &[u8], n: usize) -> u32 {
    u32::from_le_bytes(p[n..n + 4].try_into().unwrap())
}

pub fn decode(p: &[u8]) -> Result<Value, &'static str> {
    let kind = *p.first().ok_or("empty frame")?;
    let need = |n| {
        if p.len() >= n {
            Ok(())
        } else {
            Err("truncated MeshCore packet")
        }
    };
    Ok(match kind {
        0x03 => {
            need(132)?;
            json!({"type":"contact", "public_key":hex(&p[1..33]), "name":text(&p[100..132]), "contact_type":p[33]})
        }
        0x82 => {
            need(5)?;
            json!({"type":"delivery_ack", "tag":hex(&p[1..5])})
        }
        0x8f => {
            need(33)?;
            json!({"type":"contact_deleted", "public_key":hex(&p[1..33])})
        }
        0x05 => {
            need(58)?;
            json!({"type":"self_info", "name":text(&p[58..]), "public_key":hex(&p[4..36]), "frequency_mhz":u32_at(p,48) as f64 / 1000.0})
        }
        0x0d => {
            need(80)?;
            json!({"type":"device_info", "protocol_version":p[1], "model":text(&p[20..60]), "firmware":text(&p[60..80]), "build":text(&p[8..20])})
        }
        0x07 | 0x08 | 0x10 | 0x11 => {
            let v3 = kind == 0x10 || kind == 0x11;
            let direct = kind == 0x07 || kind == 0x10;
            let offset = if v3 { 4 } else { 1 };
            let identity_len = if direct { 6 } else { 1 };
            let meta = offset + identity_len;
            need(meta + 6)?;
            let text_type = p[meta + 1];
            let start = meta + 6 + if direct && text_type == 2 { 4 } else { 0 };
            need(start)?;
            let mut event = json!({"type":if direct {"direct_message"} else {"channel_message"},
                "path_length":p[meta], "text_type":text_type, "timestamp":u32_at(p,meta+2), "text":text(&p[start..])});
            if direct {
                event["sender_key_prefix"] = json!(hex(&p[offset..meta]));
            } else {
                event["channel"] = json!(p[offset]);
            }
            if v3 {
                event["snr_db"] = json!(p[1] as i8 as f64 / 4.0);
            }
            event
        }
        0x0c => {
            need(3)?;
            json!({"type":"battery", "voltage":u16::from_le_bytes([p[1],p[2]]) as f64 / 1000.0})
        }
        0x0a => json!({"type":"queue_empty"}),
        0x83 => json!({"type":"messages_waiting"}),
        0x01 => json!({"type":"radio_error", "payload_hex":hex(p)}),
        _ => json!({"type":"raw", "code":kind, "payload_hex":hex(p)}),
    })
}

pub fn chat_command(
    target: &str,
    channel: u8,
    name: &str,
    text: &str,
    timestamp: u32,
) -> Result<Vec<u8>, &'static str> {
    let limit = if target == "public" {
        133usize.saturating_sub(name.len() + 2)
    } else {
        133
    };
    if text.trim().is_empty() || text.len() > limit || text.contains('\0') {
        return Err("Message is empty, contains a NUL, or exceeds the UTF-8 byte limit");
    }
    let mut p = if target == "public" {
        vec![3, 0, channel]
    } else {
        vec![2, 0, 0]
    };
    p.extend_from_slice(&timestamp.to_le_bytes());
    if target != "public" {
        if target.len() != 12 || !target.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("Invalid contact key prefix");
        }
        for i in (0..12).step_by(2) {
            p.push(u8::from_str_radix(&target[i..i + 2], 16).map_err(|_| "Invalid contact key")?);
        }
    }
    p.extend_from_slice(text.as_bytes());
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    #[test]
    fn framing_and_commands() {
        let mut bytes = Vec::new();
        send(&mut bytes, &[0x16, 3]).unwrap();
        assert_eq!(bytes, [b'<', 2, 0, 0x16, 3]);
        let mut reader = Cursor::new(b"log\n>\x02\x00\x0c\x01>\x01\x00\x0a");
        let mut f = Frames::default();
        assert_eq!(f.read(&mut reader).unwrap(), Some(vec![0x0c, 1]));
        assert_eq!(f.read(&mut reader).unwrap(), Some(vec![0x0a]));
        assert!(Frames::default()
            .read(&mut Cursor::new(b">\xff\xff"))
            .is_err());
    }
    #[test]
    fn partial_frame_survives_timeout() {
        struct Parts(usize);
        impl Read for Parts {
            fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
                self.0 += 1;
                match self.0 {
                    1 => {
                        b[..2].copy_from_slice(b">\x01");
                        Ok(2)
                    }
                    2 => Err(io::ErrorKind::TimedOut.into()),
                    3 => {
                        b[..2].copy_from_slice(b"\x00\x0a");
                        Ok(2)
                    }
                    _ => Ok(0),
                }
            }
        }
        let mut reader = Parts(0);
        let mut f = Frames::default();
        assert_eq!(f.read(&mut reader).unwrap(), None);
        assert_eq!(f.read(&mut reader).unwrap(), Some(vec![0x0a]));
    }
    #[test]
    fn messages_and_truncation() {
        let p = [0x11, 0xf8, 0, 0, 2, 0xff, 0, 1, 0, 0, 0, b'H', b'i'];
        let event = decode(&p).unwrap();
        assert_eq!(event["text"], "Hi");
        assert_eq!(event["snr_db"], -2.0);
        assert_eq!(event["channel"], 2);
        for n in 0..11 {
            assert!(decode(&p[..n]).is_err());
        }
        let mut signed = vec![7, 1, 2, 3, 4, 5, 6, 0, 2, 1, 0, 0, 0, 9, 9, 9, 9];
        signed.extend_from_slice(b"Hello");
        assert_eq!(decode(&signed).unwrap()["text"], "Hello");
        assert!(decode(&signed[..16]).is_err());
    }
}
