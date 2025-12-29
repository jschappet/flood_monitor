use std::fs::File;
use std::io::{self, BufReader, Read};

use meshtastic::protobufs::FromRadio;
use meshtastic::Message;

pub struct PlaybackStream {
    reader: BufReader<File>,
}

impl PlaybackStream {
    pub fn open(path: &str) -> io::Result<Self> {
        Ok(Self {
            reader: BufReader::new(File::open(path)?),
        })
    }
}

impl Iterator for PlaybackStream {
    type Item = io::Result<FromRadio>;

    fn next(&mut self) -> Option<Self::Item> {
    // Read 8-byte timestamp (discard for now)
    let mut ts_buf = [0u8; 8];
    if let Err(e) = self.reader.read_exact(&mut ts_buf) {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            return None;
        } else {
            return Some(Err(e.into()));
        }
    }

    // Now read 4-byte payload length
    let mut len_buf = [0u8; 4];
    if let Err(e) = self.reader.read_exact(&mut len_buf) {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            return None;
        } else {
            return Some(Err(e.into()));
        }
    }

    let len = u32::from_le_bytes(len_buf) as usize;

    let mut buf = vec![0u8; len];
    if let Err(e) = self.reader.read_exact(&mut buf) {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            eprintln!("Warning: truncated frame at end of file");
            return None;
        } else {
            return Some(Err(e.into()));
        }
    }

    match FromRadio::decode(&buf[..]) {
        Ok(msg) => Some(Ok(msg)),
        Err(e) => Some(Err(io::Error::new(io::ErrorKind::InvalidData, e))),
    }
}

}
