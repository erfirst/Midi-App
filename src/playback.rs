//! Cross-platform playback.
//!
//! * Native: notes are sent to the first available MIDI output port with `midir`
//!   (on Windows that is usually "Microsoft GS Wavetable Synth").
//! * Web: a small built-in synthesizer on top of the Web Audio API, scheduled with a
//!   lookahead so timing is sample-accurate and long files don't create thousands of
//!   timers up front.

use crate::midi::Song;

/// One note, with times in seconds relative to the playback start.
#[derive(Clone, Copy, Debug)]
struct Scheduled {
    start: f64,
    end: f64,
    pitch: u8,
    velocity: u8,
    channel: u8,
}

fn build_schedule(song: &Song, from_tick: u32) -> (Vec<Scheduled>, f64) {
    let offset = song.tick_to_seconds(from_tick);
    let mut out: Vec<Scheduled> = song
        .notes
        .iter()
        .filter(|n| n.end > from_tick)
        .map(|n| Scheduled {
            start: (song.tick_to_seconds(n.start.max(from_tick)) - offset).max(0.0),
            end: song.tick_to_seconds(n.end) - offset,
            pitch: n.pitch,
            velocity: n.velocity,
            channel: n.channel,
        })
        .collect();
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    let duration = out.iter().map(|n| n.end).fold(0.0, f64::max);
    (out, duration)
}

pub struct Player {
    inner: Option<backend::Backend>,
    duration: f64,
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}

impl Player {
    pub fn new() -> Self {
        Self {
            inner: None,
            duration: 0.0,
        }
    }

    /// Start playing `song` from `from_tick`. Returns a human readable warning if
    /// playback can't produce sound (e.g. no MIDI output device).
    pub fn play(&mut self, song: &Song, from_tick: u32) -> Option<String> {
        self.stop();
        let (schedule, duration) = build_schedule(song, from_tick);
        self.duration = duration;
        let (backend, warning) = backend::Backend::start(schedule);
        self.inner = Some(backend);
        warning
    }

    pub fn stop(&mut self) {
        if let Some(mut b) = self.inner.take() {
            b.stop();
        }
    }

    pub fn is_playing(&self) -> bool {
        self.inner.is_some()
    }

    /// Seconds since playback started, or `None` when stopped.
    /// Call once per frame: this also feeds the scheduler and stops at the end.
    pub fn tick(&mut self) -> Option<f64> {
        let b = self.inner.as_mut()?;
        let elapsed = b.update();
        if elapsed > self.duration + 0.25 {
            self.stop();
            return None;
        }
        Some(elapsed)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}

// --------------------------------------------------------------------------------------
#[cfg(not(target_arch = "wasm32"))]
mod backend {
    use super::Scheduled;
    use midir::MidiOutput;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    pub struct Backend {
        started: Instant,
        stop_flag: Arc<AtomicBool>,
        handle: Option<thread::JoinHandle<()>>,
    }

    fn connect() -> Result<(midir::MidiOutputConnection, String), String> {
        let out = MidiOutput::new("Midi App").map_err(|e| e.to_string())?;
        let ports = out.ports();
        // Prefer a real synth over Linux's "Midi Through" loopback port.
        let port = ports
            .iter()
            .find(|p| {
                out.port_name(p)
                    .map(|n| !n.contains("Through"))
                    .unwrap_or(false)
            })
            .or_else(|| ports.first())
            .ok_or_else(|| "no MIDI output device found".to_owned())?
            .clone();
        let name = out.port_name(&port).unwrap_or_else(|_| "MIDI out".into());
        let conn = out
            .connect(&port, "midi-app-playback")
            .map_err(|e| e.to_string())?;
        Ok((conn, name))
    }

    impl Backend {
        pub fn start(schedule: Vec<Scheduled>) -> (Self, Option<String>) {
            let (conn, warning) = match connect() {
                Ok((c, name)) => {
                    log::info!("Playing through MIDI port {name}");
                    (Some(c), None)
                }
                Err(e) => (
                    None,
                    Some(format!(
                        "No sound: {e}. Connect a MIDI synth to hear playback."
                    )),
                ),
            };

            // Flatten to (time, order, message); note-offs sort before note-ons.
            let mut events: Vec<(f64, u8, [u8; 3])> = Vec::with_capacity(schedule.len() * 2);
            for n in &schedule {
                let ch = n.channel & 0x0F;
                events.push((n.start, 1, [0x90 | ch, n.pitch, n.velocity.max(1)]));
                events.push((n.end, 0, [0x80 | ch, n.pitch, 0]));
            }
            events.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));

            let stop_flag = Arc::new(AtomicBool::new(false));
            let stop = stop_flag.clone();
            let started = Instant::now();

            let handle = thread::spawn(move || {
                let Some(mut conn) = conn else { return };
                for (time, _, msg) in events {
                    let target = started + Duration::from_secs_f64(time);
                    loop {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let now = Instant::now();
                        if now >= target {
                            break;
                        }
                        thread::sleep((target - now).min(Duration::from_millis(5)));
                    }
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let _ = conn.send(&msg);
                }
                // Silence everything so nothing is left hanging.
                for ch in 0..16u8 {
                    let _ = conn.send(&[0xB0 | ch, 123, 0]); // all notes off
                    let _ = conn.send(&[0xB0 | ch, 64, 0]); // sustain pedal off
                }
                conn.close();
            });

            (
                Self {
                    started,
                    stop_flag,
                    handle: Some(handle),
                },
                warning,
            )
        }

        pub fn update(&mut self) -> f64 {
            self.started.elapsed().as_secs_f64()
        }

        pub fn stop(&mut self) {
            self.stop_flag.store(true, Ordering::Relaxed);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }
}

// --------------------------------------------------------------------------------------
#[cfg(target_arch = "wasm32")]
mod backend {
    use super::Scheduled;
    use std::cell::RefCell;
    use wasm_bindgen::JsCast;
    use web_sys::{AudioContext, AudioContextState, AudioNode, GainNode, OscillatorType};

    /// How far ahead of the current audio time notes are scheduled.
    const LOOKAHEAD: f64 = 1.0;
    /// Small delay before the first note so scheduling never lands in the past.
    const START_DELAY: f64 = 0.08;

    thread_local! {
        static CONTEXT: RefCell<Option<(AudioContext, AudioNode)>> = const { RefCell::new(None) };
    }

    /// Get (or lazily create) the shared AudioContext and its output chain.
    ///
    /// `index.html` creates and unlocks `window.__midiAudioCtx` inside a real user
    /// gesture, which browsers (Safari especially) require before audio may play.
    /// We reuse that context instead of creating a new one for every playback —
    /// browsers only allow a handful of AudioContexts per page.
    fn context() -> Option<(AudioContext, AudioNode)> {
        CONTEXT.with(|cell| {
            let mut cell = cell.borrow_mut();
            if cell.is_none() {
                let window = web_sys::window()?;
                let ctx = js_sys::Reflect::get(&window, &"__midiAudioCtx".into())
                    .ok()
                    .and_then(|v| v.dyn_into::<AudioContext>().ok())
                    .or_else(|| AudioContext::new().ok())?;
                let comp = ctx.create_dynamics_compressor().ok()?;
                comp.threshold().set_value(-12.0);
                comp.ratio().set_value(6.0);
                comp.connect_with_audio_node(&ctx.destination()).ok()?;
                *cell = Some((ctx, comp.unchecked_into()));
            }
            cell.clone()
        })
    }

    fn midi_to_freq(pitch: u8) -> f32 {
        440.0 * 2f32.powf((pitch as f32 - 69.0) / 12.0)
    }

    pub struct Backend {
        ctx: Option<AudioContext>,
        master: Option<GainNode>,
        t0: f64,
        schedule: Vec<Scheduled>,
        next: usize,
    }

    impl Backend {
        pub fn start(schedule: Vec<Scheduled>) -> (Self, Option<String>) {
            let Some((ctx, out)) = context() else {
                return (
                    Self {
                        ctx: None,
                        master: None,
                        t0: 0.0,
                        schedule,
                        next: 0,
                    },
                    Some("Web Audio is not available in this browser".into()),
                );
            };
            if ctx.state() != AudioContextState::Running {
                let _ = ctx.resume();
            }
            // A fresh master gain per playback: disconnecting it silences
            // everything that was already scheduled.
            let master = ctx.create_gain().ok();
            if let Some(m) = &master {
                m.gain().set_value(0.6);
                let _ = m.connect_with_audio_node(&out);
            }
            let t0 = ctx.current_time() + START_DELAY;
            let mut b = Self {
                ctx: Some(ctx),
                master,
                t0,
                schedule,
                next: 0,
            };
            b.update();
            (b, None)
        }

        fn play_note(ctx: &AudioContext, dest: &GainNode, n: &Scheduled, t0: f64) -> Option<()> {
            let start = t0 + n.start;
            let end = (t0 + n.end).max(start + 0.03);
            let drums = n.channel == 9;
            let peak = (n.velocity as f32 / 127.0) * if drums { 0.08 } else { 0.22 };

            let osc = ctx.create_oscillator().ok()?;
            let env = ctx.create_gain().ok()?;
            if drums {
                // Percussion: short high blip rather than a pitched tone.
                osc.set_type(OscillatorType::Square);
                osc.frequency()
                    .set_value(120.0 + (n.pitch as f32 - 35.0).max(0.0) * 40.0);
            } else {
                osc.set_type(OscillatorType::Triangle);
                osc.frequency().set_value(midi_to_freq(n.pitch));
            }

            let g = env.gain();
            g.set_value_at_time(0.0, start).ok()?;
            g.linear_ramp_to_value_at_time(peak, start + 0.008).ok()?;
            if drums {
                g.set_target_at_time(0.0, start + 0.008, 0.03).ok()?;
            } else {
                // decay to a sustain level, then release
                g.set_target_at_time(peak * 0.55, start + 0.008, 0.25)
                    .ok()?;
                g.set_target_at_time(0.0, end, 0.04).ok()?;
            }

            osc.connect_with_audio_node(&env).ok()?;
            env.connect_with_audio_node(dest).ok()?;
            osc.start_with_when(start).ok()?;
            osc.stop_with_when(if drums { start + 0.2 } else { end + 0.3 })
                .ok()?;
            Some(())
        }

        pub fn update(&mut self) -> f64 {
            let (Some(ctx), Some(master)) = (&self.ctx, &self.master) else {
                return f64::INFINITY; // no audio: end immediately
            };
            let now = ctx.current_time();
            while let Some(n) = self.schedule.get(self.next) {
                if self.t0 + n.start > now + LOOKAHEAD {
                    break;
                }
                // Skip notes whose time already passed (e.g. tab was in background).
                if self.t0 + n.end > now {
                    let _ = Self::play_note(ctx, master, n, self.t0);
                }
                self.next += 1;
            }
            (now - self.t0).max(0.0)
        }

        pub fn stop(&mut self) {
            if let (Some(ctx), Some(m)) = (&self.ctx, self.master.take()) {
                let g = m.gain();
                let now = ctx.current_time();
                let _ = g.cancel_scheduled_values(now);
                let _ = g.set_target_at_time(0.0, now, 0.01);
                // disconnect slightly later to avoid a click
                let window = web_sys::window();
                let cb = wasm_bindgen::closure::Closure::once_into_js(move || {
                    let _ = m.disconnect();
                });
                if let Some(w) = window {
                    let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                        cb.unchecked_ref(),
                        80,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::Note;

    #[test]
    fn schedule_starts_from_tick() {
        let song = Song {
            notes: vec![
                Note {
                    pitch: 60,
                    velocity: 100,
                    channel: 0,
                    start: 0,
                    end: 480,
                },
                Note {
                    pitch: 62,
                    velocity: 100,
                    channel: 0,
                    start: 960,
                    end: 1440,
                },
            ],
            ..Song::default() // 120bpm, 480 tpq
        };
        let (s, dur) = build_schedule(&song, 960);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].pitch, 62);
        assert!(s[0].start.abs() < 1e-9);
        assert!((dur - 0.5).abs() < 1e-9);

        // starting mid-note keeps the note but trims its start
        let (s, _) = build_schedule(&song, 240);
        assert_eq!(s.len(), 2);
        assert!(s[0].start.abs() < 1e-9);
        assert!((s[0].end - 0.25).abs() < 1e-9);
    }

    #[test]
    fn stopping_an_idle_player_is_fine() {
        let mut p = Player::new();
        p.stop();
        assert!(!p.is_playing());
        assert!(p.tick().is_none());
    }
}
