use std::sync::atomic::{AtomicUsize, Ordering};
use meshtastic::protobufs::{FromRadio, mesh_packet::{PayloadVariant}, PortNum};

use crate::radio_message::RadioMessage;

static TELEMETRY_COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn handle_from_radio(msg: FromRadio) {
    log::debug!("FROM {} → payload present? {:?}", msg.id, msg.payload_variant);
    
    let rm = RadioMessage::try_from(&msg);

    match rm {
        Ok(rm) => {
            match rm.app {
                crate::radio_message::DecodedApp::Telemetry(_) => {
                    let count = TELEMETRY_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
                    log::info!("Telemetry messages received: {}", count);
                },
                crate::radio_message::DecodedApp::Position(_) => {
                    //let count = TELEMETRY_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
                    log::info!("Position messages received");
                },
                _ => {
                },
            }
            log::info!("Received message from node {}: {:?}", rm.node_id, rm.app);
        },
        Err(e) => {
            log::trace!("Unable to parse FromRadio message from node {}: {:?}", msg.id, e);
        },
    }
    
    //log::info!("Received message from node {}: {:?}", rm.node_id, rm.message);

}
