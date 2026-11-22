//! MIDI file model: parsing a Standard MIDI File into a flat list of notes,
//! converting ticks to seconds via the tempo map, and exporting back to bytes.

use midly::num::{u15, u24, u28, u4, u7};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};

/// Microseconds per quarter note at 120 BPM (the MIDI default).
pub const DEFAULT_US_PER_BEAT: u32 = 500_000;
pub const DEFAULT_TICKS_PER_QUARTER: u16 = 480;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub pitch: u8,
    pub velocity: u8,
    pub channel: u8,
    pub start: u32,
    pub end: u32,
}

impl Note {
    pub fn duration(&self) -> u32 {
        self.end.saturating_sub(self.start)
    }
}

/// A tempo change at an absolute tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TempoChange {
    pub tick: u32,
    pub us_per_beat: u32,
}

#[derive(Clone, Debug)]
pub struct Song {
    pub notes: Vec<Note>,
    pub ticks_per_quarter: u16,
    /// Sorted by tick; always has at least one entry at tick 0.
    pub tempo_map: Vec<TempoChange>,
}

impl Default for Song {
    fn default() -> Self {
        Self {
            notes: Vec::new(),
            ticks_per_quarter: DEFAULT_TICKS_PER_QUARTER,
            tempo_map: vec![TempoChange {
                tick: 0,
                us_per_beat: DEFAULT_US_PER_BEAT,
            }],
        }
    }
}

impl Song {
    /// Parse a Standard MIDI File.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let smf = Smf::parse(bytes).map_err(|e| format!("not a valid MIDI file: {e}"))?;

        let ticks_per_quarter = match smf.header.timing {
            Timing::Metrical(t) if t.as_int() > 0 => t.as_int(),
            Timing::Metrical(_) => DEFAULT_TICKS_PER_QUARTER,
            // SMPTE timing: treat each tick as fps*subframes per second and
            // express it as an equivalent fixed tempo.
            Timing::Timecode(fps, sub) => {
                let ticks_per_sec = (fps.as_f32() * sub as f32).max(1.0);
                // ticks_per_quarter at 120 BPM (0.5s per beat)
                (ticks_per_sec * 0.5).round().clamp(1.0, 32767.0) as u16
            }
        };

        let mut notes = Vec::new();
        let mut tempo_map = Vec::new();

        for track in &smf.tracks {
            let mut tick: u32 = 0;
            // (channel, pitch) -> stack of (start, velocity), so overlapping
            // notes of the same pitch are handled sensibly.
            let mut active: Vec<Vec<(u32, u8)>> = vec![Vec::new(); 16 * 128];

            for event in track {
                tick = tick.saturating_add(event.delta.as_int());
                match event.kind {
                    TrackEventKind::Meta(MetaMessage::Tempo(us)) if us.as_int() > 0 => {
                        tempo_map.push(TempoChange {
                            tick,
                            us_per_beat: us.as_int(),
                        });
                    }
                    TrackEventKind::Midi { channel, message } => {
                        let ch = channel.as_int();
                        match message {
                            MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                                active[ch as usize * 128 + key.as_int() as usize]
                                    .push((tick, vel.as_int()));
                            }
                            MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                                let slot = &mut active[ch as usize * 128 + key.as_int() as usize];
                                if !slot.is_empty() {
                                    let (start, velocity) = slot.remove(0);
                                    notes.push(Note {
                                        pitch: key.as_int(),
                                        velocity,
                                        channel: ch,
                                        start,
                                        end: tick.max(start + 1),
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }

            // Close any notes that never got a note-off at the end of the track.
            for (idx, slot) in active.iter().enumerate() {
                for &(start, velocity) in slot {
                    notes.push(Note {
                        pitch: (idx % 128) as u8,
                        velocity,
                        channel: (idx / 128) as u8,
                        start,
                        end: tick.max(start + 1),
                    });
                }
            }
        }

        tempo_map.sort_by_key(|t| t.tick);
        tempo_map.dedup_by_key(|t| t.tick); // keep the first change at a given tick
        if tempo_map.first().is_none_or(|t| t.tick != 0) {
            tempo_map.insert(
                0,
                TempoChange {
                    tick: 0,
                    us_per_beat: DEFAULT_US_PER_BEAT,
                },
            );
        }

        let mut song = Self {
            notes,
            ticks_per_quarter,
            tempo_map,
        };
        song.sort();
        Ok(song)
    }

    /// Export as a single-track (format 0) Standard MIDI File.
    pub fn to_bytes(&self) -> Vec<u8> {
        // (tick, order, kind) — at equal ticks, tempo first, then note-offs,
        // then note-ons, so repeated notes are not cut short.
        let mut events: Vec<(u32, u8, TrackEventKind<'static>)> = Vec::new();

        for t in &self.tempo_map {
            events.push((
                t.tick,
                0,
                TrackEventKind::Meta(MetaMessage::Tempo(u24::new(t.us_per_beat.min(0xFF_FFFF)))),
            ));
        }
        for n in &self.notes {
            let channel = u4::new(n.channel.min(15));
            let key = u7::new(n.pitch.min(127));
            events.push((
                n.start,
                2,
                TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::NoteOn {
                        key,
                        vel: u7::new(n.velocity.clamp(1, 127)),
                    },
                },
            ));
            events.push((
                n.end.max(n.start + 1),
                1,
                TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::NoteOff {
                        key,
                        vel: u7::new(0),
                    },
                },
            ));
        }
        events.sort_by_key(|(tick, order, _)| (*tick, *order));

        let mut track = Vec::with_capacity(events.len() + 1);
        let mut last = 0u32;
        for (tick, _, kind) in events {
            track.push(TrackEvent {
                delta: u28::new(tick - last),
                kind,
            });
            last = tick;
        }
        track.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        });

        let smf = Smf {
            header: Header::new(
                Format::SingleTrack,
                Timing::Metrical(u15::new(self.ticks_per_quarter.clamp(1, 0x7FFF))),
            ),
            tracks: vec![track],
        };
        let mut buf = Vec::new();
        smf.write_std(&mut buf)
            .expect("writing MIDI to a Vec cannot fail");
        buf
    }

    pub fn sort(&mut self) {
        self.notes.sort_by_key(|n| (n.start, n.pitch));
    }

    /// Tempo at tick 0, in BPM.
    pub fn initial_bpm(&self) -> f64 {
        60_000_000.0 / self.tempo_map[0].us_per_beat as f64
    }

    /// Replace the whole tempo map with a single fixed tempo.
    pub fn set_bpm(&mut self, bpm: f64) {
        let us = (60_000_000.0 / bpm.clamp(10.0, 400.0)).round() as u32;
        self.tempo_map = vec![TempoChange {
            tick: 0,
            us_per_beat: us,
        }];
    }

    pub fn has_tempo_changes(&self) -> bool {
        self.tempo_map.len() > 1
    }

    /// Last tick at which any note is sounding.
    pub fn end_tick(&self) -> u32 {
        self.notes.iter().map(|n| n.end).max().unwrap_or(0)
    }

    /// Convert an absolute tick to seconds using the tempo map.
    pub fn tick_to_seconds(&self, tick: u32) -> f64 {
        let tpq = self.ticks_per_quarter as f64;
        let mut seconds = 0.0;
        let mut prev = self.tempo_map[0];
        for t in &self.tempo_map[1..] {
            if t.tick >= tick {
                break;
            }
            seconds += (t.tick - prev.tick) as f64 * prev.us_per_beat as f64 / 1e6 / tpq;
            prev = *t;
        }
        seconds + (tick - prev.tick.min(tick)) as f64 * prev.us_per_beat as f64 / 1e6 / tpq
    }

    /// Convert seconds back to an absolute tick using the tempo map.
    pub fn seconds_to_tick(&self, seconds: f64) -> f64 {
        let tpq = self.ticks_per_quarter as f64;
        let mut elapsed = 0.0;
        let mut prev = self.tempo_map[0];
        for t in &self.tempo_map[1..] {
            let seg = (t.tick - prev.tick) as f64 * prev.us_per_beat as f64 / 1e6 / tpq;
            if elapsed + seg > seconds {
                break;
            }
            elapsed += seg;
            prev = *t;
        }
        prev.tick as f64 + (seconds - elapsed) * 1e6 * tpq / prev.us_per_beat as f64
    }
}

/// Note name such as "C4" (MIDI 60 = C4).
pub fn note_name(pitch: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!("{}{}", NAMES[pitch as usize % 12], pitch as i32 / 12 - 1)
}

pub fn is_black_key(pitch: u8) -> bool {
    matches!(pitch % 12, 1 | 3 | 6 | 8 | 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(delta: u32, kind: TrackEventKind<'static>) -> TrackEvent<'static> {
        TrackEvent {
            delta: u28::new(delta),
            kind,
        }
    }

    fn on(ch: u8, key: u8, vel: u8) -> TrackEventKind<'static> {
        TrackEventKind::Midi {
            channel: u4::new(ch),
            message: MidiMessage::NoteOn {
                key: u7::new(key),
                vel: u7::new(vel),
            },
        }
    }

    fn off(ch: u8, key: u8) -> TrackEventKind<'static> {
        TrackEventKind::Midi {
            channel: u4::new(ch),
            message: MidiMessage::NoteOff {
                key: u7::new(key),
                vel: u7::new(0),
            },
        }
    }

    fn write(tracks: Vec<Vec<TrackEvent<'static>>>) -> Vec<u8> {
        let smf = Smf {
            header: Header::new(Format::Parallel, Timing::Metrical(u15::new(480))),
            tracks,
        };
        let mut buf = Vec::new();
        smf.write_std(&mut buf).unwrap();
        buf
    }

    #[test]
    fn parses_basic_notes_and_tempo() {
        let bytes = write(vec![vec![
            ev(
                0,
                TrackEventKind::Meta(MetaMessage::Tempo(u24::new(1_000_000))),
            ),
            ev(0, on(0, 60, 100)),
            ev(10, off(0, 60)),
            ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ]]);
        let song = Song::from_bytes(&bytes).unwrap();
        assert_eq!(song.ticks_per_quarter, 480);
        assert_eq!(song.initial_bpm(), 60.0);
        assert_eq!(
            song.notes,
            vec![Note {
                pitch: 60,
                velocity: 100,
                channel: 0,
                start: 0,
                end: 10
            }]
        );
    }

    #[test]
    fn note_on_with_zero_velocity_is_note_off() {
        let bytes = write(vec![vec![
            ev(0, on(0, 60, 100)),
            ev(10, on(0, 60, 0)),
            ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ]]);
        let song = Song::from_bytes(&bytes).unwrap();
        assert_eq!(song.notes.len(), 1);
        assert_eq!(song.notes[0].end, 10);
    }

    #[test]
    fn channels_and_tracks_are_kept_separate() {
        let bytes = write(vec![
            vec![
                ev(0, on(0, 60, 90)),
                ev(0, on(1, 60, 80)),
                ev(5, off(1, 60)),
                ev(5, off(0, 60)),
            ],
            vec![ev(3, on(2, 64, 70)), ev(3, off(2, 64))],
        ]);
        let song = Song::from_bytes(&bytes).unwrap();
        assert_eq!(song.notes.len(), 3);
        let ch0 = song.notes.iter().find(|n| n.channel == 0).unwrap();
        let ch1 = song.notes.iter().find(|n| n.channel == 1).unwrap();
        let ch2 = song.notes.iter().find(|n| n.channel == 2).unwrap();
        assert_eq!((ch0.start, ch0.end), (0, 10));
        assert_eq!((ch1.start, ch1.end), (0, 5));
        assert_eq!((ch2.start, ch2.end), (3, 6));
    }

    #[test]
    fn unterminated_notes_are_closed() {
        let bytes = write(vec![vec![ev(0, on(0, 60, 100)), ev(20, on(0, 62, 100))]]);
        let song = Song::from_bytes(&bytes).unwrap();
        assert_eq!(song.notes.len(), 2);
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(Song::from_bytes(b"definitely not midi").is_err());
    }

    #[test]
    fn export_roundtrip_keeps_notes_and_tempo() {
        let mut song = Song::default();
        song.set_bpm(90.0);
        song.notes = vec![
            Note {
                pitch: 60,
                velocity: 100,
                channel: 0,
                start: 0,
                end: 480,
            },
            // same pitch immediately repeated: must not be swallowed
            Note {
                pitch: 60,
                velocity: 90,
                channel: 0,
                start: 480,
                end: 960,
            },
            Note {
                pitch: 67,
                velocity: 80,
                channel: 3,
                start: 240,
                end: 720,
            },
        ];
        song.sort();
        let back = Song::from_bytes(&song.to_bytes()).unwrap();
        assert_eq!(back.notes, song.notes);
        assert_eq!(back.ticks_per_quarter, 480);
        assert!((back.initial_bpm() - 90.0).abs() < 0.01);
    }

    #[test]
    fn tick_seconds_conversion_follows_tempo_map() {
        let mut song = Song::default(); // 120 BPM, 480 tpq => 1 beat = 0.5s
        song.tempo_map.push(TempoChange {
            tick: 960,
            us_per_beat: 1_000_000, // 60 BPM after two beats
        });
        assert!((song.tick_to_seconds(480) - 0.5).abs() < 1e-9);
        assert!((song.tick_to_seconds(960) - 1.0).abs() < 1e-9);
        assert!((song.tick_to_seconds(1440) - 2.0).abs() < 1e-9);
        assert!((song.seconds_to_tick(2.0) - 1440.0).abs() < 1e-6);
        assert!((song.seconds_to_tick(0.25) - 240.0).abs() < 1e-6);
    }

    #[test]
    fn note_names() {
        assert_eq!(note_name(60), "C4");
        assert_eq!(note_name(69), "A4");
        assert_eq!(note_name(0), "C-1");
        assert!(is_black_key(61));
        assert!(!is_black_key(60));
    }
}
