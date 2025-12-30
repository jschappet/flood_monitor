use meshtastic::{Message, protobufs::telemetry};

use std::convert::TryFrom;
use meshtastic::protobufs::{
    from_radio::PayloadVariant as FromRadioPayload,
    mesh_packet::PayloadVariant as MeshPayload,
    FromRadio, Data, PortNum,
};

#[derive(Debug, Clone, PartialEq)]
pub struct TextMessage {
    pub to: Option<String>,
    pub from: Option<String>,
    pub msg: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppMessage {
    Telemetry(Telemetry),
    Position(Position),
    Text(TextMessage),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Position {
    pub latitude: f64,  // in degrees
    pub longitude: f64, // in degrees
    pub altitude: i32,  // in meters
    pub accuracy: u32,  // in meters
    pub speed: f32,     // in m/s
    pub heading: f32,   // in degrees
}

impl TryFrom<&[u8]> for Position {
    type Error = DecodeError;

    fn try_from(payload: &[u8]) -> Result<Self, Self::Error> {
        let proto_pos = meshtastic::protobufs::Position::decode(payload)
            .map_err(|_| DecodeError::PositionDecodeError)?;

                Ok(Self {
            latitude: proto_pos.latitude_i.map(|lat| lat as f64 / 1e7).unwrap_or(0.0),
            longitude: proto_pos.longitude_i.map(|lon| lon as f64 / 1e7).unwrap_or(0.0),
            altitude: proto_pos.altitude.unwrap_or(0),
            accuracy: proto_pos.gps_accuracy,
            speed: proto_pos.ground_speed.map(|speed| speed as f32 / 1e7).unwrap_or(0.0),
            heading: 0.0, // TODO: extract heading if available
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Telemetry {
    Device {
        battery_level: Option<u32>,
        voltage: Option<f32>,
        uptime_seconds: Option<u32>,
    },
    Environment {
        temperature: Option<f32>,
        humidity: Option<f32>,
        pressure: Option<f32>,
    },
    Power {
        voltage: Option<f32>,
        current: Option<f32>,
    },
}


impl TryFrom<&[u8]> for Telemetry {
    type Error = DecodeError;

    fn try_from(payload: &[u8]) -> Result<Self, Self::Error> {
        use meshtastic::protobufs::{
            DeviceMetrics,
            EnvironmentMetrics,
            PowerMetrics,
        };

        // Try DeviceMetrics first
        if let Ok(dm) = DeviceMetrics::decode(payload) {
            return Ok(Telemetry::Device {
                battery_level: dm.battery_level,
                voltage: dm.voltage,
                uptime_seconds: dm.uptime_seconds,
            });
        }

        // Then EnvironmentMetrics
        if let Ok(env) = EnvironmentMetrics::decode(payload) {
            return Ok(Telemetry::Environment {
                temperature: env.temperature,
                humidity: env.relative_humidity,
                pressure: env.barometric_pressure,
            });
        }

        // Then PowerMetrics
        if let Ok(pwr) = PowerMetrics::decode(payload) {
            return Ok(Telemetry::Power {
                voltage: pwr.ch1_voltage,
                current: pwr.ch1_current,
            });
        }

        log::warn!(
            "Failed to decode telemetry payload as any known telemetry type: {:?}",
            payload
        );

        Err(DecodeError::TelemetryDecodeError)
    }
}

/* 

impl TryFrom<&[u8]> for Telemetry {
    type Error = DecodeError;

    fn try_from(payload: &[u8]) -> Result<Self, Self::Error> {


        use meshtastic::protobufs::DeviceMetrics ;

        let proto_pos = DeviceMetrics::decode(payload)
            .map_err(|e| { 
                log::warn!("Failed to decode telemetry payload: {:?} {:?}", payload, e.to_string());
                
                DecodeError::TelemetryDecodeError
            })?;
        
        Ok(Self {
            battery_level: proto_pos.battery_level,
            voltage: proto_pos.voltage,
            uptime_seconds: proto_pos.uptime_seconds,
        })
    }
}
*/

#[derive(Debug, Clone)]
pub struct RadioMessage {
    pub node_id: u32,
    pub portnum: PortNum,
    pub app: AppMessage,
}

impl TryFrom<&FromRadio> for RadioMessage {
    type Error = DecodeError;

    fn try_from(msg: &FromRadio) -> Result<Self, Self::Error> {
        let node_id = msg.id;

        // Extract MeshPacket from FromRadio
        let mesh_packet = match &msg.payload_variant {
            Some(FromRadioPayload::Packet(p)) => p,
            _ => return Err(DecodeError::MeshPacketDecodeError),
        };

        // Extract the inner Data payload
        let data = match &mesh_packet.payload_variant {
            Some(MeshPayload::Decoded(d)) => d,
            _ => return Err(DecodeError::ExtractDecodeError),
        };

        let portnum = PortNum::from_i32(data.portnum).ok_or(DecodeError::CouldNotGetPortNum)?;

        // Decode based on the port type
        let payload = &data.payload[..];

        let app = match portnum {
            PortNum::TelemetryApp => {
                
                let telemetry = Telemetry::try_from(payload)?;
                AppMessage::Telemetry(telemetry)
            }
            PortNum::PositionApp => {
                let pos = Position::try_from(payload)?;
                AppMessage::Position(pos)
            }
            PortNum::TextMessageApp => {
                // For simplicity, decode payload as UTF-8 string; real implementation may parse structured fields
                let msg_str = String::from_utf8_lossy(payload).to_string();
                log::trace!("Decoded text message: {}", msg_str);
                let text_msg = TextMessage {
                    to: None,   // populate if your protocol supplies
                    from: None, // populate if your protocol supplies
                    msg: msg_str,
                };
                AppMessage::Text(text_msg)
            }
            _ => return Err(DecodeError::UnsupportedPort(portnum)),
        };

        Ok(Self {
            node_id,
            portnum,
            app,
        })
    }
}

/* 

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

 */


#[derive(Debug, PartialEq)]
pub enum DecodeError {
    CouldNotGetPortNum,
    UnsupportedPort(PortNum),
    MeshPacketDecodeError,
    ExtractDecodeError,
    TelemetryAppError,
    TelemetryDecodeError,
    PositionDecodeError,
    PositionAppError,
    ExtractedData,
    LocalSystemMessage,
    //ProtobufDecodeError(prost::error::DecodeError),
}

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
            AppMessage::Text(_) => {}
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

        // match msg.app {
        //     AppMessage::Telemetry(dm) => {
        //         assert_eq!(dm.battery_level , Some(87));
        //     }
        //     _ => panic!("Expected telemetry"),
        // }
    }
}
