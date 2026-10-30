mod capture;
mod metrics;
mod protocol;
mod web;

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
  flood_monitor live --metrics-config metrics.local.json [--port DEVICE]
  flood_monitor metrics-status --metrics-config metrics.local.json
  flood_monitor live --web-bind 0.0.0.0:8080 [--public-channel 0]
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
    metrics: Option<metrics::Reporter>,
    web: Option<web::Hub>,
}
impl<T: io::Read + io::Write> Radio<T> {
    fn receive(&mut self) -> io::Result<Option<Vec<u8>>> {
        let frame = self.frames.read(&mut self.port)?;
        if let Some(reporter) = &mut self.metrics {
            reporter.tick(frame.as_deref())?;
        }
        if let Some(ref p) = frame {
            if p.first() != Some(&0x12) {
                if let Some(r) = &mut self.recorder {
                    r.record(p)?;
                }
            }
            if let Some(hub) = &self.web {
                hub.lock().unwrap().observe(p);
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
    let mut metrics_config = if let Some(i) = args.iter().position(|s| s == "--metrics-config") {
        if i + 1 == args.len() {
            return Err("--metrics-config requires a JSON file".into());
        }
        let path = args.remove(i + 1);
        args.remove(i);
        Some(metrics::Config::load(Path::new(&path))?)
    } else {
        None
    };
    let web_address = take_option(&mut args, "--web-bind")?
        .map(|s| s.parse::<std::net::SocketAddr>())
        .transpose()?;
    let channel_option = take_option(&mut args, "--public-channel")?
        .map(|s| s.parse::<u8>())
        .transpose()?;
    if let (Some(config), Some(channel)) = (&mut metrics_config, channel_option) {
        config.public_channel = channel;
    }
    let channel = channel_option
        .or_else(|| metrics_config.as_ref().map(|c| c.public_channel))
        .unwrap_or(0);
    let mode = args.first().map(String::as_str).unwrap_or("live");
    let directory = match (mode, args.len()) {
        ("metrics-status", 1) => {
            if web_address.is_some() {
                return Err("Web UI is only available in live and record modes".into());
            }
            let config = metrics_config
                .as_ref()
                .ok_or("metrics-status requires --metrics-config")?;
            println!("{}", metrics::status(config)?);
            return Ok(());
        }
        ("live", 0 | 1) => None,
        ("record", 2) => Some(args[1].clone()),
        ("replay", 2) => {
            if web_address.is_some() {
                return Err("Web UI is only available in live and record modes".into());
            }
            if metrics_config.is_some() {
                return Err("metrics reporting is only available in live and record modes".into());
            }
            capture::replay(File::open(&args[1])?, |ts, p| display(p, Some(ts)))?;
            return Ok(());
        }
        _ => return Err(format!("invalid arguments\n{HELP}").into()),
    };
    let hub = web_address
        .map(|address| web::start(address, channel))
        .transpose()?;
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
        metrics: None,
        web: hub,
    };
    let mut start = vec![1, 0, 0, 0, 0, 0, 0, 0];
    start.extend_from_slice(b"flood-monitor");
    radio.command(&start, &[5])?;
    radio.command(&[0x16, 3], &[0x0d])?;
    if radio.web.is_some() {
        refresh_contacts(&mut radio)?;
        refresh_channels(&mut radio)?;
    }
    if let Some(config) = metrics_config {
        // Do not count historical messages queued while this receiver was offline.
        log::info!("Draining startup message backlog before starting metrics");
        while radio.command(&[0x0a], &[7, 8, 0x10, 0x11, 0x1b, 0x0a])?[0] != 0x0a {}
        match metrics::Reporter::start(config) {
            Ok(reporter) => radio.metrics = Some(reporter),
            Err(error) => log::error!("Metrics disabled: {error}. Radio reception continues."),
        }
    }
    if let Some(hub) = &radio.web {
        hub.lock().unwrap().connected = true;
    }
    log::info!("Connected. Listening for messages; press Ctrl-C to stop.");
    loop {
        receive_messages(&mut radio)?;
        service_web(&mut radio)?;
    }
}

fn take_option(
    args: &mut Vec<String>,
    name: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    if let Some(i) = args.iter().position(|s| s == name) {
        if i + 1 == args.len() {
            return Err(format!("{name} requires a value").into());
        }
        let value = args.remove(i + 1);
        args.remove(i);
        Ok(Some(value))
    } else {
        Ok(None)
    }
}

fn refresh_contacts<T: io::Read + io::Write>(radio: &mut Radio<T>) -> io::Result<()> {
    let Some(hub) = radio.web.clone() else {
        return Ok(());
    };
    {
        let mut s = hub.lock().unwrap();
        s.refresh = false;
        s.refreshing = true;
        s.contacts.clear();
        s.contacts_error = None;
    }
    let result = (|| {
        send(&mut radio.port, &[4])?;
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if let Some(p) = radio.receive()? {
                if p[0] == 4 {
                    return Ok(());
                }
                if p[0] == 1 {
                    return Err(io::Error::other("Radio rejected contact synchronization"));
                }
            }
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Contact synchronization timed out",
        ))
    })();
    {
        let mut s = hub.lock().unwrap();
        s.refreshing = false;
        if result.is_err() {
            s.contacts.clear();
            s.contacts_error = Some("Contact synchronization failed".into());
        }
    }
    result
}

fn refresh_channels<T: io::Read + io::Write>(radio: &mut Radio<T>) -> io::Result<()> {
    let hub = radio.web.clone().unwrap();
    hub.lock().unwrap().channels_ready = false;
    hub.lock().unwrap().channels.clear();
    for index in 0..=255u8 {
        match radio.command(&[31, index], &[0x12]) {
            Ok(p) if p[1] == index => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Unexpected channel index",
                ))
            }
            Err(e)
                if e.kind() == io::ErrorKind::Other
                    && e.to_string() == "radio rejected command: 0102" =>
            {
                break
            }
            Err(e) => return Err(e),
        }
    }
    let mut s = hub.lock().unwrap();
    s.channels_ready = true;
    s.channels_refresh = false;
    Ok(())
}

fn join_channel<T: io::Read + io::Write>(radio: &mut Radio<T>, name: &str) -> io::Result<String> {
    // Refresh immediately before choosing a slot; never overwrite existing settings.
    refresh_channels(radio)?;
    let (name, secret) = web::hashtag(name).map_err(io::Error::other)?;
    let hub = radio.web.clone().unwrap();
    let index = {
        let s = hub.lock().unwrap();
        if let Some(c) = s
            .channels
            .values()
            .find(|c| c.secret == secret && !c.empty())
        {
            return Ok(format!("Already joined {} (channel {})", c.name, c.index));
        }
        if s.channels.values().any(|c| c.name == name) {
            return Ok(format!(
                "Cannot join {name}: an existing channel uses that name with a different key"
            ));
        }
        s.channels
            .values()
            .find(|c| c.index != 0 && c.index != s.channel && c.empty())
            .map(|c| c.index)
    };
    let Some(index) = index else {
        return Ok("No empty channel slots available. Existing channels were preserved.".into());
    };
    let mut command = vec![32, index];
    command.extend_from_slice(name.as_bytes());
    command.resize(34, 0);
    command.extend_from_slice(&secret);
    radio.command(&command, &[0])?;
    let p = radio.command(&[31, index], &[0x12])?;
    if p[1] != index
        || p[34..50] != secret
        || p[2..34].split(|b| *b == 0).next() != Some(name.as_bytes())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Channel save could not be verified",
        ));
    }
    Ok(format!("Joined {name}"))
}

fn service_web<T: io::Read + io::Write>(radio: &mut Radio<T>) -> io::Result<()> {
    let Some(hub) = radio.web.clone() else {
        return Ok(());
    };
    let join = hub.lock().unwrap().channel_join.clone();
    if let Some(name) = join {
        let result = join_channel(radio, &name);
        let mut s = hub.lock().unwrap();
        s.channel_join = None;
        match result {
            Ok(status) => s.channel_status = status,
            Err(e) => {
                s.channel_status = format!("Channel join failed: {e}");
                if e.kind() != io::ErrorKind::Other
                    || !e.to_string().starts_with("radio rejected command:")
                {
                    return Err(e);
                }
            }
        }
    } else if hub.lock().unwrap().channels_refresh {
        refresh_channels(radio)?;
    }
    let refresh = hub.lock().unwrap().refresh;
    if refresh {
        refresh_contacts(radio)?;
    }
    let work = hub.lock().unwrap().outbox.pop_front();
    if let Some(work) = work {
        let command = {
            let mut s = hub.lock().unwrap();
            s.last_tx_timestamp = (web::unix() as u32).max(s.last_tx_timestamp.saturating_add(1));
            let channel = s.channel_index(&work.target);
            if channel.is_some_and(|i| !s.channel_available(i)) {
                Err("Channel no longer available")
            } else if channel.is_none() && !s.can_dm(&work.target) {
                Err("Contact no longer available")
            } else {
                protocol::chat_command(
                    if channel.is_some() {
                        "public"
                    } else {
                        &work.target
                    },
                    channel.unwrap_or(s.channel),
                    &s.name,
                    &work.text,
                    s.last_tx_timestamp,
                )
            }
        };
        match command {
            Ok(command) => {
                let expected = if hub.lock().unwrap().channel_index(&work.target).is_some() {
                    0
                } else {
                    6
                };
                match radio.command(&command, &[expected]) {
                    Ok(p) => {
                        if expected == 6 && p.len() < 10 {
                            hub.lock().unwrap().failed(work.id, true);
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "Truncated send confirmation",
                            ));
                        }
                        hub.lock().unwrap().sent(work.id, &p);
                    }
                    Err(e) => {
                        let rejected = e.kind() == io::ErrorKind::Other
                            && e.to_string().starts_with("radio rejected command:");
                        hub.lock().unwrap().failed(work.id, !rejected);
                        if !rejected {
                            return Err(e);
                        } // Never retry an ambiguous radio send automatically.
                    }
                }
            }
            Err(_) => hub.lock().unwrap().failed(work.id, false),
        }
    }
    Ok(())
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
            metrics: None,
            web: None,
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
    #[test]
    fn hashtag_join_preserves_channels_and_sends_to_selected_slot() {
        let (client, mut device) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        device
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let worker = std::thread::spawn(move || {
            let mut slots = [
                ("Public".to_owned(), [1; 16]),
                ("Existing".to_owned(), [2; 16]),
                (String::new(), [0; 16]),
            ];
            let mut writes = 0;
            loop {
                let mut header = [0; 3];
                device.read_exact(&mut header).unwrap();
                let mut cmd = vec![0; header[1] as usize];
                device.read_exact(&mut cmd).unwrap();
                let response = match cmd[0] {
                    31 => {
                        if let Some((name, key)) = slots.get(cmd[1] as usize) {
                            let mut p = vec![0x12, cmd[1]];
                            p.extend_from_slice(name.as_bytes());
                            p.resize(34, 0);
                            p.extend_from_slice(key);
                            p
                        } else {
                            vec![1, 2]
                        }
                    }
                    32 => {
                        writes += 1;
                        assert_eq!(cmd[1], 2);
                        assert_eq!(cmd.len(), 50);
                        assert_eq!(&cmd[34..], &web::hashtag("#skagit").unwrap().1);
                        slots[2] = ("#skagit".into(), cmd[34..50].try_into().unwrap());
                        vec![0]
                    }
                    3 => {
                        assert_eq!(cmd[2], 2);
                        assert_eq!(&cmd[7..], b"Hello Skagit");
                        assert_eq!(writes, 1);
                        device.write_all(&[b'>', 1, 0, 0]).unwrap();
                        break;
                    }
                    _ => panic!("unexpected command"),
                };
                device.write_all(&[b'>', response.len() as u8, 0]).unwrap();
                device.write_all(&response).unwrap();
            }
        });
        let hub = std::sync::Arc::new(std::sync::Mutex::new(web::State::new(0)));
        hub.lock().unwrap().connected = true;
        let mut radio = Radio {
            port: client,
            frames: Frames::default(),
            recorder: None,
            metrics: None,
            web: Some(hub.clone()),
        };
        assert_eq!(
            join_channel(&mut radio, "#skagit").unwrap(),
            "Joined #skagit"
        );
        assert!(join_channel(&mut radio, "#skagit")
            .unwrap()
            .starts_with("Already joined"));
        assert!(join_channel(&mut radio, "#other")
            .unwrap()
            .starts_with("No empty"));
        hub.lock()
            .unwrap()
            .enqueue(web::SendRequest {
                request_id: "skagit-send-1".into(),
                target: "channel:2".into(),
                text: "Hello Skagit".into(),
            })
            .unwrap();
        service_web(&mut radio).unwrap();
        assert_eq!(
            hub.lock().unwrap().messages.back().unwrap().status,
            "submitted"
        );
        worker.join().unwrap();
    }

    #[test]
    fn received_v3_public_message_reaches_web_snapshot() {
        // Match the RAK4631's reported channel, SNR, path byte and sender text.
        let mut payload = vec![0x11, 47, 0, 0, 0, 128, 0];
        payload.extend_from_slice(&1793144701u32.to_le_bytes());
        payload.extend_from_slice(b"ReVillage: Hello All. ");
        let (client, mut device) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        device
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let worker = std::thread::spawn(move || {
            let mut command = [0; 4];
            device.read_exact(&mut command).unwrap();
            assert_eq!(command, [b'<', 1, 0, 10]);
            // Notifications and RF logs may precede the queued chat response.
            device
                .write_all(&[b'>', 1, 0, 0x83, b'>', 3, 0, 0x88, 47, 180])
                .unwrap();
            device.write_all(&[b'>', payload.len() as u8, 0]).unwrap();
            device.write_all(&payload).unwrap();
        });
        let hub = std::sync::Arc::new(std::sync::Mutex::new(web::State::new(0)));
        let mut radio = Radio {
            port: client,
            frames: Frames::default(),
            recorder: None,
            metrics: None,
            web: Some(hub.clone()),
        };
        receive_messages(&mut radio).unwrap();
        let snapshot = hub.lock().unwrap().snapshot();
        assert_eq!(snapshot["messages"].as_array().unwrap().len(), 1);
        let message = &snapshot["messages"][0];
        assert_eq!(message["conversation"], "public");
        assert_eq!(message["sender"], "ReVillage");
        assert_eq!(message["text"], "Hello All. ");
        assert_eq!(message["direction"], "in");
        assert_eq!(message["snr_db"], 11.75);
        worker.join().unwrap();
    }

    #[test]
    fn web_contact_sync_and_sends_share_radio_stream() {
        let (client, mut device) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        device
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let worker = std::thread::spawn(move || {
            for expected in [4, 3, 2] {
                let mut header = [0; 3];
                device.read_exact(&mut header).unwrap();
                assert_eq!(header[0], b'<');
                let mut p = vec![0; u16::from_le_bytes([header[1], header[2]]) as usize];
                device.read_exact(&mut p).unwrap();
                assert_eq!(p[0], expected);
                let responses = match expected {
                    4 => {
                        let mut contact = vec![0; 132];
                        contact[0] = 3;
                        contact[1..33].fill(0xab);
                        contact[100..109].copy_from_slice(b"ReVillage");
                        vec![vec![2, 1, 0, 0, 0], contact, vec![4, 0, 0, 0, 0]]
                    }
                    3 => {
                        assert_eq!(&p[7..], b"Hello public");
                        vec![vec![0]]
                    }
                    _ => {
                        assert_eq!(&p[7..13], &[0xab; 6]);
                        assert_eq!(&p[13..], b"Hello DM");
                        // A delivery receipt can arrive before command confirmation.
                        vec![
                            vec![0x82, 1, 2, 3, 4],
                            vec![6, 0, 1, 2, 3, 4, 0xe8, 3, 0, 0],
                        ]
                    }
                };
                for response in responses {
                    device.write_all(&[b'>', response.len() as u8, 0]).unwrap();
                    device.write_all(&response).unwrap();
                }
            }
        });
        let hub = std::sync::Arc::new(std::sync::Mutex::new(web::State::new(0)));
        hub.lock().unwrap().connected = true;
        let mut radio = Radio {
            port: client,
            frames: Frames::default(),
            recorder: None,
            metrics: None,
            web: Some(hub.clone()),
        };
        refresh_contacts(&mut radio).unwrap();
        assert_eq!(
            hub.lock().unwrap().contacts.values().next().unwrap().name,
            "ReVillage"
        );
        for (target, text, id) in [
            ("public", "Hello public", "public-request-1"),
            ("abababababab", "Hello DM", "direct-request-1"),
        ] {
            hub.lock()
                .unwrap()
                .enqueue(web::SendRequest {
                    request_id: id.into(),
                    target: target.into(),
                    text: text.into(),
                })
                .unwrap();
            service_web(&mut radio).unwrap();
        }
        let state = hub.lock().unwrap();
        assert_eq!(state.messages[0].status, "submitted");
        assert_eq!(state.messages[1].status, "delivered");
        worker.join().unwrap();
    }
}
