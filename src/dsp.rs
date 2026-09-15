//! The leveler itself: a virtual fader that only ever moves *down* from unity.
//!
//! Everything in here is host-agnostic and allocation-free so it can be driven from unit tests
//! and from the plugin wrapper alike. The signal path is a single multiply per sample; the
//! detector runs in parallel on a high-passed copy of the input, so the audio path has no
//! look-ahead and therefore no latency.
//!
//! Two detectors run on the sidechain:
//!
//! * a fast RMS (10 ms) that answers "is anyone talking right now?" — it drives the hold gate
//!   and the meters;
//! * a slow RMS (`averaging_ms`) that only integrates while the gate is open, so it measures the
//!   level *of the speech* rather than the average of speech and pauses. That is what the fader
//!   is steered by.

/// The most channels the leveler links across. Anything beyond this passes through the same
/// gain but is not measured.
pub const MAX_CHANNELS: usize = 2;

/// Sidechain high-pass corner. Plosives, handling noise and stage rumble sit below this and must
/// not pull the fader down; speech energy sits above it.
pub const DETECTOR_HPF_HZ: f32 = 100.0;

/// Time constant of the gate/meter detector.
pub const FAST_DETECTOR_MS: f32 = 10.0;

/// Levels below this are treated as silence in every dB conversion.
pub const FLOOR_DB: f32 = -200.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The fader tracks the input on its own.
    Auto,
    /// The fader stays where it is (where Learn or the operator put it).
    Fixed,
    /// The fader stays put while the input level is measured; leaving this mode commits the
    /// measurement as a fixed attenuation.
    Learn,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Output level the fader aims for, dBFS RMS.
    pub target_db: f32,
    /// How fast the fader is pulled down, dB per second.
    pub speed_db_per_s: f32,
    /// How fast the fader eases back up towards unity, dB per second.
    pub recovery_db_per_s: f32,
    /// The most the fader will ever attenuate, dB.
    pub range_db: f32,
    /// Below this input level (dBFS RMS, fast detector) the fader freezes: a pause is not a
    /// reason to bring a hot mic back up.
    pub hold_below_db: f32,
    /// Speech-level detector time constant, milliseconds.
    pub averaging_ms: f32,
    pub mode: Mode,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            target_db: -20.0,
            speed_db_per_s: 10.0,
            recovery_db_per_s: 2.0,
            range_db: 30.0,
            hold_below_db: -50.0,
            averaging_ms: 300.0,
            mode: Mode::Auto,
        }
    }
}

/// Second-order high-pass (RBJ cookbook), transposed direct form II.
#[derive(Clone, Copy, Debug, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn highpass(sample_rate: f32, freq: f32, q: f32) -> Self {
        let w0 = 2.0 * std::f32::consts::PI * freq / sample_rate;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: ((1.0 + cos_w0) / 2.0) / a0,
            b1: (-(1.0 + cos_w0)) / a0,
            b2: ((1.0 + cos_w0) / 2.0) / a0,
            a1: (-2.0 * cos_w0) / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    10.0f32.powf(db * 0.05)
}

#[inline]
fn mean_square_to_db(ms: f32) -> f32 {
    if ms > 0.0 {
        (10.0 * ms.log10()).max(FLOOR_DB)
    } else {
        FLOOR_DB
    }
}

/// One-pole coefficient for an exponential average with time constant `ms`.
fn averaging_coeff(ms: f32, sample_rate: f32) -> f32 {
    let tau = (ms.max(0.01) / 1000.0) * sample_rate;
    1.0 - (-1.0 / tau).exp()
}

pub struct Leveler {
    sample_rate: f32,
    settings: Settings,

    hpf: [Biquad; MAX_CHANNELS],
    /// Fast mean square of the detector signal, per channel.
    ms_fast: [f32; MAX_CHANNELS],
    fast_coeff: f32,
    /// Slow, gated mean square of the loudest channel: the speech level.
    ms_slow: f32,
    slow_coeff: f32,
    /// Fast mean square after the fader, for the output meter.
    out_ms: f32,
    /// Fast detector level of the last processed sample, dBFS RMS (the loudest channel).
    level_db: f32,

    /// Current attenuation in dB, always >= 0. The fader position.
    fader_db: f32,
    /// Linear gain for `gain_db`, recomputed only when the fader moves.
    gain: f32,
    gain_db: f32,

    learn_acc: f64,
    learn_n: u64,
    learned_level_db: Option<f32>,
}

impl Leveler {
    pub fn new(sample_rate: f32) -> Self {
        let mut leveler = Self {
            sample_rate,
            settings: Settings::default(),
            hpf: [Biquad::default(); MAX_CHANNELS],
            ms_fast: [0.0; MAX_CHANNELS],
            fast_coeff: 0.0,
            ms_slow: 0.0,
            slow_coeff: 0.0,
            out_ms: 0.0,
            level_db: FLOOR_DB,
            fader_db: 0.0,
            gain: 1.0,
            gain_db: 0.0,
            learn_acc: 0.0,
            learn_n: 0,
            learned_level_db: None,
        };
        leveler.set_sample_rate(sample_rate);
        leveler
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.hpf = [Biquad::highpass(
            sample_rate,
            DETECTOR_HPF_HZ,
            std::f32::consts::FRAC_1_SQRT_2,
        ); MAX_CHANNELS];
        self.fast_coeff = averaging_coeff(FAST_DETECTOR_MS, sample_rate);
        self.slow_coeff = averaging_coeff(self.settings.averaging_ms, sample_rate);
        self.reset();
    }

    /// Clear the detector state. The fader keeps its position: a transport jump is not a reason
    /// to bring a hot mic back up either.
    pub fn reset(&mut self) {
        for f in &mut self.hpf {
            f.reset();
        }
        self.ms_fast = [0.0; MAX_CHANNELS];
        self.ms_slow = 0.0;
        self.out_ms = 0.0;
        self.level_db = FLOOR_DB;
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Apply new settings. A change of mode out of `Learn` commits the measurement.
    pub fn set_settings(&mut self, settings: Settings) {
        if settings.mode != self.settings.mode {
            if self.settings.mode == Mode::Learn {
                self.commit_learn(&settings);
            }
            if settings.mode == Mode::Learn {
                self.learn_acc = 0.0;
                self.learn_n = 0;
            }
        }
        if settings.averaging_ms != self.settings.averaging_ms {
            self.slow_coeff = averaging_coeff(settings.averaging_ms, self.sample_rate);
        }
        self.settings = settings;
    }

    /// Current attenuation in dB (>= 0).
    pub fn fader_db(&self) -> f32 {
        self.fader_db
    }

    pub fn set_fader_db(&mut self, db: f32) {
        self.fader_db = db.clamp(0.0, self.settings.range_db.max(0.0));
    }

    /// Fast detector level of the input, dBFS RMS. What a meter would show.
    pub fn level_db(&self) -> f32 {
        self.level_db
    }

    /// Gated speech level of the input, dBFS RMS. What the fader is steered by.
    pub fn speech_level_db(&self) -> f32 {
        mean_square_to_db(self.ms_slow)
    }

    /// Fast detector level after the fader, dBFS RMS.
    pub fn out_level_db(&self) -> f32 {
        mean_square_to_db(self.out_ms)
    }

    /// Seconds of above-threshold signal measured so far in `Learn` mode.
    pub fn learn_seconds(&self) -> f32 {
        self.learn_n as f32 / self.sample_rate
    }

    /// Running average of the current learn measurement, dBFS RMS.
    pub fn learn_level_db(&self) -> Option<f32> {
        (self.learn_n > 0).then(|| mean_square_to_db((self.learn_acc / self.learn_n as f64) as f32))
    }

    /// The level the last committed learn measured, dBFS RMS.
    pub fn learned_level_db(&self) -> Option<f32> {
        self.learned_level_db
    }

    fn commit_learn(&mut self, settings: &Settings) {
        if let Some(level) = self.learn_level_db() {
            self.learned_level_db = Some(level);
            self.fader_db = (level - settings.target_db).clamp(0.0, settings.range_db.max(0.0));
        }
    }

    /// Process a block in place. All channels get the same gain; the first [`MAX_CHANNELS`] are
    /// measured. Every channel slice must be the same length.
    pub fn process(&mut self, channels: &mut [&mut [f32]]) {
        let Some(len) = channels.first().map(|c| c.len()) else {
            return;
        };
        let s = self.settings;
        let pull_step = s.speed_db_per_s.max(0.0) / self.sample_rate;
        let ease_step = s.recovery_db_per_s.max(0.0) / self.sample_rate;
        let range = s.range_db.max(0.0);
        let mut fader = self.fader_db.min(range);

        for i in 0..len {
            // Fast detector: high-passed, 10 ms mean square, loudest channel wins.
            let mut ms_max = 0.0f32;
            let mut det_max = 0.0f32;
            for ((channel, hpf), ms) in channels
                .iter()
                .zip(self.hpf.iter_mut())
                .zip(self.ms_fast.iter_mut())
            {
                let d = hpf.process(channel[i]);
                *ms += (d * d - *ms) * self.fast_coeff;
                if *ms > ms_max {
                    ms_max = *ms;
                    det_max = d;
                }
            }
            let level_db = mean_square_to_db(ms_max);
            let audible = level_db >= s.hold_below_db;

            if audible {
                // Speech level: only ever integrates what is above the hold threshold.
                self.ms_slow += (ms_max - self.ms_slow) * self.slow_coeff;
                match s.mode {
                    Mode::Auto => {
                        let speech_db = mean_square_to_db(self.ms_slow);
                        let desired = (speech_db - s.target_db).clamp(0.0, range);
                        if desired > fader {
                            fader = (fader + pull_step).min(desired);
                        } else if desired < fader {
                            fader = (fader - ease_step).max(desired);
                        }
                    }
                    Mode::Fixed => {}
                    Mode::Learn => {
                        self.learn_acc += ms_max as f64;
                        self.learn_n += 1;
                    }
                }
            }

            if fader != self.gain_db {
                self.gain_db = fader;
                self.gain = db_to_gain(-fader);
            }
            let gain = self.gain;
            for channel in channels.iter_mut() {
                channel[i] *= gain;
            }

            let out = det_max * gain;
            self.out_ms += (out * out - self.out_ms) * self.fast_coeff;
            self.level_db = level_db;
        }

        self.fader_db = fader;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;
    const BLOCK: usize = 256;

    struct Rig {
        leveler: Leveler,
        phase: f32,
        seed: u32,
    }

    impl Rig {
        fn new(settings: Settings) -> Self {
            let mut leveler = Leveler::new(SR);
            leveler.set_settings(settings);
            Self {
                leveler,
                phase: 0.0,
                seed: 0x1234_5678,
            }
        }

        fn settings(&mut self, f: impl FnOnce(&mut Settings)) {
            let mut s = *self.leveler.settings();
            f(&mut s);
            self.leveler.set_settings(s);
        }

        /// Feed `seconds` of a 1 kHz sine at `level_db` dBFS RMS, returning the output RMS in dB.
        fn sine(&mut self, level_db: f32, seconds: f32) -> f32 {
            let amp = db_to_gain(level_db) * std::f32::consts::SQRT_2;
            let blocks = (seconds * SR / BLOCK as f32) as usize;
            let mut acc = 0.0f64;
            let mut n = 0u64;
            for _ in 0..blocks {
                let mut buf = [0.0f32; BLOCK];
                for x in buf.iter_mut() {
                    *x = amp * (self.phase * std::f32::consts::TAU).sin();
                    self.phase = (self.phase + 1000.0 / SR).fract();
                }
                self.leveler.process(&mut [&mut buf]);
                for x in buf {
                    acc += (x * x) as f64;
                    n += 1;
                }
            }
            mean_square_to_db((acc / n as f64) as f32)
        }

        fn silence(&mut self, seconds: f32) {
            let blocks = (seconds * SR / BLOCK as f32) as usize;
            for _ in 0..blocks {
                let mut buf = [0.0f32; BLOCK];
                self.leveler.process(&mut [&mut buf]);
            }
        }

        fn noise_sample(&mut self) -> f32 {
            // xorshift32, plenty for a test signal
            self.seed ^= self.seed << 13;
            self.seed ^= self.seed >> 17;
            self.seed ^= self.seed << 5;
            (self.seed as f32 / u32::MAX as f32) * 2.0 - 1.0
        }
    }

    #[test]
    fn hot_signal_is_pulled_down_to_target() {
        let mut rig = Rig::new(Settings::default());
        // -10 dBFS in, -20 target: the fader should settle 10 dB down and the output at target.
        let out = rig.sine(-10.0, 6.0);
        assert!(
            (rig.leveler.fader_db() - 10.0).abs() < 0.3,
            "fader {}",
            rig.leveler.fader_db()
        );
        // Averaged over the whole run including the ~1 s approach.
        assert!((out - (-20.0)).abs() < 2.0, "out {out}");
        let out = rig.sine(-10.0, 1.0);
        assert!((out - (-20.0)).abs() < 0.2, "settled out {out}");
    }

    #[test]
    fn quiet_signal_is_never_boosted() {
        let mut rig = Rig::new(Settings::default());
        let out = rig.sine(-35.0, 4.0);
        assert_eq!(rig.leveler.fader_db(), 0.0);
        assert!((out - (-35.0)).abs() < 0.05, "out {out}");
    }

    #[test]
    fn output_never_exceeds_input_sample_for_sample() {
        let mut rig = Rig::new(Settings {
            target_db: -30.0,
            speed_db_per_s: 100.0,
            recovery_db_per_s: 50.0,
            ..Settings::default()
        });
        for _ in 0..2000 {
            let mut input = [0.0f32; BLOCK];
            for x in input.iter_mut() {
                *x = rig.noise_sample();
            }
            let mut output = input;
            rig.leveler.process(&mut [&mut output]);
            for (i, o) in input.iter().zip(output.iter()) {
                assert!(o.abs() <= i.abs() + 1e-7, "boosted: in {i} out {o}");
                assert!(i.signum() == o.signum() || *o == 0.0, "polarity flipped");
            }
        }
        assert!(
            rig.leveler.fader_db() > 20.0,
            "noise at 0 dBFS should be pulled well down"
        );
    }

    #[test]
    fn fader_moves_at_the_configured_speed() {
        let mut rig = Rig::new(Settings {
            speed_db_per_s: 4.0,
            averaging_ms: 20.0,
            ..Settings::default()
        });
        // 20 dB over target with a 4 dB/s fader: after 2 s it should be close to 8 dB.
        rig.sine(0.0, 2.0);
        let fader = rig.leveler.fader_db();
        assert!(
            (fader - 8.0).abs() < 0.3,
            "fader {fader} after 2 s at 4 dB/s"
        );
        rig.sine(0.0, 2.0);
        let fader = rig.leveler.fader_db();
        assert!(
            (fader - 16.0).abs() < 0.3,
            "fader {fader} after 4 s at 4 dB/s"
        );
    }

    #[test]
    fn recovery_is_slower_and_stops_at_unity() {
        let mut rig = Rig::new(Settings {
            speed_db_per_s: 20.0,
            recovery_db_per_s: 5.0,
            averaging_ms: 20.0,
            ..Settings::default()
        });
        rig.sine(-10.0, 3.0);
        assert!((rig.leveler.fader_db() - 10.0).abs() < 0.3);
        // A signal that is audible but below target: ease back up at 5 dB/s, less the ~0.1 s
        // the detectors take to fall.
        rig.sine(-30.0, 1.0);
        let fader = rig.leveler.fader_db();
        assert!(
            (4.5..=6.5).contains(&fader),
            "fader {fader} after 1 s of recovery"
        );
        rig.sine(-30.0, 3.0);
        assert_eq!(rig.leveler.fader_db(), 0.0);
    }

    #[test]
    fn silence_holds_the_fader() {
        let mut rig = Rig::new(Settings::default());
        rig.sine(-10.0, 4.0);
        let settled = rig.leveler.fader_db();
        assert!(settled > 9.5, "{settled}");
        // The fast detector takes ~0.1 s to fall below the hold threshold; the fader may ease
        // a fraction of a dB in that tail, then it must not move at all.
        rig.silence(0.5);
        let held = rig.leveler.fader_db();
        assert!(held > settled - 0.3, "tail dip {settled} -> {held}");
        rig.silence(10.0);
        assert_eq!(rig.leveler.fader_db(), held);
        // Below the hold threshold but not digital silence: still held.
        rig.sine(-70.0, 5.0);
        assert_eq!(rig.leveler.fader_db(), held);
    }

    #[test]
    fn range_caps_the_attenuation() {
        let mut rig = Rig::new(Settings {
            range_db: 6.0,
            speed_db_per_s: 50.0,
            ..Settings::default()
        });
        rig.sine(0.0, 3.0);
        assert_eq!(rig.leveler.fader_db(), 6.0);
        // Shrinking the range pulls an existing fader position in with it, in any mode.
        rig.settings(|s| {
            s.range_db = 3.0;
            s.mode = Mode::Fixed;
        });
        rig.sine(0.0, 0.1);
        assert_eq!(rig.leveler.fader_db(), 3.0);
    }

    #[test]
    fn fixed_mode_does_not_move() {
        let mut rig = Rig::new(Settings {
            mode: Mode::Fixed,
            ..Settings::default()
        });
        rig.leveler.set_fader_db(7.5);
        let out = rig.sine(-10.0, 2.0);
        assert_eq!(rig.leveler.fader_db(), 7.5);
        assert!((out - (-17.5)).abs() < 0.05, "out {out}");
    }

    #[test]
    fn learn_measures_then_fixes_the_fader() {
        let mut rig = Rig::new(Settings::default());
        rig.settings(|s| s.mode = Mode::Learn);
        // Speech-like: loud passages with a pause; the pause must not dilute the measurement.
        rig.sine(-8.0, 1.0);
        rig.silence(1.0);
        rig.sine(-8.0, 1.0);
        let seconds = rig.leveler.learn_seconds();
        assert!((seconds - 2.0).abs() < 0.2, "gated seconds {seconds}");
        let level = rig.leveler.learn_level_db().unwrap();
        assert!((level - (-8.0)).abs() < 0.4, "learn level {level}");
        // While learning the fader stays put.
        assert_eq!(rig.leveler.fader_db(), 0.0);

        rig.settings(|s| s.mode = Mode::Fixed);
        let fader = rig.leveler.fader_db();
        assert!((fader - 12.0).abs() < 0.4, "learned fader {fader}");
        assert!((rig.leveler.learned_level_db().unwrap() - (-8.0)).abs() < 0.4);
        let out = rig.sine(-8.0, 1.0);
        assert!((out - (-20.0)).abs() < 0.4, "out {out}");
        assert_eq!(rig.leveler.fader_db(), fader);
    }

    #[test]
    fn learn_with_nothing_heard_keeps_the_fader() {
        let mut rig = Rig::new(Settings::default());
        rig.sine(-10.0, 3.0);
        rig.silence(0.5);
        let before = rig.leveler.fader_db();
        assert!(before > 9.0);
        rig.settings(|s| s.mode = Mode::Learn);
        rig.silence(2.0);
        assert_eq!(rig.leveler.learn_level_db(), None);
        rig.settings(|s| s.mode = Mode::Fixed);
        assert_eq!(rig.leveler.fader_db(), before);
        assert_eq!(rig.leveler.learned_level_db(), None);
    }

    #[test]
    fn learn_to_auto_starts_auto_from_the_learned_position() {
        let mut rig = Rig::new(Settings::default());
        rig.settings(|s| s.mode = Mode::Learn);
        rig.sine(-5.0, 2.0);
        rig.settings(|s| s.mode = Mode::Auto);
        let fader = rig.leveler.fader_db();
        assert!((fader - 15.0).abs() < 0.4, "learned fader {fader}");
        // Auto now tracks: a quieter talker lets it ease back up.
        rig.sine(-25.0, 4.0);
        assert!(rig.leveler.fader_db() < fader - 4.0);
    }

    #[test]
    fn stereo_channels_are_linked_to_the_louder_side() {
        let mut leveler = Leveler::new(SR);
        leveler.set_settings(Settings {
            speed_db_per_s: 100.0,
            averaging_ms: 20.0,
            ..Settings::default()
        });
        let amp_l = db_to_gain(-10.0) * std::f32::consts::SQRT_2;
        let amp_r = db_to_gain(-30.0) * std::f32::consts::SQRT_2;
        let mut phase = 0.0f32;
        for _ in 0..(3.0 * SR / BLOCK as f32) as usize {
            let mut l = [0.0f32; BLOCK];
            let mut r = [0.0f32; BLOCK];
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                let s = (phase * std::f32::consts::TAU).sin();
                *a = amp_l * s;
                *b = amp_r * s;
                phase = (phase + 1000.0 / SR).fract();
            }
            let (in_l, in_r) = (l, r);
            leveler.process(&mut [&mut l, &mut r]);
            // Both channels get the same gain at every sample.
            for i in 0..BLOCK {
                assert!((l[i] * in_r[i] - r[i] * in_l[i]).abs() < 1e-6, "sample {i}");
            }
        }
        // Steered by the loud left channel, not the average of the two.
        assert!(
            (leveler.fader_db() - 10.0).abs() < 0.3,
            "{}",
            leveler.fader_db()
        );
    }

    #[test]
    fn rumble_below_the_detector_hpf_is_ignored() {
        let mut leveler = Leveler::new(SR);
        leveler.set_settings(Settings {
            speed_db_per_s: 100.0,
            ..Settings::default()
        });
        // A 20 Hz hum at -3 dBFS: far above target, but the detector should barely see it.
        let amp = db_to_gain(-3.0) * std::f32::consts::SQRT_2;
        let mut phase = 0.0f32;
        for _ in 0..(3.0 * SR / BLOCK as f32) as usize {
            let mut buf = [0.0f32; BLOCK];
            for x in buf.iter_mut() {
                *x = amp * (phase * std::f32::consts::TAU).sin();
                phase = (phase + 20.0 / SR).fract();
            }
            leveler.process(&mut [&mut buf]);
        }
        // Second-order Butterworth at 100 Hz: 20 Hz is ~28 dB down, so the detector sees ~-31.
        assert!(
            leveler.level_db() < -25.0,
            "detector saw {}",
            leveler.level_db()
        );
        assert_eq!(leveler.fader_db(), 0.0);
    }

    #[test]
    fn zero_latency_impulse_passes_straight_through() {
        let mut leveler = Leveler::new(SR);
        let mut buf = [0.0f32; 64];
        buf[10] = 0.5;
        leveler.process(&mut [&mut buf]);
        assert_eq!(buf[10], 0.5);
        assert!(buf.iter().enumerate().all(|(i, x)| i == 10 || *x == 0.0));
    }

    #[test]
    fn sample_rate_change_keeps_the_fader() {
        let mut rig = Rig::new(Settings::default());
        rig.sine(-10.0, 3.0);
        let fader = rig.leveler.fader_db();
        rig.leveler.set_sample_rate(96_000.0);
        assert_eq!(rig.leveler.fader_db(), fader);
        assert_eq!(rig.leveler.level_db(), FLOOR_DB);
    }
}
