use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use crate::app::Note;

/// Cross-platform playback controller. On native platforms this uses `midir` to send
/// NoteOn/NoteOff to an available MIDI output. On wasm this is a stub (to be
/// implemented with WebAudio later).
pub struct Player {
    #[cfg(not(target_arch = "wasm32"))]
    inner: Option<native::NativePlayer>,

    #[cfg(target_arch = "wasm32")]
    inner: Option<wasm::WasmPlayer>,
}

impl Player {
    pub fn new() -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            inner: None,

            #[cfg(target_arch = "wasm32")]
            inner: None,
        }
    }

    /// Start playback of the provided notes. `ticks_per_quarter` is used to convert
    /// ticks into seconds together with `tempo_bpm`.
    pub fn start(&mut self, notes: Vec<Note>, ticks_per_quarter: u32, tempo_bpm: u32) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let p = native::NativePlayer::start(notes, ticks_per_quarter, tempo_bpm);
            self.inner = Some(p);
        }

        #[cfg(target_arch = "wasm32")]
        {
            let p = wasm::WasmPlayer::start(notes, ticks_per_quarter, tempo_bpm);
            self.inner = Some(p);
        }
    }

    pub fn stop(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(p) = &mut self.inner {
                p.stop();
            }
            self.inner = None;
        }

        #[cfg(target_arch = "wasm32")]
        {
            if let Some(p) = &mut self.inner {
                p.stop();
            }
            self.inner = None;
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use midir::{MidiOutput, MidiOutputConnection};
    use std::thread;
    use std::time::{Duration, Instant};

    pub struct NativePlayer {
        handle: Option<thread::JoinHandle<()>>,
        stop_flag: Arc<AtomicBool>,
    }

    impl NativePlayer {
        pub fn start(notes: Vec<Note>, ticks_per_quarter: u32, tempo_bpm: u32) -> Self {
            let stop_flag = Arc::new(AtomicBool::new(false));

            // Try to open the first available MIDI output port.
            let midi_out = MidiOutput::new("eframe_template_playback");

            let conn = match midi_out {
                Ok(out) => {
                    let ports = out.ports();
                    if ports.is_empty() {
                        eprintln!("No MIDI output ports found");
                        None
                    } else {
                        // Open the first port (best-effort)
                        match out.connect(&ports[0], "eframe_playback") {
                            Ok(c) => Some(c),
                            Err(e) => {
                                eprintln!("Failed to open MIDI port: {}", e);
                                None
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Failed to create MidiOutput: {}", e);
                    None
                }
            };

            // Build chronological events (time in seconds, is_on, key, vel)
            let mut events: Vec<(f64, bool, u8, u8)> = Vec::new();
            for note in &notes {
                let seconds_per_tick = (60.0 / tempo_bpm as f64) / ticks_per_quarter as f64;
                let start_s = note.start_time as f64 * seconds_per_tick;
                let end_s = note.end_time as f64 * seconds_per_tick;
                events.push((start_s, true, note.pitch, note.velocity));
                events.push((end_s, false, note.pitch, 0));
            }

            events.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

            let stop_clone = stop_flag.clone();

            let handle = std::thread::spawn(move || {
                let mut conn = conn;
                let start_instant = Instant::now();
                for (time_s, is_on, key, vel) in events {
                    if stop_clone.load(Ordering::Relaxed) {
                        break;
                    }
                    let target = start_instant + Duration::from_secs_f64(time_s);
                    // Busy-wait sleep loop for slightly better accuracy
                    loop {
                        if stop_clone.load(Ordering::Relaxed) {
                            break;
                        }
                        let now = Instant::now();
                        if now >= target {
                            break;
                        }
                        let remaining = target - now;
                        if remaining > Duration::from_millis(5) {
                            thread::sleep(Duration::from_millis(3));
                        } else {
                            std::thread::yield_now();
                        }
                    }

                    if stop_clone.load(Ordering::Relaxed) {
                        break;
                    }

                    if let Some(c) = conn.as_mut() {
                        let msg = if is_on { vec![0x90, key, vel] } else { vec![0x80, key, vel] };
                        let _ = c.send(&msg);
                    }
                }
            });

            NativePlayer {
                handle: Some(handle),
                stop_flag,
            }
        }

        pub fn stop(&mut self) {
            self.stop_flag.store(true, Ordering::Relaxed);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::*;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;

    pub struct WasmPlayer {
        timeouts: Vec<i32>,
        _context: Option<web_sys::AudioContext>,
    }

    fn midi_to_freq(pitch: u8) -> f32 {
        440.0 * 2f32.powf((pitch as f32 - 69.0) / 12.0)
    }

    impl WasmPlayer {
        pub fn start(notes: Vec<Note>, ticks_per_quarter: u32, tempo_bpm: u32) -> Self {
            let window = web_sys::window().expect("no window");
            let perf = window
                .performance()
                .expect("performance should be available");

            let ctx = web_sys::AudioContext::new().ok();

            let seconds_per_tick = (60.0 / tempo_bpm as f64) / ticks_per_quarter as f64;

            let mut timeouts: Vec<i32> = Vec::new();

            for note in notes.into_iter() {
                let start_ms = (note.start_time as f64) * (seconds_per_tick * 1000.0);
                let dur_ms = ((note.end_time as f64 - note.start_time as f64) * seconds_per_tick
                    * 1000.0)
                    .max(1.0);

                // Schedule start closure
                let ctx_clone = ctx.clone();
                let window_clone = window.clone();
                let note_pitch = note.pitch;
                let start_closure = Closure::wrap(Box::new(move || {
                    if let Some(ctx2) = &ctx_clone {
                        if let Ok(osc) = ctx2.create_oscillator() {
                            // set basic sine and frequency
                            let freq = midi_to_freq(note_pitch);
                            let _ = osc.frequency().set_value(freq);
                            let dest = ctx2.destination();
                            let _ = osc.connect_with_audio_node(&dest);
                            let _ = osc.start();

                            // schedule stop for this oscillator
                            let osc_for_stop = osc;
                            let stop_closure = Closure::wrap(Box::new(move || {
                                let _ = osc_for_stop.stop();
                            }) as Box<dyn FnMut()>);
                            let _ = window_clone.set_timeout_with_callback_and_timeout_and_arguments_0(
                                stop_closure.as_ref().unchecked_ref(),
                                dur_ms as i32,
                            );
                            stop_closure.forget();
                        }
                    } else {
                        // Fallback: if no AudioContext, do nothing
                    }
                }) as Box<dyn FnMut()>);

                if let Ok(id) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                    start_closure.as_ref().unchecked_ref(),
                    start_ms as i32,
                ) {
                    timeouts.push(id);
                }
                start_closure.forget();
            }

            WasmPlayer {
                timeouts,
                _context: ctx,
            }
        }

        pub fn stop(&mut self) {
            let window = web_sys::window().expect("no window");
            for id in &self.timeouts {
                window.clear_timeout_with_handle(*id);
            }
            self.timeouts.clear();
            if let Some(ctx) = &self._context {
                let _ = ctx.suspend();
            }
        }
    }
}
