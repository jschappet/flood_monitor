mod playback;
mod recording_stream;
mod handler;

use std::env;

use handler::handle_from_radio;
use playback::PlaybackStream;

use meshtastic::api::StreamApi;
use meshtastic::utils;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    /*
        Usage:

        Live mode (default):
            cargo run
            cargo run -- live

        Playback mode:
            cargo run -- replay capture.bin
    */

    match args.get(1).map(String::as_str) {
        Some("replay") => {
            let path = args
                .get(2)
                .expect("missing replay file path");

            run_playback(path)?;
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

    let serial_stream = utils::stream::build_serial_stream(
        "/dev/ttyACM0".to_string(),
        None,
        None,
        None,
    )?;

    let (mut decoded_listener, stream_api) =
        stream_api.connect(serial_stream).await;

    let config_id = utils::generate_rand_id();
    let _stream_api = stream_api.configure(config_id).await?;

    while let Some(from_radio) = decoded_listener.recv().await {
        handle_from_radio(from_radio);
    }

    Ok(())
}

/* ---------------- Playback Path ---------------- */

fn run_playback(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("Replaying capture from: {}", path);

    let playback = PlaybackStream::open(path)?;

    for msg in playback {
        let from_radio = msg?;
        handle_from_radio(from_radio);
    }

    Ok(())
}

