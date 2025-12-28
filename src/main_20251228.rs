mod recording_stream;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write, BufReader};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Serialize, Deserialize};
use serde_json::to_string_pretty;

/// Represents a decoded FromRadio packet
#[derive(Debug, Serialize, Deserialize)]
pub struct FromRadio {
    pub raw_bytes: Vec<u8>,
    pub source_id: u32,
    pub timestamp: u64,
    pub decoded: Option<DecodedPayload>,
}

/// Example payload enum (extend as needed)
#[derive(Debug, Serialize, Deserialize)]
pub enum DecodedPayload {
    Telemetry(TelemetryData),
    Position(PositionData),
    Text(String),
}

/// Example telemetry struct
#[derive(Debug, Serialize, Deserialize)]
pub struct TelemetryData {
    pub battery_level: u8,
    pub voltage: f32,
    pub uptime_seconds: u64,
}

/// Example position struct
#[derive(Debug, Serialize, Deserialize)]
pub struct PositionData {
    pub lat: f64,
    pub lon: f64,
}

/// Recorder for writing FromRadio packets
pub struct FromRadioRecorder {
    bin_file: File,
    json_file: Option<File>,
}

impl FromRadioRecorder {
    /// Create a new recorder
    pub fn new<P: AsRef<Path>>(bin_path: P, json_path: Option<P>) -> io::Result<Self> {
        let bin_file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(bin_path)?;

        let json_file = match json_path {
            Some(p) => Some(OpenOptions::new().append(true).create(true).open(p)?),
            None => None,
        };

        Ok(Self { bin_file, json_file })
    }

    /// Record a packet to disk
    pub fn record(&mut self, packet: &FromRadio) -> io::Result<()> {
        // 1. Write raw bytes length + bytes
        let len = packet.raw_bytes.len() as u32;
        self.bin_file.write_all(&len.to_le_bytes())?;
        self.bin_file.write_all(&packet.raw_bytes)?;

        // 2. Optionally write JSON
        if let Some(ref mut f) = self.json_file {
            let json = to_string_pretty(&packet)?;
            f.write_all(json.as_bytes())?;
            f.write_all(b"\n")?;
        }

        Ok(())
    }
}

/// Helper to get current timestamp in seconds since epoch
pub fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Replay iterator for raw binary log
pub struct ReplayIterator<R: Read> {
    reader: BufReader<R>,
}

impl<R: Read> ReplayIterator<R> {
    pub fn new(reader: R) -> Self {
        ReplayIterator {
            reader: BufReader::new(reader),
        }
    }
}

impl<R: Read> Iterator for ReplayIterator<R> {
    type Item = io::Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut len_bytes = [0u8; 4];
        if let Err(e) = self.reader.read_exact(&mut len_bytes) {
            return if e.kind() == io::ErrorKind::UnexpectedEof {
                None
            } else {
                Some(Err(e))
            };
        }

        let len = u32::from_le_bytes(len_bytes) as usize;
        let mut buf = vec![0u8; len];
        if let Err(e) = self.reader.read_exact(&mut buf) {
            return Some(Err(e));
        }

        Some(Ok(buf))
    }
}



/// This example connects to a radio via serial and prints out all received packets.
/// This example requires a powered and flashed Meshtastic radio.
/// https://meshtastic.org/docs/supported-hardware


use meshtastic::api::StreamApi;
use meshtastic::utils;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let stream_api = StreamApi::new();


    let serial_stream = utils::stream::build_serial_stream("/dev/ttyACM0".to_string(), None, None, None)?;
    let (mut decoded_listener, stream_api) = stream_api.connect(serial_stream).await;

    let config_id = utils::generate_rand_id();
    let stream_api = stream_api.configure(config_id).await?;

    // This loop can be broken with ctrl+c or by disconnecting
    // the attached serial port.
    while let Some(decoded) = decoded_listener.recv().await {
        println!("Received: {:?}", decoded);
    }

    // Note that in this specific example, this will only be called when
    // the radio is disconnected, as the above loop will never exit.
    // Typically, you would allow the user to manually kill the loop,
    // for example, with tokio::select!.
    let _stream_api = stream_api.disconnect().await?;

    Ok(())
}

// ---------------------- Example Usage ---------------------- //

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_and_replay() {
        let tmp_bin = "test.bin";
        let tmp_json = "test.jsonl";

        let mut recorder = FromRadioRecorder::new(tmp_bin, Some(tmp_json)).unwrap();

        let packet = FromRadio {
            raw_bytes: vec![1, 2, 3, 4, 5],
            source_id: 12345,
            timestamp: current_timestamp(),
            decoded: Some(DecodedPayload::Telemetry(TelemetryData {
                battery_level: 100,
                voltage: 4.2,
                uptime_seconds: 3600,
            })),
        };

        recorder.record(&packet).unwrap();

        // Replay
        let file = File::open(tmp_bin).unwrap();
        let replay = ReplayIterator::new(file);
        for frame in replay {
            let bytes = frame.unwrap();
            assert_eq!(bytes, vec![1, 2, 3, 4, 5]);
        }
    }
}

