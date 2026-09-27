//! Flight recorder: fine-grained logs for tuning the flight model.
//!
//! Every flight (each time the game is entered) writes one JSON Lines file,
//! one record per line, tagged by `"type"`:
//! - `header`: format version, creation time, and every tuning constant the
//!   flight was flown with (so old logs stay interpretable).
//! - `spawn`: the aircraft (type and full airframe) and its initial state.
//! - `frame`, every rendered frame — what the pilot did and saw: each raw
//!   mouse-motion event, the keys held, the resulting mouse aim, free look,
//!   the camera, and the rendered (interpolated) plane pose.
//! - `tick`, every fixed 60 Hz tick — what the plane did: the exact input
//!   the flight model got, the instructor's reasoning, the surface command
//!   and the resulting state.
//! - `mark`: the pilot pressed `M` ("that felt wrong, look here").
//!
//! Ticks run before the frame's input is gathered, so the aim a tick flies
//! toward is the one recorded by the previous frame. `aces-client replay`
//! (see `replay`) re-flies a log through the same code.
//!
//! Units are SI with angles in radians; quaternions are `[x, y, z, w]`.
//! Logs go to `flightlogs/` at the repository root in debug builds. Set
//! `ACES_FLIGHT_LOG` to a directory to choose another, or to `off` to
//! disable; release builds record only when it is set.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use bevy::ecs::message::MessageReader;
use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::Phase;
use crate::flight::input::{FlightInput, FreeLook, MouseAim};
use crate::flight::instructor::{Instructor, InstructorDebug};
use crate::flight::model::{FlightState, LastCommand, step_flight};
use crate::flight::{Aircraft, AngularRates, LocalPlane, Surfaces, camera};

/// Bumped whenever a record changes shape.
pub const FORMAT_VERSION: u32 = 3;
/// Environment variable choosing the log directory (`off` disables).
pub const LOG_DIR_ENV: &str = "ACES_FLIGHT_LOG";

// ── Records ─────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Record {
    Header(Header),
    Spawn(Spawn),
    Frame(FrameRecord),
    Tick(TickRecord),
    Mark(Mark),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Header {
    pub version: u32,
    /// UTC, `YYYYMMDD-HHMMSS`.
    pub created: String,
    pub tick_hz: f64,
    pub tuning: BTreeMap<String, f32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Spawn {
    pub frame: u64,
    pub state: StateRecord,
    /// The aircraft flown (absent in logs before format 3).
    #[serde(default)]
    pub aircraft: Option<AircraftRecord>,
}

/// Which aircraft a flight was flown on, with every number of its airframe
/// as it was then.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct AircraftRecord {
    pub index: u8,
    pub name: String,
    pub airframe: aces_protocol::Airframe,
}

/// One rendered frame.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FrameRecord {
    pub frame: u64,
    /// Game time [s].
    pub t: f64,
    pub dt: f32,
    /// Raw mouse-motion events this frame, in mouse counts (x right, y down).
    pub mouse: Vec<[f32; 2]>,
    /// Held keys: `W S A D Q E V` as themselves, `L` for the limiter
    /// (`Shift`).
    pub keys: String,
    /// Free-look orbit `[yaw, pitch]`.
    pub look: [f32; 2],
    /// Mouse aim after this frame's input (forward = -Z).
    pub aim: [f32; 4],
    pub cam_pos: [f32; 3],
    pub cam_rot: [f32; 4],
    /// The plane as rendered (interpolated between ticks).
    pub plane_pos: [f32; 3],
    pub plane_rot: [f32; 4],
}

/// One fixed tick of the flight model.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TickRecord {
    pub tick: u64,
    /// The frame this tick ran in (before that frame's input was gathered).
    pub frame: u64,
    /// Fixed-clock time after the tick [s].
    pub t: f64,
    pub dt: f32,
    pub input: InputRecord,
    pub instructor: InstructorDebug,
    /// Surface command (instructor + keyboard overrides): elevator,
    /// aileron, rudder.
    pub command: [f32; 3],
    /// State after the tick.
    pub state: StateRecord,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Mark {
    pub frame: u64,
    pub tick: u64,
    pub t: f64,
}

/// [`FlightInput`] on the wire.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct InputRecord {
    pub aim: [f32; 3],
    pub roll: Option<f32>,
    pub yaw: Option<f32>,
    pub throttle: f32,
    pub limiter_off: bool,
}

impl From<&FlightInput> for InputRecord {
    fn from(i: &FlightInput) -> Self {
        Self {
            aim: i.aim.to_array(),
            roll: i.roll_override,
            yaw: i.yaw_override,
            throttle: i.throttle_delta,
            limiter_off: i.limiter_off,
        }
    }
}

impl InputRecord {
    pub fn to_input(self) -> FlightInput {
        FlightInput {
            aim: Vec3::from(self.aim),
            roll_override: self.roll,
            yaw_override: self.yaw,
            throttle_delta: self.throttle,
            limiter_off: self.limiter_off,
        }
    }
}

/// [`FlightState`] on the wire.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct StateRecord {
    pub pos: [f32; 3],
    pub quat: [f32; 4],
    pub vel: [f32; 3],
    /// Body rates `[pitch, yaw, roll]`.
    pub omega: [f32; 3],
    /// Actual surfaces `[elevator, aileron, rudder]`.
    pub surfaces: [f32; 3],
    pub throttle: f32,
    pub engine: f32,
    pub alpha: f32,
    pub beta: f32,
    pub g_load: f32,
    pub stalled: bool,
    pub buffet_phase: f32,
}

impl From<&FlightState> for StateRecord {
    fn from(s: &FlightState) -> Self {
        Self {
            pos: s.pos.to_array(),
            quat: s.quat.to_array(),
            vel: s.vel.to_array(),
            omega: [s.omega.pitch, s.omega.yaw, s.omega.roll],
            surfaces: s.surfaces.to_array(),
            throttle: s.throttle,
            engine: s.engine,
            alpha: s.alpha,
            beta: s.beta,
            g_load: s.g_load,
            stalled: s.stalled,
            buffet_phase: s.buffet_phase,
        }
    }
}

impl StateRecord {
    pub fn to_state(self) -> FlightState {
        FlightState {
            pos: Vec3::from(self.pos),
            quat: Quat::from_array(self.quat),
            vel: Vec3::from(self.vel),
            omega: AngularRates {
                pitch: self.omega[0],
                yaw: self.omega[1],
                roll: self.omega[2],
            },
            surfaces: Surfaces::from_array(self.surfaces),
            throttle: self.throttle,
            engine: self.engine,
            alpha: self.alpha,
            beta: self.beta,
            g_load: self.g_load,
            stalled: self.stalled,
            buffet_phase: self.buffet_phase,
        }
    }
}

/// The header for a log written now with the current constants.
pub fn header() -> Header {
    Header {
        version: FORMAT_VERSION,
        created: utc_stamp(std::time::SystemTime::now()),
        tick_hz: aces_protocol::TICK_HZ,
        tuning: crate::flight::tuning()
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect(),
    }
}

/// `YYYYMMDD-HHMMSS` (UTC) for `time`.
pub fn utc_stamp(time: std::time::SystemTime) -> String {
    let secs = time
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

// ── Recording ───────────────────────────────────────────────────────────────

/// Where logs go, from [`LOG_DIR_ENV`] (debug builds default to
/// `flightlogs/` at the repository root).
pub fn log_dir_from_env() -> Option<PathBuf> {
    match std::env::var(LOG_DIR_ENV) {
        Ok(v) if v.is_empty() || v == "off" || v == "0" => None,
        Ok(v) => Some(PathBuf::from(v)),
        Err(_) => {
            #[cfg(debug_assertions)]
            if let Some(dir) = option_env!("CARGO_MANIFEST_DIR") {
                return Some(Path::new(dir).join("..").join("flightlogs"));
            }
            None
        }
    }
}

pub struct RecorderPlugin {
    pub dir: Option<PathBuf>,
}

impl Plugin for RecorderPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(FlightRecorder {
            dir: self.dir.clone(),
            out: None,
            frame: 0,
            tick: 0,
        })
        .add_systems(First, count_frames)
        .add_systems(OnEnter(Phase::InGame), start_log)
        .add_systems(OnExit(Phase::InGame), stop_log)
        // Nothing is built, formatted or written unless a log is open.
        .add_systems(
            FixedUpdate,
            (
                record_spawn.before(step_flight),
                record_tick.after(step_flight),
            )
                .run_if(recording),
        )
        .add_systems(
            Update,
            (record_frame, mark)
                .chain()
                .after(camera::update_camera)
                .run_if(in_state(Phase::InGame).and_then(recording)),
        )
        .add_systems(Last, flush_on_exit);
    }
}

/// Run condition: a flight log is open.
fn recording(recorder: Res<FlightRecorder>) -> bool {
    recorder.out.is_some()
}

/// Write buffer: roughly a second of flight, so the main thread makes about
/// one write call per second (the periodic flush) instead of a dozen.
const WRITE_BUFFER: usize = 128 * 1024;

/// The open log, if any, and the frame/tick counters.
#[derive(Resource)]
pub struct FlightRecorder {
    dir: Option<PathBuf>,
    out: Option<(PathBuf, BufWriter<File>)>,
    frame: u64,
    tick: u64,
}

impl FlightRecorder {
    fn write(&mut self, record: &Record) {
        let Some((path, out)) = &mut self.out else {
            return;
        };
        let result = serde_json::to_writer(&mut *out, record)
            .map_err(std::io::Error::from)
            .and_then(|()| out.write_all(b"\n"));
        if let Err(err) = result {
            warn!(
                "flight log {} failed, recording stopped: {err}",
                path.display()
            );
            self.out = None;
        }
    }

    fn close(&mut self) {
        if let Some((path, mut out)) = self.out.take() {
            match out.flush() {
                Ok(()) => info!("flight log saved: {}", path.display()),
                Err(err) => warn!("flight log {} failed: {err}", path.display()),
            }
        }
    }

    /// The file being written, if recording.
    pub fn path(&self) -> Option<&Path> {
        self.out.as_ref().map(|(path, _)| path.as_path())
    }
}

fn count_frames(mut recorder: ResMut<FlightRecorder>) {
    recorder.frame += 1;
}

fn start_log(mut recorder: ResMut<FlightRecorder>) {
    recorder.close();
    recorder.tick = 0;
    let Some(dir) = recorder.dir.clone() else {
        return;
    };
    let header = header();
    let opened = std::fs::create_dir_all(&dir).and_then(|()| {
        let mut path = dir.join(format!("flight-{}.jsonl", header.created));
        let mut n = 1;
        while path.exists() {
            n += 1;
            path = dir.join(format!("flight-{}-{n}.jsonl", header.created));
        }
        Ok((
            path.clone(),
            BufWriter::with_capacity(WRITE_BUFFER, File::create(path)?),
        ))
    });
    match opened {
        Ok(out) => {
            info!("recording flight to {}", out.0.display());
            recorder.out = Some(out);
            recorder.write(&Record::Header(header));
        }
        Err(err) => warn!("cannot record flight into {}: {err}", dir.display()),
    }
}

fn stop_log(mut recorder: ResMut<FlightRecorder>) {
    recorder.close();
}

fn flush_on_exit(mut exits: MessageReader<AppExit>, mut recorder: ResMut<FlightRecorder>) {
    if exits.read().next().is_some() {
        recorder.close();
    }
}

/// The local plane, on the tick it appeared.
type NewLocalPlane = (With<LocalPlane>, Added<FlightState>);

fn record_spawn(
    mut recorder: ResMut<FlightRecorder>,
    spawned: Query<(&FlightState, &Aircraft), NewLocalPlane>,
) {
    for (state, aircraft) in &spawned {
        let frame = recorder.frame;
        recorder.write(&Record::Spawn(Spawn {
            frame,
            state: state.into(),
            aircraft: Some(AircraftRecord {
                index: aircraft.index,
                name: aircraft.name().to_string(),
                airframe: aircraft.airframe,
            }),
        }));
    }
}

/// Runs right after `step_flight` in the same fixed tick, so the input
/// resource and the tick length are exactly what the model flew on.
fn record_tick(
    time: Res<Time>,
    input: Res<FlightInput>,
    mut recorder: ResMut<FlightRecorder>,
    plane: Single<(&FlightState, &Instructor, &LastCommand), With<LocalPlane>>,
) {
    let (state, instructor, command) = *plane;
    recorder.tick += 1;
    let record = Record::Tick(TickRecord {
        tick: recorder.tick,
        frame: recorder.frame,
        t: time.elapsed_secs_f64(),
        dt: time.delta_secs(),
        input: (&*input).into(),
        instructor: instructor.debug,
        command: command.0.to_array(),
        state: state.into(),
    });
    recorder.write(&record);
    // Keep a crash from losing more than a second of flight.
    if recorder.tick.is_multiple_of(60)
        && let Some((_, out)) = &mut recorder.out
    {
        let _ = out.flush();
    }
}

#[allow(clippy::too_many_arguments)]
fn record_frame(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut mouse_motion: MessageReader<MouseMotion>,
    aim: Res<MouseAim>,
    free_look: Res<FreeLook>,
    camera: Single<&Transform, (With<Camera3d>, Without<LocalPlane>)>,
    plane: Single<&Transform, With<LocalPlane>>,
    mut recorder: ResMut<FlightRecorder>,
) {
    let mouse = mouse_motion.read().map(|m| m.delta.to_array()).collect();
    let mut held = String::new();
    for (key, c) in [
        (KeyCode::KeyW, 'W'),
        (KeyCode::KeyS, 'S'),
        (KeyCode::KeyA, 'A'),
        (KeyCode::KeyD, 'D'),
        (KeyCode::KeyQ, 'Q'),
        (KeyCode::KeyE, 'E'),
        (KeyCode::KeyV, 'V'),
    ] {
        if keys.pressed(key) {
            held.push(c);
        }
    }
    if keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]) {
        held.push('L');
    }
    let frame = recorder.frame;
    recorder.write(&Record::Frame(FrameRecord {
        frame,
        t: time.elapsed_secs_f64(),
        dt: time.delta_secs(),
        mouse,
        keys: held,
        look: [free_look.yaw, free_look.pitch],
        aim: aim.rot.to_array(),
        cam_pos: camera.translation.to_array(),
        cam_rot: camera.rotation.to_array(),
        plane_pos: plane.translation.to_array(),
        plane_rot: plane.rotation.to_array(),
    }));
}

/// `M` drops a marker into the log, to find a moment again when tuning.
fn mark(time: Res<Time>, keys: Res<ButtonInput<KeyCode>>, mut recorder: ResMut<FlightRecorder>) {
    if !keys.just_pressed(KeyCode::KeyM) {
        return;
    }
    let (frame, tick) = (recorder.frame, recorder.tick);
    let t = time.elapsed_secs_f64();
    recorder.write(&Record::Mark(Mark { frame, tick, t }));
    if let Some(path) = recorder.path() {
        info!("mark at t = {t:.2} s (tick {tick}) in {}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_stamps_are_civil_dates() {
        let at = |secs| utc_stamp(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs));
        assert_eq!(at(0), "19700101-000000");
        assert_eq!(at(951_782_400), "20000229-000000");
        assert_eq!(at(1_790_000_000 + 3661), "20260921-151421");
    }

    /// Records survive the JSON round trip bit-exactly — the replay depends
    /// on it.
    #[test]
    fn records_round_trip_exactly() {
        let mut state = FlightState::new(
            Vec3::new(1.5, 1234.567, -9.25),
            Quat::from_rotation_z(0.3),
            171.3,
            0.9,
        );
        state.omega.roll = 1.0 / 3.0;
        state.alpha = 0.1234567;
        let record = Record::Tick(TickRecord {
            tick: 7,
            frame: 9,
            t: 1.0 / 60.0,
            dt: 1.0 / 60.0,
            input: InputRecord {
                aim: [0.1, -0.2, -0.974_679_4],
                roll: Some(-1.0),
                yaw: None,
                throttle: 1.0,
                limiter_off: true,
            },
            instructor: InstructorDebug {
                err: 0.3333333,
                ..default()
            },
            command: [0.1, -0.7, 0.0],
            state: (&state).into(),
        });
        let line = serde_json::to_string(&record).unwrap();
        assert!(line.starts_with(r#"{"type":"tick""#), "{line}");
        let back: Record = serde_json::from_str(&line).unwrap();
        assert_eq!(back, record);
        let Record::Tick(tick) = back else {
            unreachable!()
        };
        let restored = tick.state.to_state();
        assert_eq!(restored.pos.to_array(), state.pos.to_array());
        assert_eq!(restored.omega.roll.to_bits(), state.omega.roll.to_bits());
    }
}
