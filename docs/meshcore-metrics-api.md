# MeshCore metrics: Heron contract and conformance

The receiver follows the supplied **Device enrollment and deployment** and
**IoT metrics and Arduino development** specifications. Heron accepts arbitrary
schema names and JSON data, so no MeshCore-specific backend change is required.

## Request

POST to the configured full IoT endpoint with JSON content type and
`Authorization: Bearer <persisted 64-character lowercase hexadecimal device token>`.
The credential is generated from 32 cryptographically random bytes. It is never
included in the request body, output, capture, or diagnostic metadata.

```json
{
  "device_id": "meshcore-schappet",
  "name": "Schappet MeshCore traffic",
  "ip_address": "127.0.0.1",
  "port": 80,
  "deployed_code": {"name": "flood-monitor-meshcore", "version": "0.1.0"},
  "capabilities": ["meshcore_message_count"],
  "reading": {
    "schema": "meshcore_message_count",
    "schema_version": 1,
    "report_id": "meshcore-example-1",
    "boot_id": "meshcore-example",
    "observed_at_unix": 1793145000,
    "time_quality": "estimated",
    "window_start_ms": 0,
    "window_end_ms": 300000,
    "window_duration_ms": 300000,
    "data": {"public_channel": 0, "public_count": 5, "dm_count": 2}
  }
}
```

The IP/port are compatibility registration fields, not an advertised working
sensor server. The example values are placeholders. Device identity must stay
stable; configuration paths are relative to the config file.

## Validation and measurement

The client validates bodies before enqueue and before POST:

- Body no larger than 16 KiB; object-valued `data` no larger than 12 KiB and depth
  no greater than eight below its root.
- Schema is 1–80 ASCII letters/digits/underscore/hyphen/dot; version is positive
  and at most 2,147,483,647. Device ID is at most 80 bytes; report/boot IDs at most
  160 bytes, nonempty, and without control characters.
- Window values are u32 milliseconds, with positive duration and wrapping
  `end - start == duration`. This metric uses 300,000 ms.
- Timestamp is positive for estimated/synchronized (or legacy missing quality),
  and zero only with explicit `unknown`.

`public_channel` is a byte, and `public_count`/`dm_count` are u32 counters.
The receiver counts plain/signed chat deliveries (text types 0/2) on the selected
public channel and DMs received by this companion. It excludes other channels,
CLI traffic, radio logs, adverts and malformed packets. It does not attempt to
observe DMs addressed to other nodes or deduplicate repeated legitimate text.
A radio-log event plus a decoded message counts once.

Five-minute windows begin after the initial companion queue drains. Idle windows
report zero. Startup backlog and replay are excluded. Reports do not contain chat
text or sender keys. Partial windows are not persisted; downtime is not reported
as observed zero traffic.

## Enrollment and delivery

The first metric (after five minutes) creates the provisional enrollment; no
separate discovery POST is necessary. Admin approval happens in Workshop `/devices/`.
A valid provisional 202 must contain the matching report ID, boolean
`metric_duplicate` and `sample_recorded`, sample count 0–5, and provisional status.
It removes the acknowledged local report even when the server deliberately does
not store it, then waits at least three hours. The client does not attempt to
control which five samples Heron retains. Server-discarded reports are counted
separately; duplicate sample acknowledgments are not counted as fresh losses.

Approved 201 requires approved status, matching report ID and boolean
`metric_duplicate`. Failed/mismatched acknowledgments retain the unchanged report.
429 retains it and observes a three-hour minimum delay. Numeric retry instructions
from JSON/Retry-After can extend provisional/429 waits (defensively bounded to seven
days). Even malformed 202 responses wait three hours, without acknowledging data.
401/403 retain the same token/report and back off five minutes; there is no
silent credential regeneration following revocation. Network/5xx/malformed normal
responses back off 30 seconds. Redirects are never followed.

400/409 save the latest rejected report and increment a persistent rejection
counter before advancing. The 2,048-report FIFO overwrites the oldest record on
capacity overflow and increments its dropped counter. In-flight acknowledgments
are matched by report ID; eviction never causes the next record to be removed.
These are bounded-storage semantics, not lossless delivery guarantees.

## Persistence and timing

Each backend URL/device ID uses its own configured outbox. Destination metadata
prevents replay to another identity or server. The token is stored separately
(mode 0600, directory 0700); report snapshots contain no credential. Missing or
corrupt credentials do not cause automatic re-enrollment. Use a fresh outbox and
explicit replacement approval if a credential is lost. Revocation and approval
remain server/admin responsibilities; the client only submits readings.

An exclusive OS file lock prevents multiple writers. Queue snapshots are
checksummed and atomically replaced with file/directory synchronization. They
persist FIFO order, counters, latest rejected data, HTTP/enrollment status and
backoff deadlines. A pending cooldown is restored on restart. Corruption or a
storage write failure stops mutations/uploads and preserves files; radio reception
continues. Runtime status reports missed windows while storage is disabled.

The Pi implementation uses a local disk snapshot rather than ESP32 Preferences,
LittleFS slots or Wi-Fi provisioning. It provides CLI/JSON diagnostics rather
than an ESP32 `/sensor` HTTP UI. Hardware-specific flashing, GPIO and Wi-Fi controls
in the supplied specs do not apply to this USB receiver. Heron-side quotas,
admin/member authorization, approval transactions and reverse-proxy deployment
are unchanged and outside this client's implementation.

A plausible system clock is marked estimated. An unset clock is unknown (zero).
An unknown same-process reading can acquire an estimated time before its first
POST. A reading from an earlier process remains unknown. The final payload is
saved before first submission; retry payloads remain identical across restarts.
Monotonic elapsed time defines durations even if the wall clock changes.

## Verification

Automated tests cover message filtering/window boundaries, persistent random
credentials and destination binding, exclusive queue access, envelope limits,
provisional/approved/error handling, restart backoff, duplicate/discard distinction,
revocation without token rotation, overflow, in-flight acknowledgment races,
rejected-record retention, corruption, storage failure and timestamp freezing.
A local HTTP test checks bearer authentication, unchanged retry payloads and
redirect rejection. Existing serial/capture tests still run.

For deployment: configure the HTTPS URL/CA, start the Pi monitor, approve the
provisional device on the correct host, and verify metrics at the next scheduled
check. Disconnect the network for multiple windows, reconnect, and test reboot
with a backlog. Hardware power-cut testing and deployed-server approval/revocation
have not been performed by these host tests. Do not publish device.token.
