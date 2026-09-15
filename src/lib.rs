//! VoiceLevel: an attenuation-only automatic microphone leveler.
//!
//! A virtual fader for a live mic. It only ever turns the mic *down* — it never applies gain,
//! so it cannot push a channel into feedback — and it has no look-ahead, so it adds no latency.
//! The fader moves at an operator-like speed towards a target level, holds still through pauses,
//! and can be parked at a learned position for the rest of the show.

pub mod dsp;
mod editor;

use nih_plug::prelude::*;
use nih_plug_egui::EguiState;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use dsp::{Leveler, Mode, Settings};

pub struct VoiceLevel {
    params: Arc<VoiceLevelParams>,
    leveler: Leveler,
    meters: Arc<Meters>,
}

/// Values the audio thread publishes for the editor. All in dB; `NAN` where there is nothing to
/// show yet.
pub struct Meters {
    /// Fast input level (what a meter shows).
    pub in_db: AtomicF32,
    /// Gated speech level (what steers the fader).
    pub speech_db: AtomicF32,
    /// Fast output level.
    pub out_db: AtomicF32,
    /// Seconds of speech measured by the current Learn.
    pub learn_seconds: AtomicF32,
    /// Running level of the current Learn.
    pub learn_level_db: AtomicF32,
    /// Level the last committed Learn measured.
    pub learned_level_db: AtomicF32,
}

impl Default for Meters {
    fn default() -> Self {
        Self {
            in_db: AtomicF32::new(dsp::FLOOR_DB),
            speech_db: AtomicF32::new(dsp::FLOOR_DB),
            out_db: AtomicF32::new(dsp::FLOOR_DB),
            learn_seconds: AtomicF32::new(0.0),
            learn_level_db: AtomicF32::new(f32::NAN),
            learned_level_db: AtomicF32::new(f32::NAN),
        }
    }
}

#[derive(Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeParam {
    #[id = "auto"]
    Auto,
    #[id = "fixed"]
    Fixed,
    #[id = "learn"]
    Learn,
}

impl From<ModeParam> for Mode {
    fn from(mode: ModeParam) -> Self {
        match mode {
            ModeParam::Auto => Mode::Auto,
            ModeParam::Fixed => Mode::Fixed,
            ModeParam::Learn => Mode::Learn,
        }
    }
}

#[derive(Params)]
pub struct VoiceLevelParams {
    #[persist = "editor-state"]
    pub editor_state: Arc<EguiState>,

    /// The fader position: dB of attenuation, >= 0. Not a host parameter on purpose — the audio
    /// thread writes it in Auto mode and on a Learn commit, the editor writes it in Fixed mode,
    /// and the host restores it with the rest of the state.
    #[persist = "fader"]
    pub fader_db: Arc<AtomicF32>,

    #[id = "target"]
    pub target: FloatParam,
    #[id = "speed"]
    pub speed: FloatParam,
    #[id = "recovery"]
    pub recovery: FloatParam,
    #[id = "range"]
    pub range: FloatParam,
    #[id = "hold"]
    pub hold_below: FloatParam,
    #[id = "averaging"]
    pub averaging: FloatParam,
    #[id = "mode"]
    pub mode: EnumParam<ModeParam>,
}

impl Default for VoiceLevelParams {
    fn default() -> Self {
        let d = Settings::default();
        Self {
            editor_state: EguiState::from_size(editor::WIDTH, editor::HEIGHT),
            fader_db: Arc::new(AtomicF32::new(0.0)),

            target: FloatParam::new(
                "Target",
                d.target_db,
                FloatRange::Linear {
                    min: -40.0,
                    max: 0.0,
                },
            )
            .with_unit(" dBFS")
            .with_step_size(0.5)
            .with_value_to_string(formatters::v2s_f32_rounded(1)),

            speed: FloatParam::new(
                "Speed",
                d.speed_db_per_s,
                FloatRange::Skewed {
                    min: 1.0,
                    max: 100.0,
                    factor: FloatRange::skew_factor(-1.5),
                },
            )
            .with_unit(" dB/s")
            .with_value_to_string(formatters::v2s_f32_rounded(1)),

            recovery: FloatParam::new(
                "Recovery",
                d.recovery_db_per_s,
                FloatRange::Skewed {
                    min: 0.1,
                    max: 50.0,
                    factor: FloatRange::skew_factor(-1.5),
                },
            )
            .with_unit(" dB/s")
            .with_value_to_string(formatters::v2s_f32_rounded(1)),

            range: FloatParam::new(
                "Range",
                d.range_db,
                FloatRange::Linear {
                    min: 0.0,
                    max: 60.0,
                },
            )
            .with_unit(" dB")
            .with_step_size(1.0)
            .with_value_to_string(formatters::v2s_f32_rounded(0)),

            hold_below: FloatParam::new(
                "Hold below",
                d.hold_below_db,
                FloatRange::Linear {
                    min: -80.0,
                    max: -20.0,
                },
            )
            .with_unit(" dBFS")
            .with_step_size(1.0)
            .with_value_to_string(formatters::v2s_f32_rounded(0)),

            averaging: FloatParam::new(
                "Averaging",
                d.averaging_ms,
                FloatRange::Skewed {
                    min: 50.0,
                    max: 3000.0,
                    factor: FloatRange::skew_factor(-1.0),
                },
            )
            .with_unit(" ms")
            .with_step_size(10.0)
            .with_value_to_string(formatters::v2s_f32_rounded(0)),

            mode: EnumParam::new("Mode", ModeParam::Auto),
        }
    }
}

impl VoiceLevelParams {
    fn settings(&self) -> Settings {
        Settings {
            target_db: self.target.value(),
            speed_db_per_s: self.speed.value(),
            recovery_db_per_s: self.recovery.value(),
            range_db: self.range.value(),
            hold_below_db: self.hold_below.value(),
            averaging_ms: self.averaging.value(),
            mode: self.mode.value().into(),
        }
    }
}

impl Default for VoiceLevel {
    fn default() -> Self {
        Self {
            params: Arc::new(VoiceLevelParams::default()),
            leveler: Leveler::new(48_000.0),
            meters: Arc::new(Meters::default()),
        }
    }
}

impl Plugin for VoiceLevel {
    const NAME: &'static str = "VoiceLevel";
    const VENDOR: &'static str = "Stoatworks";
    const URL: &'static str = "https://github.com/stoatworks-labs/voicelevel";
    const EMAIL: &'static str = "";

    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(1),
            main_output_channels: NonZeroU32::new(1),
            ..AudioIOLayout::const_default()
        },
        AudioIOLayout {
            main_input_channels: NonZeroU32::new(2),
            main_output_channels: NonZeroU32::new(2),
            ..AudioIOLayout::const_default()
        },
    ];

    const MIDI_INPUT: MidiConfig = MidiConfig::None;
    const MIDI_OUTPUT: MidiConfig = MidiConfig::None;
    const SAMPLE_ACCURATE_AUTOMATION: bool = false;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        editor::create(self.params.clone(), self.meters.clone())
    }

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        context: &mut impl InitContext<Self>,
    ) -> bool {
        self.leveler.set_sample_rate(buffer_config.sample_rate);
        // No look-ahead anywhere: the host must not compensate for anything.
        context.set_latency_samples(0);
        true
    }

    fn reset(&mut self) {
        self.leveler.reset();
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        _context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let params = &self.params;

        // The persisted fader is the source of truth between blocks: the editor (Fixed mode) or
        // a host state restore may have moved it. Load it before applying settings so that a
        // Learn commit inside `set_settings` wins over whatever was there.
        let loaded = params.fader_db.load(Ordering::Relaxed);
        self.leveler.set_fader_db(loaded);
        self.leveler.set_settings(params.settings());

        self.leveler.process(buffer.as_slice());

        let fader = self.leveler.fader_db();
        if fader != loaded {
            params.fader_db.store(fader, Ordering::Relaxed);
        }

        let meters = &self.meters;
        meters
            .in_db
            .store(self.leveler.level_db(), Ordering::Relaxed);
        meters
            .speech_db
            .store(self.leveler.speech_level_db(), Ordering::Relaxed);
        meters
            .out_db
            .store(self.leveler.out_level_db(), Ordering::Relaxed);
        meters
            .learn_seconds
            .store(self.leveler.learn_seconds(), Ordering::Relaxed);
        meters.learn_level_db.store(
            self.leveler.learn_level_db().unwrap_or(f32::NAN),
            Ordering::Relaxed,
        );
        meters.learned_level_db.store(
            self.leveler.learned_level_db().unwrap_or(f32::NAN),
            Ordering::Relaxed,
        );

        ProcessStatus::Normal
    }
}

impl ClapPlugin for VoiceLevel {
    const CLAP_ID: &'static str = "com.stoatworks.voicelevel";
    const CLAP_DESCRIPTION: Option<&'static str> = Some(
        "Attenuation-only automatic mic leveler: a virtual fader that only turns a hot mic down",
    );
    const CLAP_MANUAL_URL: Option<&'static str> = Some(Self::URL);
    const CLAP_SUPPORT_URL: Option<&'static str> = Some(Self::URL);
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Compressor,
        ClapFeature::Mixing,
        ClapFeature::Mono,
        ClapFeature::Stereo,
    ];
}

impl Vst3Plugin for VoiceLevel {
    const VST3_CLASS_ID: [u8; 16] = *b"StoatVoiceLevel1";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Dynamics];
}

nih_export_clap!(VoiceLevel);
nih_export_vst3!(VoiceLevel);
