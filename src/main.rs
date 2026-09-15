//! Standalone build: runs the leveler on a live audio device for testing outside a host.
//!
//!     cargo run --release -- --input-device "MacBook Pro Microphone" --output-device "MacBook Pro Speakers"
//!
//! See `--help` for the backend, sample-rate and period-size options.

use nih_plug::prelude::*;

use voicelevel::VoiceLevel;

fn main() {
    nih_export_standalone::<VoiceLevel>();
}
