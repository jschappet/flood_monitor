use std::fs::File;
use std::io::{self, Read, BufReader};

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
        let mut len_buf = [0u8; 4];
        if let Err(e) = self.reader.read_exact(&mut len_buf) {
            return if e.kind() == io::ErrorKind::UnexpectedEof {
                None
            } else {
                Some(Err(e))
            };
        }

        let len = u32::from_le_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        if let Err(e) = self.reader.read_exact(&mut buf) {
            return Some(Err(e));
        }

        match FromRadio::decode(&buf[..]) {
            Ok(msg) => Some(Ok(msg)),
            Err(e) => Some(Err(io::Error::new(io::ErrorKind::InvalidData, e))),
        }
    }
}

