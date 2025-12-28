use meshtastic::api::StreamApi;
use meshtastic::protobufs::FromRadio;
use meshtastic::utils;

use std::error::Error;

mod recording_stream;
use recording_stream::RecordingStream;

use meshtastic::Message;


fn serialize_from_radio(msg: &FromRadio) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    if msg.encode(&mut buf).is_ok() {
        Some(buf)
    } else {
        None
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
env_logger::Builder::new()
    .filter_level(log::LevelFilter::Warn)
    .filter_module("meshtastic::connections::stream_buffer", log::LevelFilter::Error)
    .init();


    let port = "/dev/ttyACM0";
    println!("Connecting to Meshtastic on {port}");

    let stream_api = StreamApi::new();

    let serial_stream =
        utils::stream::build_serial_stream(port.to_string(), None, None, None)?;

    let (mut from_radio_rx, stream_api) =
        stream_api.connect(serial_stream).await;

    let config_id = utils::generate_rand_id();
    let _stream_api = stream_api.configure(config_id).await?;

    let mut recorder = RecordingStream::new("./recordings")?;

    while let Some(from_radio) = from_radio_rx.recv().await {
        if let Some(raw) = serialize_from_radio(&from_radio) {
            recorder.record(&raw)?;
        }
    }

    recorder.flush()?;
    Ok(())
}

