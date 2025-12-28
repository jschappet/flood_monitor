use meshtastic::protobufs::FromRadio;

pub fn handle_from_radio(msg: FromRadio) {
    // For now: just log what we got
    println!("FROM {:?} → {:?}", msg.id, msg.payload_variant);

    // Later:
    // - decode telemetry
    // - update node registry
    // - emit events
    // - persist derived metrics
}

