use meshtastic::protobufs::{DeviceMetrics, FromRadio, PortNum, mesh_packet::PayloadVariant};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::radio_message::{AppMessage, RadioMessage, Telemetry};

static TELEMETRY_COUNT: AtomicUsize = AtomicUsize::new(0);
use meshtastic::Message;

pub fn handle_from_radio(msg: FromRadio) {
    let payload = &msg.payload_variant.clone();
    match payload.as_ref().unwrap() {
        meshtastic::protobufs::from_radio::PayloadVariant::Channel(channel) => {
            log::info!("Received channel packet: {:?}", channel);
            // You can handle channel-specific logic here
        }
        meshtastic::protobufs::from_radio::PayloadVariant::NodeInfo(node_info) => {
            //log::info!("Received node info packet: {:?}", node_info);
            node_info.user.as_ref().map(|user| {
                log::info!("Node User Info: {:?}", user);
            });
            node_info.device_metrics.as_ref().map(|dm| {
                log::info!("Node Device Metrics: {:?}", dm);
            });
            node_info.position.as_ref().map(|pos| {
                log::info!("Node Position: {:?}", pos);
            });
            // Handle node info logic here
        }
        meshtastic::protobufs::from_radio::PayloadVariant::Packet(mesh_packet) => {
            log::info!("Received mesh packet: {:?}", mesh_packet);

            if let Some(pv) = &mesh_packet.payload_variant {
                match pv {
                    meshtastic::protobufs::mesh_packet::PayloadVariant::Decoded(data) => {
                        let port = meshtastic::protobufs::PortNum::from_i32(data.portnum);
                        use meshtastic::protobufs::{
                            DeviceMetrics, EnvironmentMetrics, PowerMetrics,
                        };

                        if let Some(meshtastic::protobufs::PortNum::TelemetryApp) = port {
                            // Try decoding as DeviceMetrics
                            let decoded = DeviceMetrics::decode(&data.payload[..])
                                .map(|dm| {
                                    log::info!("Decoded Device Metrics → {:?}", dm);
                                    //dm
                                })
                                .or_else(|_| {
                                    EnvironmentMetrics::decode(&data.payload[..]).map(|em| {
                                        log::info!("Decoded Environment Metrics → {:?}", em);
                                        //em
                                    })
                                })
                                .or_else(|_| {
                                    PowerMetrics::decode(&data.payload[..]).map(|pm| {
                                        log::info!("Decoded Power Metrics → {:?}", pm);
                                        //pm
                                    })
                                });

                            if decoded.is_err() {
                                log::trace!(
                                    "Telemetry payload could not be decoded, storing raw: {}",
                                    data.payload
                                        .iter()
                                        .map(|b| format!("{:02X}", b))
                                        .collect::<Vec<_>>()
                                        .join(" ")
                                );
                            }
                        }
                    }
                    _ => log::trace!("Unhandled mesh packet payload variant"),
                }
            }
        }
        _ => {
            log::trace!("Unhandled FromRadio payload variant");
        }
    }

    /*
    match RadioMessage::try_from(&msg) {
        Ok(rm) => match &rm.app {
            AppMessage::Telemetry(tel) => {
                // Increment global telemetry counter
                let count = TELEMETRY_COUNT.fetch_add(1, Ordering::SeqCst) + 1;

                // Pattern match on the specific telemetry variant
                match tel {
                    Telemetry::Device {
                        battery_level,
                        voltage,
                        uptime_seconds,
                    } => {
                        log::info!(
                            "Node {} Device Telemetry #{} → Voltage: {:?} V, Battery: {:?} %, Uptime: {:?} s",
                            rm.node_id,
                            count,
                            voltage,
                            battery_level,
                            uptime_seconds
                        );
                    }
                    Telemetry::Environment {
                        temperature,
                        humidity,
                        pressure,
                    } => {
                        log::info!(
                            "Node {} Environment Telemetry #{} → Temp: {:?} °C, Humidity: {:?} %, Pressure: {:?} hPa",
                            rm.node_id,
                            count,
                            temperature,
                            humidity,
                            pressure
                        );
                    }
                    Telemetry::Power { voltage, current } => {
                        log::info!(
                            "Node {} Power Telemetry #{} → Voltage: {:?} V, Current: {:?} A",
                            rm.node_id,
                            count,
                            voltage,
                            current
                        );
                    }
                }
            }
            AppMessage::Position(pos) => {
                log::info!(
                    "Node {} Position → Lat: {:.7}, Lon: {:.7}, Alt: {} m, Accuracy: {} m",
                    rm.node_id,
                    pos.latitude,
                    pos.longitude,
                    pos.altitude,
                    pos.accuracy
                );
            }
            AppMessage::Text(text) => {
                log::info!(
                    "Node {} Text → From: {:?}, To: {:?}, Msg: {}",
                    rm.node_id,
                    text.from,
                    text.to,
                    text.msg
                );
            }
        },
        Err(e) => {
            log::trace!("Failed to parse FromRadio message from node {}: {:?}", msg.id, e);
        }
    }
    */
}
