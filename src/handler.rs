use std::sync::atomic::{AtomicUsize, Ordering};
use meshtastic::protobufs::{FromRadio, mesh_packet::PayloadVariant, PortNum};

use crate::radio_message::{AppMessage, RadioMessage, Telemetry};

static TELEMETRY_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn handle_from_radio(msg: FromRadio) {
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
            log::warn!("Failed to parse FromRadio message from node {}: {:?}", msg.id, e);
        }
    }
}
