mod capture;
mod protocol;

use capture::Recorder;
use protocol::{decode, hex, send, Frames};
use std::{
    env,
    fs::File,
    io,
    path::Path,
    time::{Duration, Instant},
};

const HELP: &str = "MeshCore USB companion receiver (115200 baud)
Usage:
  flood_monitor [live] [--port /dev/ttyACM0]
  flood_monitor record DIRECTORY [--port /dev/ttyACM0]
  flood_monitor replay FILE
  flood_monitor --help

Port defaults to MESHCORE_PORT, then /dev/ttyACM0.
Recordings contain received MeshCore frames, including unrecognized packets.
Receiving drains the companion's queued messages; stop other USB clients first.
";

fn display(p: &[u8], timestamp: Option<u64>) {
    let mut event = decode(p).unwrap_or_else(
        |e| serde_json::json!({"type":"malformed", "error":e, "payload_hex":hex(p)}),
    );
    if matches!(p.first(), Some(0x0a | 0x83)) {
        return;
    }
    if let Some(ts) = timestamp {
        event["recorded_at"] = ts.into();
    }
    println!("{event}");
}

struct Radio<T> {
    port: T,
    frames: Frames,
    recorder: Option<Recorder>,
}
impl<T: io::Read + io::Write> Radio<T> {
    fn receive(&mut self) -> io::Result<Option<Vec<u8>>> {
        let frame = self.frames.read(&mut self.port)?;
        if let Some(ref p) = frame {
            if let Some(r) = &mut self.recorder {
                r.record(p)?;
            }
            display(p, None);
        }
        Ok(frame)
    }
    fn command(&mut self, command: &[u8], expected: &[u8]) -> io::Result<Vec<u8>> {
        send(&mut self.port, command)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(p) = self.receive()? {
                if p[0] == 1 {
                    return Err(io::Error::other(format!(
                        "radio rejected command: {}",
                        hex(&p)
                    )));
                }
                if expected.contains(&p[0]) {
                    // Do not accept a malformed response as successful initialization.
                    decode(&p).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                    return Ok(p);
                }
            }
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "no expected MeshCore response within 5 seconds; check USB companion firmware and port",
        ))
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let mut args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|s| s == "--help" || s == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    let mut port = env::var("MESHCORE_PORT").unwrap_or_else(|_| "/dev/ttyACM0".into());
    if let Some(i) = args.iter().position(|s| s == "--port") {
        if i + 1 == args.len() {
            return Err("--port requires a device path".into());
        }
        port = args.remove(i + 1);
        args.remove(i);
    }
    let mode = args.first().map(String::as_str).unwrap_or("live");
    let directory = match (mode, args.len()) {
        ("live", 0 | 1) => None,
        ("record", 2) => Some(args[1].clone()),
        ("replay", 2) => {
            capture::replay(File::open(&args[1])?, |ts, p| display(p, Some(ts)))?;
            return Ok(());
        }
        _ => return Err(format!("invalid arguments\n{HELP}").into()),
    };
    log::info!("Connecting to MeshCore on {port}");
    let serial = serialport::new(&port, 115200)
        .timeout(Duration::from_millis(200))
        .open()?;
    // Allow USB firmware to settle after opening the port, as in the successful Pi probe.
    std::thread::sleep(Duration::from_secs(1));
    let recorder = directory
        .as_deref()
        .map(|d| Recorder::new(Path::new(d)))
        .transpose()?;
    let mut radio = Radio {
        port: serial,
        frames: Frames::default(),
        recorder,
    };
    let mut start = vec![1, 0, 0, 0, 0, 0, 0, 0];
    start.extend_from_slice(b"flood-monitor");
    radio.command(&start, &[5])?;
    radio.command(&[0x16, 3], &[0x0d])?;
    log::info!("Connected. Listening for messages; press Ctrl-C to stop.");
    loop {
        receive_messages(&mut radio)?;
    }
}

fn receive_messages<T: io::Read + io::Write>(radio: &mut Radio<T>) -> io::Result<()> {
    // One outstanding command at a time. Poll even when notifications are lost.
    let response = radio.command(&[0x0a], &[7, 8, 0x10, 0x11, 0x1b, 0x0a])?;
    if response[0] == 0x0a {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if radio.receive()?.is_some_and(|p| p[0] == 0x83) {
                break;
            }
        }
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

#[cfg(all(test, unix))]
mod integration_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    fn fixture(hex: &str) -> Vec<u8> {
        hex.split_whitespace()
            .map(|s| u8::from_str_radix(s, 16).unwrap())
            .collect()
    }

    #[test]
    fn handshake_queue_and_capture_over_fragmented_stream() {
        let (client, mut device) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        device
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let worker = std::thread::spawn(move || {
            let replies = [
                fixture(include_str!("../tests/fixtures/self-info.hex")),
                fixture(include_str!("../tests/fixtures/device-info.hex")),
                [
                    b">\x10\x00\x11\xf8\x00\x00\x02\xff\x00\x01\x00\x00\x00".as_slice(),
                    b"hello",
                ]
                .concat(),
                b">\x01\x00\x0a>\x01\x00\x83".to_vec(),
                b">\x01\x00\x0a".to_vec(),
            ];
            for (command, response) in [1, 0x16, 10, 10, 10].into_iter().zip(replies) {
                let mut header = [0; 3];
                device.read_exact(&mut header).unwrap();
                assert_eq!(header[0], b'<');
                let mut payload = vec![0; u16::from_le_bytes([header[1], header[2]]) as usize];
                device.read_exact(&mut payload).unwrap();
                assert_eq!(payload[0], command);
                if command == 0x16 {
                    assert_eq!(payload, [0x16, 3]);
                }
                device.write_all(&response[..2]).unwrap();
                std::thread::sleep(Duration::from_millis(40));
                device.write_all(&response[2..]).unwrap();
            }
            // Keep the stream connected through the idle polling interval.
            std::thread::sleep(Duration::from_millis(2300));
        });
        let dir = std::env::temp_dir().join(format!(
            "meshcore-integration-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut radio = Radio {
            port: client,
            frames: Frames::default(),
            recorder: Some(Recorder::new(&dir).unwrap()),
        };
        let p = radio.command(&[1, 0, 0, 0, 0, 0, 0, 0], &[5]).unwrap();
        assert_eq!(decode(&p).unwrap()["name"], "Schappet");
        let p = radio.command(&[0x16, 3], &[0x0d]).unwrap();
        let info = decode(&p).unwrap();
        assert_eq!(info["model"], "RAK 4631");
        assert_eq!(info["firmware"], "v1.18.0-81dbae5");
        receive_messages(&mut radio).unwrap();
        receive_messages(&mut radio).unwrap();
        receive_messages(&mut radio).unwrap();
        drop(radio);
        worker.join().unwrap();
        let mut events = Vec::new();
        capture::replay(
            File::open(dir.join("meshcore-recording-00000.bin")).unwrap(),
            |ts, p| {
                assert!(ts > 0);
                events.push(decode(p).unwrap());
            },
        )
        .unwrap();
        assert_eq!(events.len(), 6);
        assert_eq!(events[2]["text"], "hello");
        assert_eq!(events[2]["snr_db"], -2.0);
        assert_eq!(events[4]["type"], "messages_waiting");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
