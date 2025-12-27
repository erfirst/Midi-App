use eframe::egui;
use eframe::egui::{containers::*, *};
use egui::style::Margin;
use egui::{Color32, Painter, Pos2, Rect};
use midly::{MidiMessage, Smf, Track, TrackEventKind};
use crate::playback;
use std::collections::HashMap;

/// Persisted app state (serde) — runtime-only fields are skipped.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(default)]

pub struct MidiApp {
    // persisted/state fields:
    label: String,

    // runtime-only fields (do not serialize):
    #[serde(skip)]
    value: f32,

    #[serde(skip)]
    shapes: Vec<epaint::Shape>,

    #[serde(skip)]
    midi_data: Option<Smf<'static>>,

    #[serde(skip)]
    midi_path: Option<String>,

    #[serde(skip)]
    status: String,

    #[serde(skip)]
    game_running: bool,

    #[serde(skip)]
    playback: Option<playback::Player>,

    #[serde(skip)]
    playback_running: bool,

    #[serde(skip)]
    piano_roll: PianoRoll,

    #[serde(skip)]
    tempo: u32,
}

impl Default for MidiApp {
    fn default() -> Self {
        Self {
            // persisted field defaults
            label: "Hello World!".to_owned(),

            // runtime fields
            value: 2.7,
            game_running: false,

            playback: None,
            playback_running: false,

            shapes: Vec::with_capacity(100),
            piano_roll: PianoRoll {
                tempo: 120,
                notes: Vec::new(),
                length: 64,
                ticks_per_quarter: 480,
            },
            tempo: 120,

            // midi/runtime defaults
            midi_data: None,
            midi_path: None,
            status: "No file loaded".to_owned(),
        }
    }
}

impl MidiApp {
    /// Called once before the first frame.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // restore persisted state if available, otherwise default
        if let Some(storage) = cc.storage {
            return eframe::get_value(storage, eframe::APP_KEY).unwrap_or_default();
        }
        Default::default()
    }

    pub fn parse_midi_to_pianoroll(&mut self) {
        if let Some(smf) = &self.midi_data {
            let mut notes = Vec::new();
            let mut active_notes: HashMap<u8, (u32, u8)> = HashMap::new(); // pitch -> (start_time, velocity)
            self.tempo = 120; // default tempo

            // capture ticks-per-quarter from SMF header if available
            match &smf.header.timing {
                midly::Timing::Metrical(ticks) => {
                    self.piano_roll.ticks_per_quarter = ticks.as_int() as usize;
                }
                _ => {
                    // keep default
                }
            }

            for track in &smf.tracks {
                let mut time: u32 = 0;

                for event in track {
                    time += event.delta.as_int();

                    match &event.kind {
                        TrackEventKind::Meta(midly::MetaMessage::Tempo(microsecs)) => {
                            self.tempo = 60_000_000 / microsecs.as_int(); // convert µs per beat to BPM
                        }

                        TrackEventKind::Midi { message, .. } => match message {
                            MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                                // Start of a note
                                active_notes.insert(key.as_int(), (time, vel.as_int()));
                            }

                            MidiMessage::NoteOff { key, vel }
                            | MidiMessage::NoteOn { key, vel }
                                if vel.as_int() == 0 =>
                            {
                                // End of a note
                                if let Some((start_time, velocity)) =
                                    active_notes.remove(&key.as_int())
                                {
                                    notes.push(Note {
                                        pitch: key.as_int(), // store absolute MIDI key
                                        velocity,
                                        start_time,
                                        end_time: time,
                                    });
                                }
                            }

                            _ => {}
                        },

                        _ => {}
                    }
                }
            }

            // Update your piano roll
            self.piano_roll.tempo = self.tempo;
            self.piano_roll.notes = notes;
            self.piano_roll.length = self
                .piano_roll
                .notes
                .iter()
                .map(|n| n.end_time as usize)
                .max()
                .unwrap_or(64)
                + 1;
        }
    }
}
#[derive(Clone, Debug)]
pub struct Note {
    pub pitch: u8,
    pub velocity: u8,
    pub start_time: u32,
    pub end_time: u32,
}

struct PianoRoll {
    tempo: u32,
    notes: Vec<Note>,
    length: usize, // total columns
    ticks_per_quarter: usize,
}
impl PianoRoll {
    pub fn export_to_midi(&self, file_path: String) -> std::io::Result<()> {
        use midly::{
            num::u7, Format, Header, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind,
        };

        let tpp = self.ticks_per_quarter as u16;
        let header = Header {
            format: Format::SingleTrack,
            timing: Timing::Metrical(midly::num::u15::from(tpp)),
        };

        let mut track = Track::new();

        // build all note on/off events first
        let mut events: Vec<(u32, MidiMessage)> = Vec::new();
        for note in &self.notes {
            let pitch = note.pitch.min(127);

            events.push((
                note.start_time,
                MidiMessage::NoteOn {
                    key: u7::from(pitch),
                    vel: u7::from(note.velocity),
                },
            ));
            events.push((
                note.end_time,
                MidiMessage::NoteOff {
                    key: u7::from(pitch),
                    vel: u7::from(0),
                },
            ));
        }

        // sort events chronologically
        events.sort_by_key(|(time, _)| *time);

        // emit events with correct deltas
        let mut last_time = 0u32;
        for (time, message) in events {
            let delta = time.saturating_sub(last_time);
            track.push(TrackEvent {
                delta: delta.into(),
                kind: TrackEventKind::Midi {
                    channel: 0.into(),
                    message,
                },
            });
            last_time = time;
        }

        track.push(TrackEvent {
            delta: 0.into(),
            kind: TrackEventKind::Meta(midly::MetaMessage::EndOfTrack),
        });

        let smf = Smf {
            header,
            tracks: vec![track],
        };

        let bytes = {
            use std::io::Cursor;
            let mut buf = Vec::new();
            smf.write_std(&mut Cursor::new(&mut buf))?;
            buf
        };
        std::fs::write(file_path, &bytes)?;
        Ok(())
    }

    pub fn export_to_vec(&self) -> std::io::Result<Vec<u8>> {
        use std::io::Cursor;
        // Build the SMF and write it to a Vec<u8>
        use midly::{num::u7, Format, Header, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};

        let tpp = self.ticks_per_quarter as u16;
        let header = Header {
            format: Format::SingleTrack,
            timing: Timing::Metrical(midly::num::u15::from(tpp)),
        };

        let mut track = Track::new();

        let mut events: Vec<(u32, MidiMessage)> = Vec::new();
        for note in &self.notes {
            let pitch = note.pitch.min(127);
            events.push((
                note.start_time,
                MidiMessage::NoteOn {
                    key: u7::from(pitch),
                    vel: u7::from(note.velocity),
                },
            ));
            events.push((
                note.end_time,
                MidiMessage::NoteOff {
                    key: u7::from(pitch),
                    vel: u7::from(0),
                },
            ));
        }

        events.sort_by_key(|(time, _)| *time);

        let mut last_time = 0u32;
        for (time, message) in events {
            let delta = time.saturating_sub(last_time);
            track.push(TrackEvent {
                delta: delta.into(),
                kind: TrackEventKind::Midi {
                    channel: 0.into(),
                    message,
                },
            });
            last_time = time;
        }

        track.push(TrackEvent {
            delta: 0.into(),
            kind: TrackEventKind::Meta(midly::MetaMessage::EndOfTrack),
        });

        let smf = Smf {
            header,
            tracks: vec![track],
        };

        let mut buf = Vec::new();
        smf.write_std(&mut Cursor::new(&mut buf))?;
        Ok(buf)
    }
}

fn draw_piano_roll(ui: &mut egui::Ui, roll: &mut PianoRoll) {
    let cell_height = 20.0;

    // horizontal scaling
    let time_scale = 1.0 / 40.0;
    let base_width = 40.0;
    let complexity_scale = (100.0 / (roll.notes.len() as f32 + 1.0)).clamp(0.4, 2.0);
    let tempo_scale = (120.0 / (roll.tempo as f32)).clamp(0.5, 2.0);
    let cell_width = base_width * complexity_scale * tempo_scale * time_scale;

    let ticks_per_grid = 20usize;

    let total_ticks = roll.length as f32;
    let total_width = total_ticks * cell_width;
    let total_height = 12.0 * cell_height;

    egui::ScrollArea::horizontal()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let frame = egui::Frame::canvas(ui.style());
            frame.show(ui, |ui| {
                let (response, painter) = ui.allocate_painter(
                    egui::vec2(total_width, total_height),
                    egui::Sense::click_and_drag(),
                );

                let origin = response.rect.min;

                let clip = ui.clip_rect();
                let local_clip_min_x = (clip.min.x - origin.x).max(0.0).min(total_width);
                let local_clip_max_x = (clip.max.x - origin.x).max(0.0).min(total_width);

                let visible_start_col = (local_clip_min_x / cell_width).floor().max(0.0) as usize;
                let visible_end_col = (local_clip_max_x / cell_width)
                    .ceil()
                    .min(roll.length as f32) as usize;

                // draw alternating row backgrounds
                for row in 0..12 {
                    let bg_color = if row % 2 == 0 {
                        ui.visuals().faint_bg_color
                    } else {
                        ui.visuals().extreme_bg_color
                    };
                    let r = egui::Rect::from_min_size(
                        origin + egui::vec2(0.0, row as f32 * cell_height),
                        egui::vec2(total_width, cell_height),
                    );
                    painter.rect_filled(r, 0.0, bg_color);
                }

                // grid lines
                let full_y1 = origin.y;
                let full_y2 = origin.y + total_height;
                let stroke =
                    egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color);

                let start_t = (visible_start_col / ticks_per_grid) * ticks_per_grid;
                let end_t =
                    ((visible_end_col + ticks_per_grid - 1) / ticks_per_grid) * ticks_per_grid;

                for t in (start_t..=end_t).step_by(ticks_per_grid) {
                    if t > roll.length {
                        break;
                    }
                    let x_canvas = t as f32 * cell_width;
                    if x_canvas + cell_width < local_clip_min_x - 1.0
                        || x_canvas > local_clip_max_x + 1.0
                    {
                        continue;
                    }
                    let x_screen = origin.x + x_canvas;
                    painter.line_segment(
                        [egui::pos2(x_screen, full_y1), egui::pos2(x_screen, full_y2)],
                        stroke,
                    );
                }

                // draw notes
                for note in &roll.notes {
                    if note.end_time < visible_start_col as u32
                        || note.start_time > visible_end_col as u32
                    {
                        continue;
                    }

                    let start_x_canvas = note.start_time as f32 * cell_width;
                    let end_x_canvas = note.end_time as f32 * cell_width;
                    let pitch_mod = note.pitch % 12;
                    let note_y_canvas = pitch_mod as f32 * cell_height;

                    let rect = egui::Rect::from_min_max(
                        origin + egui::vec2(start_x_canvas, note_y_canvas),
                        origin + egui::vec2(end_x_canvas, note_y_canvas + cell_height),
                    );

                    let color = match pitch_mod {
                        0 => egui::Color32::from_rgb(139, 0, 0),
                        1 => egui::Color32::from_rgb(220, 20, 60),
                        2 => egui::Color32::from_rgb(255, 99, 71),
                        3 => egui::Color32::from_rgb(255, 255, 224),
                        4 => egui::Color32::from_rgb(255, 255, 0),
                        5 => egui::Color32::from_rgb(144, 238, 144),
                        6 => egui::Color32::from_rgb(0, 128, 0),
                        7 => egui::Color32::from_rgb(0, 100, 0),
                        8 => egui::Color32::from_rgb(25, 25, 112),
                        9 => egui::Color32::from_rgb(0, 0, 139),
                        10 => egui::Color32::from_rgb(0, 0, 255),
                        11 => egui::Color32::from_rgb(173, 216, 230),
                        _ => egui::Color32::GOLD,
                    };

                    painter.rect_filled(rect, 2.0, color);
                    painter.rect_stroke(
                        rect,
                        2.0,
                        egui::Stroke::new(1.0, egui::Color32::from_black_alpha(100)),
                    );
                }

                if response.clicked() {
                    if let Some(pointer_pos) = response.interact_pointer_pos() {
                        let local = pointer_pos - origin;

                        // Check if click is on an existing note (within its bounds)
                        let mut clicked_note_idx = None;
                        for (idx, note) in roll.notes.iter().enumerate() {
                            let note_x_min = note.start_time as f32 * cell_width;
                            let note_x_max = note.end_time as f32 * cell_width;
                            let pitch_mod = note.pitch % 12;
                            let note_y_min = pitch_mod as f32 * cell_height;
                            let note_y_max = note_y_min + cell_height;

                            if local.x >= note_x_min && local.x < note_x_max
                                && local.y >= note_y_min && local.y < note_y_max
                            {
                                clicked_note_idx = Some(idx);
                                break;
                            }
                        }

                        if let Some(idx) = clicked_note_idx {
                            // Remove the clicked note
                            roll.notes.remove(idx);
                        } else {
                            // Add a new note at the clicked position
                            let visual_pitch = (local.y / cell_height).floor().clamp(0.0, 11.0) as u8;
                            let pitch = 60u8.saturating_add(visual_pitch); // place in middle C octave
                            let start_time = (local.x / cell_width).floor().max(0.0) as u32;
                            let note_length = 20; // ticks
                            let end_time = start_time + note_length;

                            roll.notes.push(Note {
                                pitch,
                                velocity: 100,
                                start_time,
                                end_time,
                            });
                        }
                    }
                }
            });
        });
}

impl eframe::App for MidiApp {
    /// Called by the frame work to save state before shutdown.
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, self);
    }

    /// Called each time the UI needs repainting, which may be many times per second.
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Put your widgets into a `SidePanel`, `TopPanel`, `CentralPanel`, `Window` or `Area`.
        // For inspiration and more examples, go to https://emilk.github.io/egui

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                egui::widgets::global_dark_light_mode_buttons(ui);
            });
        });

        // Web drag-and-drop MIDI loader
        #[cfg(target_arch = "wasm32")]
        {
            let dropped_files = ctx.input(|i| i.raw.dropped_files.clone());
            for file in dropped_files {
                if let Some(bytes) = file.bytes {
                    // Leak the bytes to get 'static lifetime
                    let leaked_bytes: &'static [u8] = Box::leak(bytes.to_vec().into_boxed_slice());

                    match midly::Smf::parse(leaked_bytes) {
                        Ok(smf) => {
                            self.midi_data = Some(smf);
                            self.parse_midi_to_pianoroll();
                            self.status = format!("Loaded dropped MIDI file",);
                        }
                        Err(e) => {
                            self.status = format!("Failed to parse dropped file: {e}");
                        }
                    }
                }
            }
        }
        if self.game_running {
            // ===== TOP EXIT BAR =====
            egui::TopBottomPanel::top("exit_panel").show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    if ui.button("Exit Game").clicked() {
                        self.playback = None;
                        self.playback_running = false;
                        self.game_running = false;
                    }
                });
            });

            // ===== GAMEPLAY UI =====
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label(format!(
                    "Tempo: {} BPM | Notes: {}",
                    self.tempo,
                    self.piano_roll.notes.len()
                ));

                ui.horizontal(|ui| {
                    if self.playback_running {
                        if ui.button("Stop").clicked() {
                            if let Some(p) = &mut self.playback {
                                p.stop();
                            }
                            self.playback = None;
                            self.playback_running = false;
                            self.status = "Playback stopped".into();
                        }
                    } else {
                        if ui.button("Play").clicked() {
                            let mut player = playback::Player::new();
                            player.start(
                                self.piano_roll.notes.clone(),
                                self.piano_roll.ticks_per_quarter as u32,
                                self.tempo,
                            );
                            self.playback = Some(player);
                            self.playback_running = true;
                            self.status = "Playback started".into();
                        }
                    }
                    if ui.button("Save MIDI").clicked() {
                        let save_path = if let Some(path) = &self.midi_path {
                            let mut path_buf = std::path::PathBuf::from(path);
                            path_buf.set_file_name("exported.mid");
                            path_buf
                        } else {
                            let exe_dir = std::env::current_exe()
                                .ok()
                                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                                .unwrap_or_else(|| std::env::current_dir().unwrap());
                            exe_dir.join("exported.mid")
                        };

                        match self
                            .piano_roll
                            .export_to_midi(save_path.display().to_string())
                        {
                            Ok(_) => self.status = format!("MIDI exported to {}", save_path.display()),
                            Err(e) => self.status = format!("Failed to export MIDI: {}", e),
                        }
                    }
                });

                egui::ScrollArea::horizontal().show(ui, |ui| {
                    draw_piano_roll(ui, &mut self.piano_roll);
                });
            });
        } else {
            // ===== MENU =====
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    if ui.button("Playback").clicked() {
                        if self.midi_data.is_some() {
                            self.game_running = true;
                            self.status = "Playback started!".into();
                        } else {
                            self.status = "Load a MIDI file first!".into();
                        }
                    }

                    ui.add_space(10.0);
                    #[cfg(not(target_arch = "wasm32"))]
                    if ui.button("Load MIDI").clicked() {
                        {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("MIDI", &["mid", "midi"])
                                .pick_file()
                            {
                                if let Ok(bytes) = std::fs::read(&path) {
                                    let leaked_bytes: &'static [u8] =
                                        Box::leak(bytes.into_boxed_slice());

                                    match midly::Smf::parse(leaked_bytes) {
                                        Ok(smf) => {
                                            self.midi_data = Some(smf);
                                            self.midi_path = Some(path.display().to_string());
                                            self.parse_midi_to_pianoroll();
                                            self.status = "MIDI loaded successfully!".into();
                                        }
                                        Err(e) => {
                                            self.status = format!("Failed to parse MIDI: {e}")
                                        }
                                    }
                                } else {
                                    self.status = "Failed to read file".into();
                                }
                            }
                        }

                        #[cfg(target_arch = "wasm32")]
                        {
                            self.status = "Drag & drop a .mid file to load it.".into();
                        }
                    }

                    ui.add_space(10.0);
                    if ui.button("Save MIDI").clicked() {
                        #[cfg(not(target_arch = "wasm32"))]
                        {
                            let save_path = if let Some(path) = &self.midi_path {
                                let mut path_buf = std::path::PathBuf::from(path);
                                path_buf.set_file_name("exported.mid");
                                path_buf
                            } else {
                                let exe_dir = std::env::current_exe()
                                    .ok()
                                    .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                                    .unwrap_or_else(|| std::env::current_dir().unwrap());
                                exe_dir.join("exported.mid")
                            };

                            match self
                                .piano_roll
                                .export_to_midi(save_path.display().to_string())
                            {
                                Ok(_) => {
                                    self.status =
                                        format!("MIDI exported to {}", save_path.display())
                                }
                                Err(e) => self.status = format!("Failed to export MIDI: {}", e),
                            }
                        }
                        #[cfg(target_arch = "wasm32")] {
                            match self.piano_roll.export_to_vec() {
                                Ok(bytes) => {
                                    download_blob(bytes, "exported.mid");
                                    self.status = "MIDI exported (download started)".into();
                                }
                                Err(e) => self.status = format!("Failed to export MIDI: {}", e),
                            }
                        }
                    }
                });
            });
        }
        // show status in a bottom panel
        egui::TopBottomPanel::bottom("status_panel").show(ctx, |bottom_ui| {
            bottom_ui.set_min_height(28.0);
            bottom_ui.horizontal(|bottom_ui| {
                bottom_ui.add_space(8.0);
                bottom_ui.label(format!("Status: {}", self.status));
            });
        });
    }
}

// ===== Web helper: download bytes as a .mid file in the browser =====
#[cfg(target_arch = "wasm32")]
fn download_blob(bytes: Vec<u8>, filename: &str) {
    use wasm_bindgen::JsCast;
    use js_sys::Array;

    let u8a = js_sys::Uint8Array::from(bytes.as_slice());
    let parts = Array::new();
    parts.push(&u8a.into());
    let blob = web_sys::Blob::new_with_u8_array_sequence(&parts).unwrap();
    let url = web_sys::Url::create_object_url_with_blob(&blob).unwrap();
    let window = web_sys::window().unwrap();
    let document = window.document().unwrap();
    let a = document
        .create_element("a")
        .unwrap()
        .dyn_into::<web_sys::HtmlAnchorElement>()
        .unwrap();
    a.set_href(&url);
    a.set_download(filename);
    a.style().set_property("display", "none").ok();
    document.body().unwrap().append_child(&a).ok();
    a.click();
    document.body().unwrap().remove_child(&a).ok();
    web_sys::Url::revoke_object_url(&url).ok();
}
