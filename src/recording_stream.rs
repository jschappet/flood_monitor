use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10 MB

pub struct RecordingStream {
    dir: PathBuf,
    current_file: File,
    current_size: u64,
    file_index: u64,
}



impl RecordingStream {
    pub fn new<P: AsRef<Path>>(dir: P) -> io::Result<Self> {
        std::fs::create_dir_all(&dir)?;

        let stream = Self {
            dir: dir.as_ref().to_path_buf(),
            current_file: Self::open_file(&dir, 0)?,
            current_size: 0,
            file_index: 0,
        };

        Ok(stream)
    }

    fn open_file<P: AsRef<Path>>(dir: P, index: u64) -> io::Result<File> {
        let filename = format!("meshtastic-recording-{:05}.bin", index);
        let path = dir.as_ref().join(filename);

        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    }

    fn rotate_if_needed(&mut self, next_record_size: u64) -> io::Result<()> {
        if self.current_size + next_record_size <= MAX_FILE_SIZE {
            return Ok(());
        }

        self.file_index += 1;
        self.current_file = Self::open_file(&self.dir, self.file_index)?;
        self.current_size = 0;

        Ok(())
    }

    pub fn record(&mut self, raw_payload: &[u8]) -> io::Result<()> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_secs();

        let payload_len = raw_payload.len() as u32;

        let record_size =
            8 + // timestamp
            4 + // payload_len
            payload_len as u64;

        self.rotate_if_needed(record_size)?;

        self.current_file.write_all(&timestamp.to_le_bytes())?;
        self.current_file.write_all(&payload_len.to_le_bytes())?;
        self.current_file.write_all(raw_payload)?;

        self.current_size += record_size;

        Ok(())
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.current_file.flush()
    }
}

