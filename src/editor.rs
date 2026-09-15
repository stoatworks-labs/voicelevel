//! The editor: two level meters, the virtual fader, the controls, and the Learn button.

use nih_plug::prelude::*;
use nih_plug_egui::{
    create_egui_editor,
    egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2},
    widgets,
};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{Meters, ModeParam, VoiceLevelParams};

pub const WIDTH: u32 = 640;
pub const HEIGHT: u32 = 290;

/// Learn stops itself once it has heard this much speech (pauses do not count) ...
const LEARN_SECONDS: f32 = 5.0;
/// ... or after this long on the clock, whichever comes first.
const LEARN_TIMEOUT: Duration = Duration::from_secs(30);
/// The audio thread needs a few blocks to notice the mode change and reset its counters, so
/// don't trust the published learn progress before this has elapsed.
const LEARN_SETTLE: Duration = Duration::from_millis(250);

const METER_MIN_DB: f32 = -60.0;
const METER_MAX_DB: f32 = 0.0;
const BAR_HEIGHT: f32 = 190.0;
const BAR_WIDTH: f32 = 26.0;

const GREEN: Color32 = Color32::from_rgb(76, 175, 80);
const AMBER: Color32 = Color32::from_rgb(255, 167, 38);
const RED: Color32 = Color32::from_rgb(239, 83, 80);
const BLUE: Color32 = Color32::from_rgb(66, 165, 245);
const TARGET_LINE: Color32 = Color32::from_rgb(240, 240, 240);

#[derive(Default)]
pub struct EditorState {
    /// When this editor started the current Learn; `None` when Learn was started elsewhere (a
    /// host automation lane, another editor instance), in which case we never stop it ourselves.
    learn_started: Option<Instant>,
}

pub fn create(params: Arc<VoiceLevelParams>, meters: Arc<Meters>) -> Option<Box<dyn Editor>> {
    let egui_state = params.editor_state.clone();
    create_egui_editor(
        egui_state,
        EditorState::default(),
        |ctx, _state| {
            ctx.set_visuals(egui::Visuals::dark());
        },
        move |ctx, setter, state| {
            egui::CentralPanel::default().show(ctx, |ui| {
                draw(ui, setter, state, &params, &meters);
            });
        },
    )
}

fn draw(
    ui: &mut egui::Ui,
    setter: &ParamSetter,
    state: &mut EditorState,
    params: &VoiceLevelParams,
    meters: &Meters,
) {
    let mode = params.mode.value();
    let target = params.target.value();
    let range = params.range.value();
    let fader_db = params.fader_db.load(Ordering::Relaxed);
    let in_db = meters.in_db.load(Ordering::Relaxed);
    let speech_db = meters.speech_db.load(Ordering::Relaxed);
    let out_db = meters.out_db.load(Ordering::Relaxed);
    let learn_seconds = meters.learn_seconds.load(Ordering::Relaxed);
    let learn_level = meters.learn_level_db.load(Ordering::Relaxed);
    let learned_level = meters.learned_level_db.load(Ordering::Relaxed);

    // Header: title on the left, mode buttons on the right.
    ui.horizontal(|ui| {
        ui.heading("VoiceLevel");
        ui.label(egui::RichText::new("attenuation-only mic leveler").weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let learning = mode == ModeParam::Learn;
            let learn_text = if learning {
                format!("Learning…  {learn_seconds:.1} s")
            } else {
                String::from("Learn")
            };
            if ui
                .selectable_label(learning, learn_text)
                .on_hover_text(
                    "Listen to the talker for a few seconds, then park the fader where it puts \
                     them on target",
                )
                .clicked()
            {
                if learning {
                    set_mode(setter, params, ModeParam::Fixed);
                    state.learn_started = None;
                } else {
                    set_mode(setter, params, ModeParam::Learn);
                    state.learn_started = Some(Instant::now());
                }
            }
            for (m, label, hover) in [
                (ModeParam::Fixed, "Fixed", "The fader stays where it is"),
                (ModeParam::Auto, "Auto", "The fader follows the talker"),
            ] {
                if ui
                    .selectable_label(mode == m, label)
                    .on_hover_text(hover)
                    .clicked()
                    && mode != m
                {
                    set_mode(setter, params, m);
                    state.learn_started = None;
                }
            }
        });
    });

    // A Learn this editor started ends itself.
    if mode == ModeParam::Learn {
        if let Some(started) = state.learn_started {
            let elapsed = started.elapsed();
            let heard_enough = elapsed >= LEARN_SETTLE && learn_seconds >= LEARN_SECONDS;
            if heard_enough || elapsed >= LEARN_TIMEOUT {
                set_mode(setter, params, ModeParam::Fixed);
                state.learn_started = None;
            }
        }
    } else {
        state.learn_started = None;
    }

    ui.separator();

    ui.horizontal_top(|ui| {
        ui.add_space(4.0);
        meter(
            ui,
            "IN",
            in_db,
            target,
            Some(speech_db),
            "Input level (RMS). White line: target. Yellow tick: the talker's level with pauses \
             left out — what the fader is steered by.",
        );
        ui.add_space(6.0);
        meter(
            ui,
            "OUT",
            out_db,
            target,
            None,
            "Output level (RMS). White line: target.",
        );
        ui.add_space(14.0);

        let editable = mode == ModeParam::Fixed;
        if let Some(new_db) = fader(ui, fader_db, range, editable) {
            params.fader_db.store(new_db, Ordering::Relaxed);
        }
        ui.add_space(14.0);

        ui.vertical(|ui| {
            egui::Grid::new("controls")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    for (label, param, hover) in [
                        (
                            "Target",
                            &params.target,
                            "Output level the fader aims for (RMS)",
                        ),
                        (
                            "Speed",
                            &params.speed,
                            "How fast the fader is pulled down when the mic is too loud",
                        ),
                        (
                            "Recovery",
                            &params.recovery,
                            "How fast the fader eases back up towards 0 dB when the talker \
                             is under target",
                        ),
                        (
                            "Range",
                            &params.range,
                            "The most the fader will ever take off",
                        ),
                        (
                            "Hold below",
                            &params.hold_below,
                            "When the mic is quieter than this the fader freezes: a pause is \
                             not a reason to bring a hot mic back up",
                        ),
                        (
                            "Averaging",
                            &params.averaging,
                            "How much speech the level is judged over",
                        ),
                    ] {
                        ui.label(label).on_hover_text(hover);
                        ui.add(widgets::ParamSlider::for_param(param, setter).with_width(190.0));
                        ui.end_row();
                    }
                });

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(4.0);

            let status = match mode {
                ModeParam::Auto => format!(
                    "Auto · fader {} · talker at {}",
                    fmt_att(fader_db),
                    fmt_db(speech_db)
                ),
                ModeParam::Fixed => {
                    let mut s = format!("Fixed · fader {}", fmt_att(fader_db));
                    if !learned_level.is_nan() {
                        s.push_str(&format!(
                            " · learned from a talker at {}",
                            fmt_db(learned_level)
                        ));
                    }
                    s
                }
                ModeParam::Learn => {
                    if learn_level.is_nan() {
                        String::from("Learning · waiting for the talker…")
                    } else {
                        let would = (learn_level - target).clamp(0.0, range.max(0.0));
                        format!(
                            "Learning · {learn_seconds:.1} s of speech at {} → fader {}",
                            fmt_db(learn_level),
                            fmt_att(would)
                        )
                    }
                }
            };
            ui.label(egui::RichText::new(status).monospace().size(11.5));
        });
    });
}

fn set_mode(setter: &ParamSetter, params: &VoiceLevelParams, mode: ModeParam) {
    setter.begin_set_parameter(&params.mode);
    setter.set_parameter(&params.mode, mode);
    setter.end_set_parameter(&params.mode);
}

fn fmt_db(db: f32) -> String {
    if db.is_nan() || db <= METER_MIN_DB - 20.0 {
        String::from("-∞ dBFS")
    } else {
        format!("{db:.1} dBFS")
    }
}

/// Attenuation shown the way a fader scale reads it: `0.0 dB`, `-6.3 dB`. `-db` would print
/// `-0.0` at rest, so zero is special-cased.
fn fmt_att(db: f32) -> String {
    if db.abs() < 0.05 {
        String::from("0.0 dB")
    } else {
        format!("{:.1} dB", -db)
    }
}

fn meter_y(rect: &Rect, db: f32) -> f32 {
    let t = ((db - METER_MIN_DB) / (METER_MAX_DB - METER_MIN_DB)).clamp(0.0, 1.0);
    rect.bottom() - t * rect.height()
}

/// A vertical RMS meter, green up to the target and amber above it, with the target drawn as a
/// line across the bar and an optional tick for the gated speech level.
fn meter(
    ui: &mut egui::Ui,
    label: &str,
    level_db: f32,
    target_db: f32,
    speech_db: Option<f32>,
    hover: &str,
) {
    ui.vertical(|ui| {
        ui.set_width(BAR_WIDTH + 16.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new(label).small().strong());
            let (rect, response) =
                ui.allocate_exact_size(Vec2::new(BAR_WIDTH, BAR_HEIGHT), Sense::hover());
            response.on_hover_text(hover);
            let p = ui.painter();
            p.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);

            let level = level_db.clamp(METER_MIN_DB, METER_MAX_DB);
            let level_y = meter_y(&rect, level);
            let target_y = meter_y(&rect, target_db);
            if level > METER_MIN_DB {
                let green_top = level_y.max(target_y);
                p.rect_filled(
                    Rect::from_min_max(Pos2::new(rect.left(), green_top), rect.right_bottom()),
                    0.0,
                    GREEN,
                );
                if level_y < target_y {
                    let over = if level - target_db > 6.0 { RED } else { AMBER };
                    p.rect_filled(
                        Rect::from_min_max(
                            Pos2::new(rect.left(), level_y),
                            Pos2::new(rect.right(), target_y),
                        ),
                        0.0,
                        over,
                    );
                }
            }
            p.line_segment(
                [
                    Pos2::new(rect.left() - 3.0, target_y),
                    Pos2::new(rect.right() + 3.0, target_y),
                ],
                Stroke::new(1.5_f32, TARGET_LINE),
            );
            if let Some(speech) = speech_db {
                if speech > METER_MIN_DB {
                    let y = meter_y(&rect, speech);
                    p.line_segment(
                        [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                        Stroke::new(2.0_f32, Color32::YELLOW),
                    );
                }
            }
            p.rect_stroke(
                rect,
                3.0,
                Stroke::new(1.0_f32, ui.visuals().window_stroke.color),
                StrokeKind::Outside,
            );

            ui.label(
                egui::RichText::new(fmt_db(level_db).replace(" dBFS", ""))
                    .monospace()
                    .small(),
            );
        });
    });
}

/// The virtual fader: 0 dB at the top, `range_db` of attenuation at the bottom. The blue fill is
/// what is being taken off. Draggable when `editable`; returns the new attenuation if the user
/// moved it (double-click returns it to 0 dB).
fn fader(ui: &mut egui::Ui, fader_db: f32, range_db: f32, editable: bool) -> Option<f32> {
    let mut changed = None;
    let range = range_db.max(0.0);
    ui.vertical(|ui| {
        ui.set_width(BAR_WIDTH + 44.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("FADER").small().strong());
            let sense = if editable {
                Sense::click_and_drag()
            } else {
                Sense::hover()
            };
            let (rect, response) =
                ui.allocate_exact_size(Vec2::new(BAR_WIDTH + 44.0, BAR_HEIGHT), sense);
            let track = Rect::from_center_size(rect.center(), Vec2::new(BAR_WIDTH, rect.height()));

            let att_y = |db: f32| {
                let t = if range > 0.0 {
                    (db / range).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                track.top() + t * track.height()
            };

            let p = ui.painter();
            p.rect_filled(track, 3.0, ui.visuals().extreme_bg_color);
            let cap_y = att_y(fader_db);
            if fader_db > 0.0 {
                p.rect_filled(
                    Rect::from_min_max(track.left_top(), Pos2::new(track.right(), cap_y)),
                    0.0,
                    BLUE,
                );
            }
            // Scale ticks every 6 dB, labelled every 12.
            let font = FontId::proportional(9.0);
            let mut db = 0.0f32;
            while db <= range + 0.01 {
                let y = att_y(db);
                p.line_segment(
                    [Pos2::new(track.left() - 4.0, y), Pos2::new(track.left(), y)],
                    Stroke::new(1.0_f32, ui.visuals().weak_text_color()),
                );
                if (db / 12.0).fract().abs() < 1e-3 {
                    p.text(
                        Pos2::new(track.left() - 6.0, y),
                        Align2::RIGHT_CENTER,
                        if db == 0.0 {
                            String::from("0")
                        } else {
                            format!("{:.0}", -db)
                        },
                        font.clone(),
                        ui.visuals().weak_text_color(),
                    );
                }
                db += 6.0;
            }
            // The fader cap.
            let cap = Rect::from_center_size(
                Pos2::new(track.center().x, cap_y),
                Vec2::new(BAR_WIDTH + 8.0, 7.0),
            );
            let cap_color = if editable {
                ui.visuals().strong_text_color()
            } else {
                ui.visuals().weak_text_color()
            };
            p.rect_filled(cap, 2.0, cap_color);
            p.rect_stroke(
                track,
                3.0,
                Stroke::new(1.0_f32, ui.visuals().window_stroke.color),
                StrokeKind::Outside,
            );

            if editable {
                if response.double_clicked() {
                    changed = Some(0.0);
                } else if response.dragged() || response.clicked() {
                    if let Some(pos) = response.interact_pointer_pos() {
                        let t = ((pos.y - track.top()) / track.height()).clamp(0.0, 1.0);
                        changed = Some(t * range);
                    }
                }
                response.on_hover_text("Drag to set the attenuation; double-click for 0 dB");
            } else {
                response.on_hover_text("Switch to Fixed to move the fader by hand");
            }

            ui.label(egui::RichText::new(fmt_att(fader_db)).monospace().small());
        });
    });
    changed
}
