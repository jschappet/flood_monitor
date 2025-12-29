use meshtastic::protobufs::from_radio::PayloadVariant as FromRadioPayload;
use meshtastic::protobufs::mesh_packet::PayloadVariant as MeshPayload;
use meshtastic::protobufs::{Data, FromRadio, PortNum};

use meshtastic::Message;

fn extract_data(msg: &FromRadio) -> Result<(PortNum, &[u8]), DecodeError> {
    let packet = match &msg.payload_variant {
        Some(FromRadioPayload::Packet(p)) => p,
        _ => return Err(DecodeError::ExtractedData),
    };

    let data = match &packet.payload_variant {
        Some(MeshPayload::Decoded(d)) => d,
        _ => return Err(DecodeError::ExtractedData),
    };

    let portnum = PortNum::from_i32(data.portnum).ok_or(DecodeError::ExtractedData)?;

    Ok((portnum, &data.payload))
}

/* #[derive(Debug)]
pub enum MessageType {
    Telemetry(Data),
    Text(Data),
    Position(Data),
    Other(Data),
} */

use meshtastic::protobufs::{DeviceMetrics, Position};

#[derive(Debug, PartialEq)]
pub enum DecodedApp {
    Telemetry(DeviceMetrics),
    Position(Position),
    Text(Data),
}

#[derive(Debug, PartialEq)]
pub enum DecodeError {
    UnsupportedPort(PortNum),
    TelemetryAppError,
    PositionAppError,
    ExtractedData,
    LocalSystemMessage,
    //ProtobufDecodeError(prost::error::DecodeError),
}

#[derive(Debug)]
pub struct RadioMessage {
    pub node_id: u32,
    pub portnum: PortNum,
    //pub message: MessageType,
    pub app: DecodedApp,
}

impl TryFrom<&FromRadio> for RadioMessage {
    type Error = DecodeError;

    fn try_from(msg: &FromRadio) -> Result<Self, Self::Error> {
        if msg.id == 0 {
            return Err(DecodeError::LocalSystemMessage);
        }

        let (portnum, payload) = extract_data(msg)?;
        log::trace!(
            "Decoding RadioMessage from node {} on port {:?}",
            msg.id,
            portnum
        );

        let app = match portnum {
            PortNum::TelemetryApp => {
                log::debug!("Decoding telemetry message from node {}", msg.id);
                let decoded =
                    DeviceMetrics::decode(payload).map_err(|_| DecodeError::TelemetryAppError)?;
                DecodedApp::Telemetry(decoded)
            }
            PortNum::PositionApp => {
                let decoded =
                    Position::decode(payload).map_err(|_| DecodeError::PositionAppError)?;
                DecodedApp::Position(decoded)
            }
            PortNum::TextMessageApp => DecodedApp::Text(Data {
                portnum: portnum as i32,
                payload: payload.to_vec(),
                want_response: false,
                dest: 0,
                source: 0,
                request_id: 0,
                reply_id: 0,
                emoji: 0,
                bitfield: None,
            }),

            _ => return Err(DecodeError::UnsupportedPort(portnum)),
        };

        Ok(Self {
            node_id: msg.id,
            portnum,
            app,
        })
    }
}

/*
pub fn from_radio(msg: &FromRadio) -> Option<Self> {
    let packet = match &msg.payload_variant {
        Some(FromRadioPayload::Packet(p)) => p,
        _ => return None,
    };

    let data = match &packet.payload_variant {
        Some(MeshPayload::Decoded(d)) => d,
        _ => return None,
    };

    let portnum = PortNum::from_i32(data.portnum)?;

    let app = match portnum {
        PortNum::TelemetryApp => DecodedApp::Telemetry(data.clone()),
        PortNum::TextMessageApp => DecodedApp::Text(data.clone()),
        PortNum::PositionApp => DecodedApp::Position(data.clone()),
        _ => MessageType::Other(data.clone()),
    };

    Some(Self {
        node_id: packet.from,
        portnum,
        app,
    })
    */

#[cfg(test)]
mod tests {
    use std::env;

    use super::*;

    use meshtastic::protobufs::from_radio::PayloadVariant as FromRadioPayload;
    use meshtastic::protobufs::mesh_packet::PayloadVariant as MeshPayload;
    use meshtastic::protobufs::{Data, FromRadio, MeshPacket, PortNum};

    fn init_test_logging() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Debug)
            .try_init();
    }

    fn make_from_radio(portnum: PortNum) -> FromRadio {
        let data = Data {
            portnum: portnum as i32,
            payload: vec![1, 2, 3], // arbitrary, not decoded here
            want_response: false,
            dest: 0,
            source: 0,
            request_id: 0,
            reply_id: 0,
            emoji: 0,
            bitfield: None,
        };

        let packet = MeshPacket {
            from: 42,
            to: 0,
            channel: 0,
            id: 123,
            rx_time: 0,
            rx_snr: 0.0,
            hop_limit: 0,
            want_ack: false,
            priority: 0,
            rx_rssi: 0,
            delayed: 0,
            via_mqtt: false,
            hop_start: 0,
            public_key: vec![],
            pki_encrypted: false,
            next_hop: 0,
            relay_node: 0,
            tx_after: 0,
            transport_mechanism: 0,
            payload_variant: Some(MeshPayload::Decoded(data)),
        };

        FromRadio {
            id: 99,
            payload_variant: Some(FromRadioPayload::Packet(packet)),
        }
    }

    #[test]
    fn detects_telemetry_decode_error() {
        init_test_logging();

        let msg = make_from_radio(PortNum::TelemetryApp);
        let radio_msg = RadioMessage::try_from(&msg);
        assert!(radio_msg.is_err());
    }

    #[test]
    fn classifies_other_ports() {
        init_test_logging();
        let msg = make_from_radio(PortNum::TextMessageApp);
        let radio_msg = RadioMessage::try_from(&msg).unwrap();

        match radio_msg.app {
            DecodedApp::Text(_) => {}
            _ => panic!("expected text message"),
        }
    }
    //use super::*;
    use meshtastic::Message;

    use meshtastic::protobufs::{DeviceMetrics, from_radio::PayloadVariant};

    #[test]
    fn decodes_telemetry_message() {
        init_test_logging();
        let metrics = DeviceMetrics {
            battery_level: Some(87),
            voltage: Some(4.12),
            uptime_seconds: Some(12345),
            ..Default::default()
        };

        let mut payload = Vec::new();
        metrics.encode(&mut payload).unwrap();

        let data = Data {
            portnum: PortNum::TelemetryApp as i32,
            payload,
            ..Default::default()
        };

        let packet = MeshPacket {
            payload_variant: Some(MeshPayload::Decoded(data)),
            ..Default::default()
        };

        let from_radio = FromRadio {
            id: 42,
            payload_variant: Some(PayloadVariant::Packet(packet)),
        };

        let msg = RadioMessage::try_from(&from_radio).unwrap();

        match msg.app {
            DecodedApp::Telemetry(dm) => {
                assert_eq!(dm.battery_level, Some(87));
            }
            _ => panic!("Expected telemetry"),
        }
    }
}
