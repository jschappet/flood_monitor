use crate::protocol::MAX_FRAME;
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
const MAGIC: &[u8; 8] = b"MCORE\0\x01\0";
const LIMIT: u64 = 10 * 1024 * 1024;
pub struct Recorder {
    dir: PathBuf,
    file: File,
    index: u64,
    size: u64,
}
impl Recorder {
    pub fn new(dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let (file, index) = Self::create(dir, 0)?;
        Ok(Self {
            dir: dir.into(),
            file,
            index,
            size: 8,
        })
    }
    fn create(dir: &Path, mut index: u64) -> io::Result<(File, u64)> {
        loop {
            let path = dir.join(format!("meshcore-recording-{index:05}.bin"));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    f.write_all(MAGIC)?;
                    log::info!("Recording to {}", path.display());
                    return Ok((f, index));
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => index += 1,
                Err(e) => return Err(e),
            }
        }
    }
    pub fn record(&mut self, p: &[u8]) -> io::Result<()> {
        if p.is_empty() || p.len() > MAX_FRAME {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        if self.size + 12 + p.len() as u64 > LIMIT {
            (self.file, self.index) = Self::create(&self.dir, self.index + 1)?;
            self.size = 8;
        }
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_secs();
        self.file.write_all(&timestamp.to_le_bytes())?;
        self.file.write_all(&(p.len() as u32).to_le_bytes())?;
        self.file.write_all(p)?;
        self.file.flush()?;
        self.size += 12 + p.len() as u64;
        Ok(())
    }
}

pub fn replay(mut input: impl Read, mut emit: impl FnMut(u64, &[u8])) -> io::Result<()> {
    let mut magic = [0; 8];
    input.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a MeshCore v1 capture; Meshtastic recordings are not supported",
        ));
    }
    loop {
        let mut timestamp = [0; 8];
        // Only EOF at the record boundary is a clean end of file.
        loop {
            match input.read(&mut timestamp[..1]) {
                Ok(0) => return Ok(()),
                Ok(_) => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        input.read_exact(&mut timestamp[1..])?;
        let mut length = [0; 4];
        input.read_exact(&mut length)?;
        let len = u32::from_le_bytes(length) as usize;
        if len == 0 || len > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid capture frame length",
            ));
        }
        let mut p = vec![0; len];
        input.read_exact(&mut p)?;
        emit(u64::from_le_bytes(timestamp), &p);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_rotation_restart_and_corruption() {
        let dir = std::env::temp_dir().join(format!(
            "flood-monitor-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let mut r = Recorder::new(&dir).unwrap();
            r.record(&[10]).unwrap();
            r.size = LIMIT;
            r.record(&[0x83]).unwrap();
            assert_eq!(r.index, 1);
        }
        let r = Recorder::new(&dir).unwrap();
        assert_eq!(r.index, 2);
        drop(r);
        let data = std::fs::read(dir.join("meshcore-recording-00000.bin")).unwrap();
        let mut frames = Vec::new();
        replay(&data[..], |_, p| frames.push(p.to_vec())).unwrap();
        assert_eq!(frames, [vec![10]]);
        for len in 9..data.len() {
            assert!(replay(&data[..len], |_, _| {}).is_err());
        }
        assert!(replay(&b"old-file"[..], |_, _| {}).is_err());
        let mut oversized = data.clone();
        oversized[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(replay(&oversized[..], |_, _| {}).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
