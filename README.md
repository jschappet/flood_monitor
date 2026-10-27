# flood_monitor

Rust receiver and recorder for a MeshCore **USB/Serial Companion** radio.
Developed against the RAK4631 v1.18.0 companion responses captured on a Raspberry Pi.
Bluetooth companion and repeater firmware do not provide this USB companion interface.

## Run on the Raspberry Pi

The project uses Rust edition 2021. Use a current stable Rust toolchain. From the repository root:

```sh
cargo build --release --locked
./target/release/flood_monitor live --port /dev/ttyACM0
```

The port defaults to `/dev/ttyACM0`. Override it with `--port` or the
`MESHCORE_PORT` environment variable. A path under `/dev/serial/by-id/` is useful
when several USB devices are connected. The connection uses 115200 baud, 8N1.
If access is denied, check the serial device permissions and your user's `dialout`
group membership. Close other applications using the port before starting.

Startup prints JSON lines for the node name/public key and firmware/model/build.
It then retrieves queued direct and channel messages, including V3 SNR values.
Receiving removes messages from the companion's queue, as other companion clients do.
The application does not change radio settings, channels, or firmware, and does
not send chat messages or adverts over the mesh.

```sh
# Receive and save incoming frames:
./target/release/flood_monitor record recordings --port /dev/ttyACM0

# Replay without a radio:
./target/release/flood_monitor replay recordings/meshcore-recording-00000.bin

# Usage:
./target/release/flood_monitor --help
```

Press Ctrl-C to stop. Each received frame is flushed to the recording immediately.
Record mode starts a new file on each run and rotates at 10 MiB without overwriting
existing captures. All received frames are saved, including queue notifications,
unknown packets, and malformed application payloads. Console output suppresses
routine queue-empty and message-waiting notifications.

## Current scope

- USB initialization, device information, direct/channel messages, battery response decoding.
- One outstanding command at a time; drain queued messages and poll every two seconds
  when idle. Notifications trigger earlier retrieval.
- Other events (including telemetry and adverts) are displayed as raw hexadecimal
  payloads and preserved in recordings. Sensor telemetry requests, subscriptions,
  and sensor-specific decoding are not implemented yet.
- This is a companion receiver, not a promiscuous decoder of all mesh traffic.
- On serial failure or a command timeout, the program exits with an error; restart
  after reconnecting. Automatic reconnection is not implemented.
- Meshtastic code has been removed. Old Meshtastic captures are rejected rather
  than interpreted as MeshCore packets.

## Capture format (version 1)

Files start with eight bytes `4d 43 4f 52 45 00 01 00` (`MCORE`, zero, version 1,
zero). Each record contains a little-endian u64 Unix reception timestamp in seconds,
a little-endian u32 payload length, and the companion payload (without the USB
`>`/length header). Frame lengths must be 1–176 bytes. Replay runs immediately,
adds `recorded_at` to output, and reports truncated/corrupt files as errors.
Recordings contain received message content and device information; store them accordingly.

## Verification

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked
```

The Unix integration test simulates a radio using a local byte stream. It
uses the actual RAK4631 startup responses, interleaves notifications, splits
frames across read timeouts, checks message queue polling, and compares recorded
replay with live output. No hardware is required. Physical Pi/radio validation
is still required after deploying the code there.

Protocol references:
- [Companion protocol](https://github.com/meshcore-dev/MeshCore/blob/main/docs/companion_protocol.md)
- [USB serial framing](https://github.com/meshcore-dev/MeshCore/blob/main/src/helpers/ArduinoSerialInterface.cpp)
