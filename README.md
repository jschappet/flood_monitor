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

## Five-minute message metrics and device enrollment

Reporting uses Heron's generic IoT envelope and its device-enrollment lifecycle.
No shared token or backend schema change is needed. Configure the endpoint and
stable device identity using [metrics.example.json](metrics.example.json):

```sh
cp metrics.example.json metrics.local.json
# Edit the HTTPS endpoint, device identity, and public channel in metrics.local.json.
cargo run --release -- live --port /dev/ttyACM0 --metrics-config metrics.local.json
# Optional raw traffic recording at the same time:
cargo run --release -- record recordings --port /dev/ttyACM0 --metrics-config metrics.local.json
```

The first completed five-minute window submits a provisional enrollment. Sign in
to Workshop as an admin on that host, open `/devices/`, inspect the device metadata
and sample, and approve it. The monitor waits three hours after each provisional
response; approval takes effect at its next scheduled check. It keeps collecting
windows during this wait. Heron keeps up to five provisional samples; additional
acknowledged samples are discarded by the server and counted in local diagnostics.

The monitor generates 32 cryptographically random bytes, hex-encodes them, and
stores the credential in `spool_dir/device.token` with mode 0600 inside a 0700
directory. The token is never printed or included in report metadata. Keep this
file when updating/restarting the application. The outbox binds it to the full
backend URL and device ID. Use separate outboxes for DEV/PROD. A missing credential
in an existing queue, corrupt state, or changed destination disables reporting
and preserves files. Replacing a lost/revoked token requires a new outbox and
explicit replacement approval in Workshop; the client never rotates it silently.
`IOT_DEVICE_TOKEN` is no longer used. Device credentials are upload-only.

The full endpoint must use HTTPS, except localhost development URLs. Certificates
are verified and redirects are disabled. Public CA certificates work by default;
set optional `root_ca_file` to a PEM CA file for a private CA (relative to the
configuration file). The example IP/port are compatibility placeholders for the
registration envelope; this receiver does not run an ESP32-style `/sensor` server.

### What is counted

Each complete 300-second window reports `public_count` and `dm_count`, including
zeros. Public channel index defaults to 0 and is configurable. Only received plain
or signed chat messages count; other channels, CLI traffic, malformed packets,
adverts and RF log entries are excluded. These are deliveries to this companion,
not all traffic on the mesh. Message text and sender identities are not uploaded.
Startup queue backlog is displayed/recorded but excluded from metrics. Replay
never enrolls or posts. Windows start after startup backlog drains, not on
wall-clock five-minute boundaries. An active partial window is lost on exit.

### Buffering and diagnostics

The disk queue retains 2,048 completed windows (7 days, 2 hours, 40 minutes).
When full, the oldest window is overwritten and the persistent `dropped` counter
increases. HTTP runs on a separate thread. Only matching acknowledgments remove
reports, except explicit permanent rejection or overflow:

- Approved 201: remove the acknowledged report and continue after about 250 ms.
- Provisional 202: remove the acknowledged report and wait at least 10,800 seconds,
  including when `sample_recorded` is false. Genuine discarded samples increment
  `provisional_discarded`; duplicate acknowledgments do not.
- Rate limit 429: retain the report and wait at least 10,800 seconds.
- Authentication 401/403: retain the credential/report and retry after five minutes.
- Permanent 400/409: retain the latest rejected payload in the snapshot, increment
  `rejected`, and advance the queue.
- Network/server/invalid acknowledgment errors: retain and retry after 30 seconds.

Backoff deadlines survive restart; restarting does not bypass a pending approval
wait. Snapshots are checksummed, written atomically and synchronized to disk.
One process owns an outbox at a time. On a storage fault queue mutations and
uploads stop; radio reception continues and missed windows are counted for the
current process. Old experimental shared-token `report-*.json` queues are
preserved and require a fresh outbox for this enrollment implementation.

JSON `metrics_status` events show queue depth/capacity, dropped/rejected/discarded
counts, current storage health, missed windows, enrollment state, last HTTP status
and retry deadline. Read the latest durable snapshot without connecting to a radio:

```sh
cargo run --release -- metrics-status --metrics-config metrics.local.json
```

Snapshot status cannot establish that a running process is healthy: use its live
status/logs for current storage faults and missed windows. No token is displayed.

Reports include explicit `time_quality`. A plausible Pi system clock is labeled
`estimated`, never assumed to be NTP-synchronized. An unset clock gives timestamp
zero and `unknown`. Before first upload, an unknown reading from the same process
can be estimated from the now-valid clock and elapsed time; after restart it
remains unknown. Values, timestamp and report ID are frozen durably before the
first POST so retries remain identical. Keep the Pi clock synchronized.

See [the API contract and conformance notes](docs/meshcore-metrics-api.md) for
payloads, limits and platform-specific scope. Run `cargo test --locked` for
counting, credential persistence, enrollment responses, backoff, overflow,
rejection, corruption, timestamp freezing and local HTTP integration tests.
Physical Pi/power-loss and deployed Heron acceptance tests remain necessary.
