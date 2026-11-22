use std::sync::{Arc, Mutex};

use egui::{
    pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, FontId, Key, KeyboardShortcut,
    Modifiers, Pos2, Rect, Sense, Stroke, StrokeKind,
};

use crate::midi::{is_black_key, note_name, Note, Song};
use crate::playback::Player;

const KEYBOARD_W: f32 = 52.0;
const RULER_H: f32 = 22.0;
const MAX_UNDO: usize = 200;

/// A file picked asynchronously (web file picker) waiting to be loaded.
type PendingFile = Arc<Mutex<Option<(String, Vec<u8>)>>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Grid {
    Off,
    Sixteenth,
    Eighth,
    Quarter,
}

impl Grid {
    const ALL: [Grid; 4] = [Grid::Sixteenth, Grid::Eighth, Grid::Quarter, Grid::Off];

    fn label(self) -> &'static str {
        match self {
            Grid::Off => "Off",
            Grid::Sixteenth => "1/16",
            Grid::Eighth => "1/8",
            Grid::Quarter => "1/4",
        }
    }

    /// Grid size in ticks (None = no snapping).
    fn ticks(self, tpq: u32) -> Option<u32> {
        match self {
            Grid::Off => None,
            Grid::Sixteenth => Some((tpq / 4).max(1)),
            Grid::Eighth => Some((tpq / 2).max(1)),
            Grid::Quarter => Some(tpq.max(1)),
        }
    }
}

#[derive(Clone, Copy)]
enum DragMode {
    Pan,
    Move {
        index: usize,
        original: (u32, u8),
        grab_tick: f32,
        grab_pitch: i32,
    },
    Resize {
        index: usize,
    },
    Scrub,
}

#[derive(Default)]
struct Status {
    text: String,
    error: bool,
}

pub struct MidiApp {
    song: Song,
    file_name: Option<String>,
    dirty: bool,
    undo: Vec<Vec<Note>>,
    redo: Vec<Vec<Note>>,

    player: Player,
    play_from: u32,
    /// Current playhead position in ticks.
    playhead: f64,
    follow: bool,

    // view
    scroll_tick: f32,
    scroll_y: f32,
    px_per_beat: f32,
    row_h: f32,
    grid: Grid,
    note_len_beats: f32,
    center_on_load: bool,

    drag: Option<DragMode>,
    pending: PendingFile,
    status: Status,
}

impl Default for MidiApp {
    fn default() -> Self {
        Self {
            song: Song::default(),
            file_name: None,
            dirty: false,
            undo: Vec::new(),
            redo: Vec::new(),
            player: Player::new(),
            play_from: 0,
            playhead: 0.0,
            follow: true,
            scroll_tick: 0.0,
            scroll_y: 0.0,
            px_per_beat: 80.0,
            row_h: 14.0,
            grid: Grid::Sixteenth,
            note_len_beats: 1.0,
            center_on_load: true,
            drag: None,
            pending: Arc::new(Mutex::new(None)),
            status: Status {
                text: "Drop a .mid file anywhere, or click Open. Click the grid to add notes."
                    .into(),
                error: false,
            },
        }
    }
}

impl MidiApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self::default()
    }

    fn info(&mut self, text: impl Into<String>) {
        self.status = Status {
            text: text.into(),
            error: false,
        };
    }

    fn error(&mut self, text: impl Into<String>) {
        self.status = Status {
            text: text.into(),
            error: true,
        };
    }

    // ---------------------------------------------------------------- file handling

    pub fn load_bytes(&mut self, name: &str, bytes: &[u8]) {
        match Song::from_bytes(bytes) {
            Ok(song) => {
                self.player.stop();
                let n = song.notes.len();
                self.song = song;
                self.file_name = Some(name.to_owned());
                self.dirty = false;
                self.undo.clear();
                self.redo.clear();
                self.playhead = 0.0;
                self.play_from = 0;
                self.scroll_tick = 0.0;
                self.center_on_load = true;
                let tempo_note = if self.song.has_tempo_changes() {
                    " (has tempo changes)"
                } else {
                    ""
                };
                self.info(format!(
                    "Loaded {name}: {n} notes, {:.0} BPM{tempo_note}",
                    self.song.initial_bpm()
                ));
            }
            Err(e) => self.error(format!("Couldn't open {name}: {e}")),
        }
    }

    fn new_song(&mut self) {
        self.player.stop();
        *self = Self {
            pending: self.pending.clone(),
            px_per_beat: self.px_per_beat,
            row_h: self.row_h,
            grid: self.grid,
            ..Self::default()
        };
        self.info("New empty song. Click the grid to add notes.");
    }

    fn export_name(&self) -> String {
        match &self.file_name {
            Some(name) => {
                let stem = name
                    .strip_suffix(".midi")
                    .or_else(|| name.strip_suffix(".mid"))
                    .unwrap_or(name);
                format!("{stem}-edited.mid")
            }
            None => "song.mid".into(),
        }
    }

    fn open_dialog(&mut self, ctx: &egui::Context) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = ctx;
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("MIDI", &["mid", "midi"])
                .pick_file()
            {
                self.load_path(&path);
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            if let Err(e) = pick_file_web(self.pending.clone(), ctx.clone()) {
                self.error(format!("Couldn't open file picker: {e}"));
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn load_path(&mut self, path: &std::path::Path) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        match std::fs::read(path) {
            Ok(bytes) => self.load_bytes(&name, &bytes),
            Err(e) => self.error(format!("Couldn't read {}: {e}", path.display())),
        }
    }

    fn save(&mut self) {
        let bytes = self.song.to_bytes();
        let name = self.export_name();

        #[cfg(not(target_arch = "wasm32"))]
        {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("MIDI", &["mid", "midi"])
                .set_file_name(&name)
                .save_file()
            else {
                return;
            };
            match std::fs::write(&path, bytes) {
                Ok(()) => {
                    self.dirty = false;
                    self.info(format!("Saved {}", path.display()));
                }
                Err(e) => self.error(format!("Couldn't save: {e}")),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            match download(&bytes, &name) {
                Ok(()) => {
                    self.dirty = false;
                    self.info(format!("Downloaded {name}"));
                }
                Err(e) => self.error(format!("Download failed: {e}")),
            }
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(file) = dropped.into_iter().next() {
            let name = file
                .path()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "dropped file".into());

            #[cfg(not(target_arch = "wasm32"))]
            match file.bytes() {
                Ok(bytes) => self.load_bytes(&name, &bytes),
                Err(e) => self.error(format!("Couldn't read {name}: {e}")),
            }

            // Browsers only allow reading files asynchronously.
            #[cfg(target_arch = "wasm32")]
            {
                let pending = self.pending.clone();
                let ctx = ctx.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    match file.bytes_async().await {
                        Ok(bytes) => *pending.lock().unwrap() = Some((name, bytes)),
                        Err(e) => log::error!("Couldn't read dropped file {name}: {e}"),
                    }
                    ctx.request_repaint();
                });
            }
        }

        let picked = self.pending.lock().unwrap().take();
        if let Some((name, bytes)) = picked {
            self.load_bytes(&name, &bytes);
        }
    }

    // ---------------------------------------------------------------- editing

    fn checkpoint(&mut self) {
        self.undo.push(self.song.notes.clone());
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
    }

    fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.redo
                .push(std::mem::replace(&mut self.song.notes, prev));
            self.dirty = true;
            self.info("Undo");
        } else {
            self.info("Nothing to undo");
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo
                .push(std::mem::replace(&mut self.song.notes, next));
            self.dirty = true;
            self.info("Redo");
        }
    }

    // ---------------------------------------------------------------- playback

    fn toggle_play(&mut self) {
        if self.player.is_playing() {
            self.player.stop();
            self.info("Stopped");
        } else {
            self.start_playback(self.playhead.round() as u32);
        }
    }

    fn start_playback(&mut self, from: u32) {
        let from = if from >= self.song.end_tick() {
            0
        } else {
            from
        };
        if self.song.notes.is_empty() {
            self.info("Nothing to play yet: add some notes or open a file.");
            return;
        }
        self.play_from = from;
        self.playhead = from as f64;
        match self.player.play(&self.song, from) {
            Some(warning) => self.error(warning),
            None => self.info("Playing (Space to stop)"),
        }
    }

    fn update_playhead(&mut self) {
        if let Some(elapsed) = self.player.tick() {
            let start = self.song.tick_to_seconds(self.play_from);
            self.playhead = self.song.seconds_to_tick(start + elapsed);
        } else if self.status.text.starts_with("Playing") {
            self.info("Finished");
        }
    }

    // ---------------------------------------------------------------- UI

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("📂 Open")
                .on_hover_text("Open a .mid file (Ctrl+O)")
                .clicked()
            {
                self.open_dialog(ui.ctx());
            }
            if ui
                .button("🗋 New")
                .on_hover_text("Start an empty song")
                .clicked()
            {
                self.new_song();
            }
            let save_label = if cfg!(target_arch = "wasm32") {
                "💾 Download"
            } else {
                "💾 Save"
            };
            if ui
                .button(save_label)
                .on_hover_text("Export as a .mid file (Ctrl+S)")
                .clicked()
            {
                self.save();
            }

            ui.separator();

            let playing = self.player.is_playing();
            if ui
                .add(
                    egui::Button::new(if playing { "⏹ Stop" } else { "▶ Play" })
                        .min_size(vec2(64.0, 0.0)),
                )
                .on_hover_text("Space")
                .clicked()
            {
                self.toggle_play();
            }
            if ui
                .button("⏮")
                .on_hover_text("Back to start (Home)")
                .clicked()
            {
                self.player.stop();
                self.playhead = 0.0;
                self.scroll_tick = 0.0;
            }
            ui.checkbox(&mut self.follow, "Follow");

            ui.separator();

            ui.label("BPM");
            let mut bpm = self.song.initial_bpm();
            let resp = ui.add(
                egui::DragValue::new(&mut bpm)
                    .range(20.0..=300.0)
                    .speed(0.5)
                    .max_decimals(1),
            );
            if self.song.has_tempo_changes() {
                resp.clone().on_hover_text(
                    "This file changes tempo. Editing replaces it with one fixed tempo.",
                );
            }
            if resp.changed() {
                self.song.set_bpm(bpm);
                self.dirty = true;
                if self.player.is_playing() {
                    self.start_playback(self.playhead as u32);
                }
            }

            ui.separator();

            ui.label("Snap");
            egui::ComboBox::from_id_salt("snap")
                .selected_text(self.grid.label())
                .width(56.0)
                .show_ui(ui, |ui| {
                    for g in Grid::ALL {
                        ui.selectable_value(&mut self.grid, g, g.label());
                    }
                });

            ui.label("New note");
            egui::ComboBox::from_id_salt("len")
                .selected_text(beats_label(self.note_len_beats))
                .width(56.0)
                .show_ui(ui, |ui| {
                    for b in [0.25, 0.5, 1.0, 2.0, 4.0] {
                        ui.selectable_value(&mut self.note_len_beats, b, beats_label(b));
                    }
                });

            ui.separator();
            ui.label("Zoom");
            ui.add(
                egui::Slider::new(&mut self.px_per_beat, 10.0..=400.0)
                    .logarithmic(true)
                    .show_value(false),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::widgets::global_theme_preference_buttons(ui);
            });
        });
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let color = if self.status.error {
                ui.visuals().error_fg_color
            } else {
                ui.visuals().text_color()
            };
            ui.colored_label(color, &self.status.text);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let secs = self.song.tick_to_seconds(self.song.end_tick());
                let pos = self.song.tick_to_seconds(self.playhead.max(0.0) as u32);
                ui.weak(format!(
                    "{} / {}   ·   {} notes   ·   {}{}",
                    fmt_time(pos),
                    fmt_time(secs),
                    self.song.notes.len(),
                    self.file_name.as_deref().unwrap_or("untitled"),
                    if self.dirty { " •" } else { "" }
                ));
            });
        });
    }

    fn piano_roll(&mut self, ui: &mut egui::Ui) {
        let full = ui.available_rect_before_wrap();
        let tpq = self.song.ticks_per_quarter as f32;
        let ppt = self.px_per_beat / tpq; // pixels per tick
        let grid_rect = Rect::from_min_max(full.min + vec2(KEYBOARD_W, RULER_H), full.max);
        let ruler_rect = Rect::from_min_max(
            pos2(grid_rect.left(), full.top()),
            pos2(full.right(), grid_rect.top()),
        );
        let keys_rect = Rect::from_min_max(
            pos2(full.left(), grid_rect.top()),
            pos2(grid_rect.left(), full.bottom()),
        );

        let grid_resp = ui.interact(
            grid_rect,
            ui.scope_id().with("grid"),
            Sense::click_and_drag(),
        );
        let ruler_resp = ui.interact(
            ruler_rect,
            ui.scope_id().with("ruler"),
            Sense::click_and_drag(),
        );
        let keys_resp = ui.interact(keys_rect, ui.scope_id().with("keys"), Sense::hover());

        // ---- scroll & zoom (mouse wheel / trackpad / pinch)
        let hovered = grid_resp.hovered() || ruler_resp.hovered() || keys_resp.hovered();
        if hovered {
            let (scroll, zoom, shift, pointer) = ui.input(|i| {
                (
                    i.smooth_scroll_delta(),
                    i.zoom_delta(),
                    i.modifiers.shift,
                    i.pointer.hover_pos(),
                )
            });
            if zoom != 1.0 {
                // zoom horizontally around the pointer
                let anchor_x = pointer.map_or(grid_rect.left(), |p| p.x.max(grid_rect.left()));
                let tick_at = self.scroll_tick + (anchor_x - grid_rect.left()) / ppt;
                self.px_per_beat = (self.px_per_beat * zoom).clamp(10.0, 400.0);
                let new_ppt = self.px_per_beat / tpq;
                self.scroll_tick = tick_at - (anchor_x - grid_rect.left()) / new_ppt;
            } else {
                let (dx, dy) = if shift && scroll.x == 0.0 {
                    (scroll.y, 0.0)
                } else {
                    (scroll.x, scroll.y)
                };
                self.scroll_tick -= dx / ppt;
                self.scroll_y -= dy;
            }
        }

        let ppt = self.px_per_beat / tpq;
        let total_h = 128.0 * self.row_h;
        let end_tick = self.song.end_tick() as f32;
        let max_scroll_tick = (end_tick + tpq * 8.0 - grid_rect.width() / ppt).max(0.0);

        if self.center_on_load {
            self.center_on_load = false;
            let mid = if self.song.notes.is_empty() {
                60.0
            } else {
                let lo = self.song.notes.iter().map(|n| n.pitch).min().unwrap_or(60) as f32;
                let hi = self.song.notes.iter().map(|n| n.pitch).max().unwrap_or(72) as f32;
                (lo + hi) / 2.0
            };
            self.scroll_y = (127.0 - mid) * self.row_h - grid_rect.height() / 2.0;
        }

        // follow the playhead while playing
        if self.follow && self.player.is_playing() && self.drag.is_none() {
            let x = (self.playhead as f32 - self.scroll_tick) * ppt;
            if x > grid_rect.width() * 0.85 || x < 0.0 {
                self.scroll_tick = self.playhead as f32 - grid_rect.width() * 0.1 / ppt;
            }
        }

        // Allow scrolling a little past the last note, but never before the start.
        // (While playing/following, the playhead may push us further right.)
        if !self.player.is_playing() {
            self.scroll_tick = self.scroll_tick.min(max_scroll_tick);
        }
        self.scroll_tick = self.scroll_tick.max(0.0);
        self.scroll_y = self
            .scroll_y
            .clamp(0.0, (total_h - grid_rect.height()).max(0.0));

        // ---- coordinate helpers
        let scroll_tick = self.scroll_tick;
        let scroll_y = self.scroll_y;
        let row_h = self.row_h;
        let x_of = |tick: f32| grid_rect.left() + (tick - scroll_tick) * ppt;
        let tick_of = |x: f32| scroll_tick + (x - grid_rect.left()) / ppt;
        let y_of = |pitch: u8| grid_rect.top() + (127 - pitch) as f32 * row_h - scroll_y;
        let pitch_of = |y: f32| 127 - ((y - grid_rect.top() + scroll_y) / row_h).floor() as i32;
        let note_rect = |n: &Note| {
            Rect::from_min_max(
                pos2(x_of(n.start as f32), y_of(n.pitch)),
                pos2(
                    x_of(n.end as f32).max(x_of(n.start as f32) + 3.0),
                    y_of(n.pitch) + row_h,
                ),
            )
        };
        let snap = self.grid.ticks(self.song.ticks_per_quarter as u32);
        let snap_tick = |t: f32| -> u32 {
            let t = t.max(0.0);
            match snap {
                Some(s) => ((t / s as f32).floor() as u32) * s,
                None => t.round() as u32,
            }
        };
        let note_at = |notes: &[Note], p: Pos2| -> Option<usize> {
            notes.iter().rposition(|n| note_rect(n).contains(p))
        };

        // ---- interaction: grid
        let pointer = grid_resp.interact_pointer_pos().or(grid_resp.hover_pos());
        if let Some(p) = grid_resp.hover_pos() {
            if self.drag.is_none() {
                if let Some(i) = note_at(&self.song.notes, p) {
                    let r = note_rect(&self.song.notes[i]);
                    let icon = if p.x > r.right() - 6.0 {
                        CursorIcon::ResizeHorizontal
                    } else {
                        CursorIcon::Grab
                    };
                    ui.ctx().set_cursor_icon(icon);
                    let n = &self.song.notes[i];
                    grid_resp.clone().on_hover_text_at_pointer(format!(
                        "{}  vel {}  ch {}",
                        note_name(n.pitch),
                        n.velocity,
                        n.channel + 1
                    ));
                }
            }
        }

        if grid_resp.drag_started() {
            if let Some(p) = grid_resp.interact_pointer_pos() {
                self.drag = Some(match note_at(&self.song.notes, p) {
                    Some(i) => {
                        self.checkpoint();
                        let r = note_rect(&self.song.notes[i]);
                        let n = &self.song.notes[i];
                        if p.x > r.right() - 6.0 {
                            DragMode::Resize { index: i }
                        } else {
                            DragMode::Move {
                                index: i,
                                original: (n.start, n.pitch),
                                grab_tick: tick_of(p.x),
                                grab_pitch: pitch_of(p.y),
                            }
                        }
                    }
                    None => DragMode::Pan,
                });
            }
        }
        if grid_resp.dragged() {
            if let (Some(mode), Some(p)) = (self.drag, pointer) {
                match mode {
                    DragMode::Pan | DragMode::Scrub => {
                        let d = grid_resp.drag_delta();
                        self.scroll_tick -= d.x / ppt;
                        self.scroll_y -= d.y;
                    }
                    DragMode::Move {
                        index,
                        original,
                        grab_tick,
                        grab_pitch,
                    } => {
                        let dt = tick_of(p.x) - grab_tick;
                        let dp = pitch_of(p.y) - grab_pitch;
                        if let Some(n) = self.song.notes.get_mut(index) {
                            let len = n.duration();
                            let start = snap_tick(
                                original.0 as f32 + dt + snap.map_or(0.0, |s| s as f32 / 2.0),
                            );
                            n.start = start;
                            n.end = start + len;
                            n.pitch = (original.1 as i32 + dp).clamp(0, 127) as u8;
                        }
                    }
                    DragMode::Resize { index } => {
                        if let Some(n) = self.song.notes.get_mut(index) {
                            let min = snap.unwrap_or(1);
                            let end =
                                snap_tick(tick_of(p.x) + snap.map_or(0.0, |s| s as f32 / 2.0));
                            n.end = end.max(n.start + min);
                        }
                    }
                }
            }
        }
        if grid_resp.drag_stopped() {
            if matches!(
                self.drag,
                Some(DragMode::Move { .. } | DragMode::Resize { .. })
            ) {
                self.song.sort();
            }
            self.drag = None;
        }

        if grid_resp.clicked() || grid_resp.secondary_clicked() {
            if let Some(p) = grid_resp.interact_pointer_pos() {
                if let Some(i) = note_at(&self.song.notes, p) {
                    self.checkpoint();
                    let n = self.song.notes.remove(i);
                    self.info(format!("Removed {}", note_name(n.pitch)));
                } else if grid_resp.clicked() {
                    let pitch = pitch_of(p.y);
                    if (0..=127).contains(&pitch) {
                        self.checkpoint();
                        let start = snap_tick(tick_of(p.x));
                        let len = (self.note_len_beats * tpq).round().max(1.0) as u32;
                        self.song.notes.push(Note {
                            pitch: pitch as u8,
                            velocity: 100,
                            channel: 0,
                            start,
                            end: start + len,
                        });
                        self.song.sort();
                        self.info(format!("Added {}", note_name(pitch as u8)));
                    }
                }
            }
        }

        // ---- interaction: ruler (click / drag to move the playhead)
        if ruler_resp.clicked() || ruler_resp.dragged() {
            if let Some(p) = ruler_resp.interact_pointer_pos() {
                let was_playing = self.player.is_playing();
                self.player.stop();
                self.playhead = tick_of(p.x).max(0.0) as f64;
                if was_playing && ruler_resp.clicked() {
                    self.start_playback(self.playhead as u32);
                }
                self.drag = Some(DragMode::Scrub);
            }
        }
        if ruler_resp.drag_stopped() {
            self.drag = None;
        }

        // ---- painting
        let painter = ui.painter_at(full);
        let v = ui.visuals().clone();
        let dark = v.dark_mode;
        let white_row = if dark {
            Color32::from_gray(38)
        } else {
            Color32::from_gray(248)
        };
        let black_row = if dark {
            Color32::from_gray(30)
        } else {
            Color32::from_gray(232)
        };
        let line = if dark {
            Color32::from_gray(48)
        } else {
            Color32::from_gray(215)
        };
        let beat_line = if dark {
            Color32::from_gray(58)
        } else {
            Color32::from_gray(200)
        };
        let bar_line = if dark {
            Color32::from_gray(90)
        } else {
            Color32::from_gray(150)
        };

        let gp = painter.with_clip_rect(grid_rect);
        gp.rect_filled(grid_rect, 0.0, white_row);

        let first_pitch = pitch_of(grid_rect.bottom()).clamp(0, 127) as u8;
        let last_pitch = pitch_of(grid_rect.top()).clamp(0, 127) as u8;
        for pitch in first_pitch..=last_pitch {
            let y = y_of(pitch);
            if is_black_key(pitch) {
                gp.rect_filled(
                    Rect::from_min_max(
                        pos2(grid_rect.left(), y),
                        pos2(grid_rect.right(), y + row_h),
                    ),
                    0.0,
                    black_row,
                );
            }
            let stroke_col = if pitch % 12 == 0 { beat_line } else { line };
            gp.hline(grid_rect.x_range(), y + row_h, Stroke::new(1.0, stroke_col));
        }

        // vertical lines
        let tpq_u = self.song.ticks_per_quarter as u32;
        let sub = snap.filter(|s| *s as f32 * ppt >= 8.0);
        let step = sub.unwrap_or(tpq_u).max(1);
        let beat_step_visible = tpq * ppt >= 6.0;
        let view_start = tick_of(grid_rect.left()).max(0.0) as u32;
        let view_end = tick_of(grid_rect.right()).max(0.0) as u32 + step;
        let mut t = (view_start / step) * step;
        while t <= view_end {
            let x = x_of(t as f32);
            let (col, w) = if t.is_multiple_of(tpq_u * 4) {
                (bar_line, 1.0)
            } else if t.is_multiple_of(tpq_u) {
                if beat_step_visible {
                    (beat_line, 1.0)
                } else {
                    (Color32::TRANSPARENT, 0.0)
                }
            } else {
                (line, 1.0)
            };
            if w > 0.0 {
                gp.vline(x, grid_rect.y_range(), Stroke::new(w, col));
            }
            t += step;
            if t == 0 {
                break;
            }
        }

        // notes
        let label_font = FontId::proportional((row_h - 4.0).clamp(8.0, 12.0));
        let dragging_idx = match self.drag {
            Some(DragMode::Move { index, .. } | DragMode::Resize { index }) => Some(index),
            _ => None,
        };
        let playhead = self.playhead as u32;
        let playing = self.player.is_playing();
        for (i, n) in self.song.notes.iter().enumerate() {
            let r = note_rect(n);
            if !r.intersects(grid_rect) {
                continue;
            }
            let base = channel_color(n.channel);
            let sounding = playing && n.start <= playhead && playhead < n.end;
            let fill = if sounding {
                base.gamma_multiply(1.0).lerp_to_gamma(Color32::WHITE, 0.45)
            } else {
                base.gamma_multiply(0.55 + 0.45 * n.velocity as f32 / 127.0)
            };
            gp.rect_filled(r.shrink(0.5), CornerRadius::same(3), fill);
            let outline = if Some(i) == dragging_idx {
                Stroke::new(2.0, v.selection.stroke.color)
            } else {
                Stroke::new(1.0, Color32::from_black_alpha(90))
            };
            gp.rect_stroke(
                r.shrink(0.5),
                CornerRadius::same(3),
                outline,
                StrokeKind::Inside,
            );
            if r.width() > 28.0 && row_h >= 11.0 {
                gp.text(
                    r.left_center() + vec2(4.0, 0.0),
                    Align2::LEFT_CENTER,
                    note_name(n.pitch),
                    label_font.clone(),
                    Color32::from_black_alpha(200),
                );
            }
        }

        // playhead
        let px = x_of(self.playhead as f32);
        let ph_color = Color32::from_rgb(235, 70, 60);
        if grid_rect.x_range().contains(px) {
            painter.vline(
                px,
                pos2(px, ruler_rect.top()).y..=grid_rect.bottom(),
                Stroke::new(2.0, ph_color),
            );
        }

        // ruler
        let rp = painter.with_clip_rect(ruler_rect);
        rp.rect_filled(ruler_rect, 0.0, v.extreme_bg_color);
        let bar = tpq_u * 4;
        let bar_px = bar as f32 * ppt;
        let label_every = (60.0 / bar_px).ceil().max(1.0) as u32;
        let mut b = view_start / bar;
        while b * bar <= view_end {
            let x = x_of((b * bar) as f32);
            rp.vline(x, ruler_rect.y_range(), Stroke::new(1.0, bar_line));
            if b.is_multiple_of(label_every) {
                rp.text(
                    pos2(x + 4.0, ruler_rect.center().y),
                    Align2::LEFT_CENTER,
                    format!("{}", b + 1),
                    FontId::proportional(11.0),
                    v.weak_text_color(),
                );
            }
            b += 1;
        }
        if ruler_rect.x_range().contains(px) {
            rp.add(egui::Shape::convex_polygon(
                vec![
                    pos2(px - 6.0, ruler_rect.top() + 4.0),
                    pos2(px + 6.0, ruler_rect.top() + 4.0),
                    pos2(px, ruler_rect.bottom()),
                ],
                ph_color,
                Stroke::NONE,
            ));
        }
        if ruler_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }

        // keyboard
        let kp = painter.with_clip_rect(keys_rect);
        kp.rect_filled(keys_rect, 0.0, Color32::from_gray(245));
        for pitch in first_pitch..=last_pitch {
            let y = y_of(pitch);
            let r = Rect::from_min_max(
                pos2(keys_rect.left(), y),
                pos2(keys_rect.right(), y + row_h),
            );
            if is_black_key(pitch) {
                kp.rect_filled(
                    Rect::from_min_max(r.min, pos2(r.left() + KEYBOARD_W * 0.62, r.bottom())),
                    CornerRadius::same(2),
                    Color32::from_gray(30),
                );
            } else {
                kp.hline(
                    r.x_range(),
                    r.bottom(),
                    Stroke::new(1.0, Color32::from_gray(200)),
                );
            }
            if pitch % 12 == 0 && row_h >= 9.0 {
                kp.text(
                    pos2(r.right() - 4.0, r.center().y),
                    Align2::RIGHT_CENTER,
                    note_name(pitch),
                    FontId::proportional((row_h - 3.0).clamp(8.0, 11.0)),
                    Color32::from_gray(80),
                );
            }
        }
        painter.vline(
            keys_rect.right(),
            keys_rect.y_range(),
            Stroke::new(1.0, bar_line),
        );
        painter.rect_filled(
            Rect::from_min_max(full.min, pos2(keys_rect.right(), ruler_rect.bottom())),
            0.0,
            v.extreme_bg_color,
        );

        // empty-state hint
        if self.song.notes.is_empty() {
            painter.text(
                grid_rect.center(),
                Align2::CENTER_CENTER,
                "Drop a .mid file here or click 📂 Open\nClick anywhere on the grid to add a note",
                FontId::proportional(16.0),
                v.weak_text_color(),
            );
        }

        ui.advance_cursor_after_rect(full);
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        // Don't steal keys while typing in a text field (e.g. the BPM box).
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let cmd = |k| KeyboardShortcut::new(Modifiers::COMMAND, k);
        let cmd_shift = |k| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, k);
        let (space, home, undo, redo, redo_y, open, save) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::Space),
                i.consume_key(Modifiers::NONE, Key::Home),
                i.consume_shortcut(&cmd(Key::Z)),
                i.consume_shortcut(&cmd_shift(Key::Z)),
                i.consume_shortcut(&cmd(Key::Y)),
                i.consume_shortcut(&cmd(Key::O)),
                i.consume_shortcut(&cmd(Key::S)),
            )
        });
        if space {
            self.toggle_play();
        }
        if home {
            self.player.stop();
            self.playhead = 0.0;
            self.scroll_tick = 0.0;
        }
        if redo || redo_y {
            self.redo();
        } else if undo {
            self.undo();
        }
        if open {
            self.open_dialog(ctx);
        }
        if save {
            self.save();
        }
    }
}

impl eframe::App for MidiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_dropped_files(&ctx);
        self.shortcuts(&ctx);
        self.update_playhead();
        if self.player.is_playing() {
            ctx.request_repaint();
        }

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.add_space(2.0);
            self.status_bar(ui);
            ui.add_space(2.0);
        });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| self.piano_roll(ui));

        // file hover overlay
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let screen = ctx.content_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::unique("drop_overlay"),
            ));
            painter.rect_filled(screen, 0.0, Color32::from_black_alpha(170));
            painter.text(
                screen.center(),
                Align2::CENTER_CENTER,
                "Drop to open MIDI file",
                FontId::proportional(28.0),
                Color32::WHITE,
            );
        }
    }
}

fn channel_color(channel: u8) -> Color32 {
    const PALETTE: [Color32; 16] = [
        Color32::from_rgb(86, 156, 255),
        Color32::from_rgb(255, 140, 66),
        Color32::from_rgb(92, 201, 120),
        Color32::from_rgb(236, 94, 120),
        Color32::from_rgb(178, 120, 255),
        Color32::from_rgb(255, 205, 70),
        Color32::from_rgb(60, 205, 205),
        Color32::from_rgb(240, 120, 210),
        Color32::from_rgb(150, 190, 70),
        Color32::from_rgb(160, 160, 160), // ch 10 = drums
        Color32::from_rgb(255, 110, 80),
        Color32::from_rgb(110, 140, 230),
        Color32::from_rgb(200, 160, 110),
        Color32::from_rgb(90, 220, 170),
        Color32::from_rgb(220, 130, 255),
        Color32::from_rgb(250, 170, 150),
    ];
    PALETTE[channel as usize % 16]
}

fn beats_label(b: f32) -> &'static str {
    match b {
        x if x <= 0.25 => "1/16",
        x if x <= 0.5 => "1/8",
        x if x <= 1.0 => "1/4",
        x if x <= 2.0 => "1/2",
        _ => "1 bar",
    }
}

fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0);
    format!("{}:{:04.1}", (s / 60.0) as u32, s % 60.0)
}

/// Show the browser's native file picker. egui handles the click a frame after
/// the real event, which is still within the browser's "user activation" window.
#[cfg(target_arch = "wasm32")]
fn pick_file_web(pending: PendingFile, ctx: egui::Context) -> Result<(), String> {
    use wasm_bindgen::JsCast;
    let err = |e: wasm_bindgen::JsValue| format!("{e:?}");

    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("no document")?;
    let input: web_sys::HtmlInputElement = document
        .create_element("input")
        .map_err(err)?
        .dyn_into()
        .map_err(|_| "not an input")?;
    input.set_type("file");
    input.set_accept(".mid,.midi,audio/midi,audio/x-midi");

    let input2 = input.clone();
    let on_change = wasm_bindgen::closure::Closure::once_into_js(move || {
        let Some(file) = input2.files().and_then(|f| f.get(0)) else {
            return;
        };
        wasm_bindgen_futures::spawn_local(async move {
            let name = file.name();
            match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                Ok(buf) => {
                    let bytes = js_sys::Uint8Array::new(&buf).to_vec();
                    *pending.lock().unwrap() = Some((name, bytes));
                }
                Err(e) => log::error!("Couldn't read {name}: {e:?}"),
            }
            ctx.request_repaint();
        });
    });
    input.set_onchange(Some(on_change.unchecked_ref()));
    input.click();
    Ok(())
}

/// Trigger a browser download of `bytes`.
#[cfg(target_arch = "wasm32")]
fn download(bytes: &[u8], filename: &str) -> Result<(), String> {
    use wasm_bindgen::JsCast;
    let err = |e: wasm_bindgen::JsValue| format!("{e:?}");

    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type("audio/midi");
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &opts).map_err(err)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(err)?;

    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or("no document")?;
    let body = document.body().ok_or("no body")?;
    let a: web_sys::HtmlAnchorElement = document
        .create_element("a")
        .map_err(err)?
        .dyn_into()
        .map_err(|_| "not an anchor")?;
    a.set_href(&url);
    a.set_download(filename);
    let _ = a.style().set_property("display", "none");
    body.append_child(&a).map_err(err)?;
    a.click();
    let _ = body.remove_child(&a);
    // Revoke later; revoking immediately can cancel the download in some browsers.
    let cb = wasm_bindgen::closure::Closure::once_into_js(move || {
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    if let Some(w) = web_sys::window() {
        let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(cb.unchecked_ref(), 10_000);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_name_strips_extension() {
        let mut app = MidiApp {
            file_name: Some("song.mid".into()),
            ..Default::default()
        };
        assert_eq!(app.export_name(), "song-edited.mid");
        app.file_name = Some("x.midi".into());
        assert_eq!(app.export_name(), "x-edited.mid");
        app.file_name = None;
        assert_eq!(app.export_name(), "song.mid");
    }

    #[test]
    fn undo_redo() {
        let mut app = MidiApp::default();
        app.checkpoint();
        app.song.notes.push(Note {
            pitch: 60,
            velocity: 100,
            channel: 0,
            start: 0,
            end: 10,
        });
        app.undo();
        assert!(app.song.notes.is_empty());
        app.redo();
        assert_eq!(app.song.notes.len(), 1);
    }

    #[test]
    fn bad_file_sets_error_and_keeps_song() {
        let mut app = MidiApp::default();
        app.song.notes.push(Note {
            pitch: 60,
            velocity: 100,
            channel: 0,
            start: 0,
            end: 10,
        });
        app.load_bytes("junk.mid", b"nope");
        assert!(app.status.error);
        assert_eq!(app.song.notes.len(), 1);
    }

    #[test]
    fn fmt_time_works() {
        assert_eq!(fmt_time(0.0), "0:00.0");
        assert_eq!(fmt_time(75.25), "1:15.2");
    }
}
