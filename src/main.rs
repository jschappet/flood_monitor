mod handler;
mod playback;
mod radio_message;
mod recording_stream;

use std::env;

use handler::handle_from_radio;
use playback::PlaybackStream;
use recording_stream::RecordingStream;

use meshtastic::api::StreamApi;
use meshtastic::utils;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Debug)
        .filter_module(
            "meshtastic::connections::stream_buffer",
            log::LevelFilter::Error,
        )
        .init();

    let args: Vec<String> = env::args().collect();

    /*
        Usage:

        Live mode (default):
            cargo run
            cargo run -- live

        Record mode:
            cargo run -- record recordings

        Playback mode:
            cargo run -- replay recordings/meshtastic-recording-00000.bin
    */

    match args.get(1).map(String::as_str) {
        Some("replay") => {
            let path = args.get(2).expect("missing replay file path");
            run_playback(path)?;
        }
        Some("record") => {
            let path = args.get(2).expect("missing record file path");
            run_record(path).await?;
        }
        _ => {
            run_live().await?;
        }
    }

    Ok(())
}

/* ---------------- Live Path ---------------- */

async fn run_live() -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting live Meshtastic stream…");

    let stream_api = StreamApi::new();
    let serial_stream =
        utils::stream::build_serial_stream("/dev/ttyACM0".to_string(), None, None, None)?;

    let (mut decoded_listener, stream_api) = stream_api.connect(serial_stream).await;
    let config_id = utils::generate_rand_id();
    let _stream_api = stream_api.configure(config_id).await?;

    while let Some(from_radio) = decoded_listener.recv().await {
        handle_from_radio(from_radio);
    }

    Ok(())
}

/* ---------------- Record Path ---------------- */

async fn run_record(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("Recording to: {}", path);

    let mut recorder = RecordingStream::new(path)?;

    let stream_api = StreamApi::new();
    let serial_stream =
        utils::stream::build_serial_stream("/dev/ttyACM0".to_string(), None, None, None)?;

    let (mut decoded_listener, stream_api) = stream_api.connect(serial_stream).await;
    let config_id = utils::generate_rand_id();
    let _stream_api = stream_api.configure(config_id).await?;

    while let Some(from_radio) = decoded_listener.recv().await {
        let raw = meshtastic::Message::encode_to_vec(&from_radio);
        recorder.record(&raw)?;

        handle_from_radio(from_radio);
    }

    recorder.flush()?;
    Ok(())
}

/* ---------------- Playback Path ---------------- */

fn run_playback(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    log::info!("Replaying capture from: {}", path);

    let playback = PlaybackStream::open(path)?;
    log::info!("Playback started");

    for msg in playback {
        let from_radio = msg?;
        //log::info!("Replayed FromRadio: {:?}", from_radio);
        handle_from_radio(from_radio);
    }

    Ok(())
}
