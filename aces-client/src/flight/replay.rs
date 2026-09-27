//! Flight-log replay:
//! `aces-client replay <log.jsonl> [--mouse] [--recorded-airframe]
//! [--csv <out.csv>]`.
//!
//! Re-flies a recorded flight (see `recorder`) through the current
//! instructor and flight model — the same [`fly_tick`] the game runs — and
//! compares the result with the recording:
//! - by default each tick gets the exact input it was recorded with, which
//!   isolates instructor and flight-model tuning;
//! - `--mouse` also re-derives the aim from the raw mouse events with the
//!   current mouse-aim code (sensitivity, view leveling).
//!
//! The plane is re-flown with the *current* definition of the aircraft it
//! was recorded on, so airframe edits show; `--recorded-airframe` flies the
//! airframe exactly as logged instead (logs before format 3 did not record
//! one and use aircraft 0).
//!
//! With unchanged constants the replay reproduces the recording bit for bit,
//! which doubles as a check that the log is complete. After a tuning change
//! it shows how the same stick work would have flown now: the report lists
//! the constants that changed since the recording, twitchiness metrics
//! (recorded vs replayed) for the whole flight and around every `M` mark,
//! and `--csv` writes both flights tick by tick for plotting.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write as _};
use std::path::Path;

use bevy::prelude::*;

use crate::flight::Surfaces;
use crate::flight::input::{AIM_SENSITIVITY, MouseAim};
use crate::flight::instructor::{Instructor, InstructorDebug};
use crate::flight::model::{Airframe, FlightState, airframe_tuning, fly_tick};
use crate::flight::recorder::{FrameRecord, Header, Mark, Record, Spawn, TickRecord};

const USAGE: &str =
    "usage: aces-client replay <log.jsonl> [--mouse] [--recorded-airframe] [--csv <out.csv>]";

/// A parsed flight log.
#[derive(Debug, Default)]
pub struct Log {
    pub header: Option<Header>,
    pub segments: Vec<Segment>,
    pub marks: Vec<Mark>,
    /// Lines that did not parse (e.g. a truncated last line after a crash).
    pub skipped: usize,
}

/// One life of the plane: its spawn and everything until the next spawn.
#[derive(Debug, Clone)]
pub struct Segment {
    pub spawn: Spawn,
    pub frames: Vec<FrameRecord>,
    pub ticks: Vec<TickRecord>,
}

pub fn load(path: &Path) -> Result<Log, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(BufReader::new(file))
}

pub fn parse(reader: impl BufRead) -> Result<Log, String> {
    let mut log = Log::default();
    // Frames can be recorded before the spawn record of their segment (the
    // first frame in game may run before the first fixed tick).
    let mut pending_frames = Vec::new();
    for line in reader.lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Record>(&line) else {
            log.skipped += 1;
            continue;
        };
        match record {
            Record::Header(h) => log.header = Some(h),
            Record::Spawn(spawn) => log.segments.push(Segment {
                spawn,
                frames: std::mem::take(&mut pending_frames),
                ticks: Vec::new(),
            }),
            Record::Frame(f) => match log.segments.last_mut() {
                Some(seg) => seg.frames.push(f),
                None => pending_frames.push(f),
            },
            Record::Tick(t) => match log.segments.last_mut() {
                Some(seg) => seg.ticks.push(t),
                None => log.skipped += 1,
            },
            Record::Mark(m) => log.marks.push(m),
        }
    }
    if log.segments.is_empty() {
        return Err("no spawn record: nothing to replay".into());
    }
    Ok(log)
}

/// One tick of a flight, recorded or replayed.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub tick: u64,
    pub t: f64,
    pub state: FlightState,
    pub command: Surfaces,
    pub debug: InstructorDebug,
}

impl Sample {
    fn recorded(r: &TickRecord) -> Self {
        Self {
            tick: r.tick,
            t: r.t,
            state: r.state.to_state(),
            command: Surfaces::from_array(r.command),
            debug: r.instructor,
        }
    }
}

/// How far a replay drifted from its recording.
#[derive(Debug, Clone, Copy, Default)]
pub struct Divergence {
    /// Every replayed state matched its recording bit for bit.
    pub exact: bool,
    pub max_position: f32,
    /// Largest attitude difference [rad].
    pub max_attitude: f32,
    /// First tick that drifted by more than 1 cm or 0.01°.
    pub first_tick: Option<u64>,
}

/// The airframe to re-fly `segment` with, and a description of it: the
/// current definition of the recorded aircraft, or with `recorded` the
/// airframe exactly as logged.
pub fn airframe_for(segment: &Segment, recorded: bool) -> (Airframe, String) {
    match &segment.spawn.aircraft {
        Some(logged) if recorded => (logged.airframe, format!("{} (as recorded)", logged.name)),
        Some(logged) => {
            let kind = aces_protocol::aircraft(logged.index);
            (kind.airframe, format!("{} (current definition)", kind.name))
        }
        None => {
            let kind = aces_protocol::aircraft(0);
            (
                kind.airframe,
                format!("{} (not in log; current definition)", kind.name),
            )
        }
    }
}

/// Re-fly `segment` on `airframe`; with `mouse`, re-derive the aim from
/// mouse events.
pub fn replay(segment: &Segment, airframe: &Airframe, mouse: bool) -> (Vec<Sample>, Divergence) {
    let mut state = segment.spawn.state.to_state();
    let mut instructor = Instructor::default();
    let mut aim = MouseAim::new(state.quat);
    let mut aim_live = false;
    let mut samples = Vec::with_capacity(segment.ticks.len());
    let mut divergence = Divergence {
        exact: true,
        ..default()
    };

    let mut fly = |rec: &TickRecord, aim: Option<Vec3>| {
        let mut input = rec.input.to_input();
        if let Some(aim) = aim {
            input.aim = aim;
        }
        let command = fly_tick(&mut state, airframe, &mut instructor, &input, rec.dt);
        let recorded = rec.state.to_state();
        let position = state.pos.distance(recorded.pos);
        let attitude = state.quat.angle_between(recorded.quat);
        divergence.exact &= crate::flight::recorder::StateRecord::from(&state) == rec.state;
        divergence.max_position = divergence.max_position.max(position);
        divergence.max_attitude = divergence.max_attitude.max(attitude);
        if divergence.first_tick.is_none() && (position > 0.01 || attitude > 0.01f32.to_radians()) {
            divergence.first_tick = Some(rec.tick);
        }
        samples.push(Sample {
            tick: rec.tick,
            t: rec.t,
            state,
            command,
            debug: instructor.debug,
        });
    };

    if !mouse {
        for rec in &segment.ticks {
            fly(rec, None);
        }
        return (samples, divergence);
    }

    // Ticks run before their frame's input is gathered: fly every tick of
    // frame N, then apply frame N's mouse motion to the aim.
    let mut ticks = segment.ticks.iter().peekable();
    for frame in &segment.frames {
        while let Some(rec) = ticks.next_if(|t| t.frame <= frame.frame) {
            fly(rec, aim_live.then(|| aim.dir()));
        }
        let delta: Vec2 =
            frame.mouse.iter().map(|&m| Vec2::from(m)).sum::<Vec2>() * AIM_SENSITIVITY;
        let delta = if frame.keys.contains('V') {
            Vec2::ZERO
        } else {
            delta
        };
        aim.turn(delta, frame.dt);
        aim_live = true;
    }
    for rec in ticks {
        fly(rec, aim_live.then(|| aim.dir()));
    }
    (samples, divergence)
}

// ── Metrics ─────────────────────────────────────────────────────────────────

/// Direction changes of `xs` larger than `threshold` (hysteresis), i.e. how
/// often a control is jerked back the other way.
pub fn reversals(xs: impl IntoIterator<Item = f32>, threshold: f32) -> usize {
    let mut xs = xs.into_iter();
    let Some(first) = xs.next() else {
        return 0;
    };
    let (mut extreme, mut direction, mut count) = (first, 0i8, 0);
    for x in xs {
        match direction {
            1 if x > extreme => extreme = x,
            1 if extreme - x > threshold => (direction, extreme, count) = (-1, x, count + 1),
            -1 if x < extreme => extreme = x,
            -1 if x - extreme > threshold => (direction, extreme, count) = (1, x, count + 1),
            0 if x - extreme > threshold => (direction, extreme) = (1, x),
            0 if extreme - x > threshold => (direction, extreme) = (-1, x),
            _ => {}
        }
    }
    count
}

fn rms(xs: impl IntoIterator<Item = f64>) -> f64 {
    let (sum, n) = xs
        .into_iter()
        .fold((0.0, 0usize), |(s, n), x| (s + x * x, n + 1));
    if n == 0 { 0.0 } else { (sum / n as f64).sqrt() }
}

fn max_abs(xs: impl IntoIterator<Item = f64>) -> f64 {
    xs.into_iter().fold(0.0, |m, x| m.max(x.abs()))
}

fn percentile(mut xs: Vec<f64>, p: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(f64::total_cmp);
    xs[((xs.len() - 1) as f64 * p).round() as usize]
}

/// Named flight metrics, in display order.
pub fn metrics(samples: &[Sample]) -> Vec<(&'static str, f64)> {
    if samples.len() < 2 {
        return Vec::new();
    }
    let deg = |r: f32| f64::from(r.to_degrees());
    let duration = (samples[samples.len() - 1].t - samples[0].t).max(1e-6);
    let per_s = |n: usize| n as f64 / duration;
    let rates = |f: fn(&Sample) -> f32| samples.iter().map(move |s| deg(f(s)));
    let accels = |f: fn(&Sample) -> f32| {
        samples
            .windows(2)
            .map(move |w| deg(f(&w[1]) - f(&w[0])) / (w[1].t - w[0].t).max(1e-6))
    };
    let command = |f: fn(&Surfaces) -> f32| samples.iter().map(move |s| f(&s.command));
    let travel = |f: fn(&Surfaces) -> f32| {
        samples
            .windows(2)
            .map(|w| f64::from((f(&w[1].command) - f(&w[0].command)).abs()))
            .sum::<f64>()
            / duration
    };
    let g = || samples.iter().map(|s| f64::from(s.state.g_load));
    let speed = || samples.iter().map(|s| f64::from(s.state.speed()) * 3.6);
    let errors: Vec<f64> = samples.iter().map(|s| deg(s.debug.err)).collect();

    vec![
        ("duration [s]", duration),
        (
            "aim error mean [deg]",
            errors.iter().sum::<f64>() / errors.len() as f64,
        ),
        ("aim error p95 [deg]", percentile(errors, 0.95)),
        ("roll rate rms [deg/s]", rms(rates(|s| s.state.omega.roll))),
        (
            "roll rate max [deg/s]",
            max_abs(rates(|s| s.state.omega.roll)),
        ),
        (
            "roll accel rms [deg/s2]",
            rms(accels(|s| s.state.omega.roll)),
        ),
        (
            "pitch rate rms [deg/s]",
            rms(rates(|s| s.state.omega.pitch)),
        ),
        (
            "pitch accel rms [deg/s2]",
            rms(accels(|s| s.state.omega.pitch)),
        ),
        ("yaw rate rms [deg/s]", rms(rates(|s| s.state.omega.yaw))),
        ("aileron travel [1/s]", travel(|c| c.aileron)),
        (
            "aileron reversals [1/s]",
            per_s(reversals(command(|c| c.aileron), 0.1)),
        ),
        (
            "aileron saturated [%]",
            100.0 * command(|c| c.aileron).filter(|a| a.abs() > 0.99).count() as f64
                / samples.len() as f64,
        ),
        ("elevator travel [1/s]", travel(|c| c.elevator)),
        (
            "elevator reversals [1/s]",
            per_s(reversals(command(|c| c.elevator), 0.1)),
        ),
        ("rudder travel [1/s]", travel(|c| c.rudder)),
        (
            "rudder reversals [1/s]",
            per_s(reversals(command(|c| c.rudder), 0.1)),
        ),
        ("load factor min [g]", g().fold(f64::MAX, f64::min)),
        ("load factor max [g]", g().fold(f64::MIN, f64::max)),
        (
            "load factor rate rms [g/s]",
            rms(samples.windows(2).map(|w| {
                f64::from(w[1].state.g_load - w[0].state.g_load) / (w[1].t - w[0].t).max(1e-6)
            })),
        ),
        ("sideslip rms [deg]", rms(rates(|s| s.state.beta))),
        ("sideslip max [deg]", max_abs(rates(|s| s.state.beta))),
        (
            "stalled [%]",
            100.0 * samples.iter().filter(|s| s.state.stalled).count() as f64
                / samples.len() as f64,
        ),
        ("speed min [km/h]", speed().fold(f64::MAX, f64::min)),
        (
            "speed mean [km/h]",
            speed().sum::<f64>() / samples.len() as f64,
        ),
        ("speed max [km/h]", speed().fold(f64::MIN, f64::max)),
    ]
}

fn metrics_table(out: &mut String, recorded: &[Sample], replayed: &[Sample]) {
    let (a, b) = (metrics(recorded), metrics(replayed));
    let _ = writeln!(
        out,
        "  {:<28} {:>11} {:>11} {:>8}",
        "", "recorded", "replayed", "change"
    );
    for ((name, x), (_, y)) in a.iter().zip(&b) {
        let change = if x.abs() > 1e-9 {
            format!("{:+.0}%", 100.0 * (y - x) / x.abs())
        } else if y.abs() > 1e-9 {
            "new".into()
        } else {
            String::new()
        };
        let change = if (y - x).abs() < 1e-9 {
            String::new()
        } else {
            change
        };
        let _ = writeln!(out, "  {name:<28} {x:>11.2} {y:>11.2} {change:>8}");
    }
}

// ── CLI ─────────────────────────────────────────────────────────────────────

/// Replay the log named in `args` (everything after `replay`) and print the
/// report.
pub fn cli(args: &[String]) -> Result<(), String> {
    let mut path = None;
    let mut mouse = false;
    let mut recorded_airframe = false;
    let mut csv = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mouse" => mouse = true,
            "--recorded-airframe" => recorded_airframe = true,
            "--csv" => csv = Some(args.next().ok_or(USAGE)?.clone()),
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            other if path.is_none() && !other.starts_with('-') => path = Some(other.to_string()),
            other => return Err(format!("unexpected argument {other:?}\n{USAGE}")),
        }
    }
    let path = path.ok_or(USAGE)?;
    let log = load(Path::new(&path))?;
    print!(
        "{}",
        report(
            &log,
            mouse,
            recorded_airframe,
            csv.as_deref().map(Path::new)
        )?
    );
    Ok(())
}

type Tuning = std::collections::BTreeMap<String, f32>;

fn to_tuning(pairs: Vec<(&'static str, f32)>) -> Tuning {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// `then -> now` lines for every constant that differs.
fn tuning_changes(then: &Tuning, now: &Tuning) -> Vec<String> {
    let mut changes = Vec::new();
    for (name, old) in then {
        match now.get(name) {
            Some(new) if new != old => changes.push(format!("  {name}: {old} -> {new}")),
            None => changes.push(format!("  {name}: {old} -> (gone)")),
            _ => {}
        }
    }
    for (name, new) in now {
        if !then.contains_key(name) {
            changes.push(format!("  {name}: (new) {new}"));
        }
    }
    changes
}

/// Names of airframe numbers (before format 3 they sat in the header's
/// tuning table next to the controller's).
fn is_airframe_constant(name: &str) -> bool {
    static NAMES: std::sync::LazyLock<std::collections::HashSet<&'static str>> =
        std::sync::LazyLock::new(|| {
            airframe_tuning(&aces_protocol::AIRCRAFT[0].airframe)
                .into_iter()
                .map(|(name, _)| name)
                .collect()
        });
    NAMES.contains(name)
}

/// The full replay report for `log`; writes the CSV if asked.
pub fn report(
    log: &Log,
    mouse: bool,
    recorded_airframe: bool,
    csv: Option<&Path>,
) -> Result<String, String> {
    let mut out = String::new();
    match &log.header {
        Some(h) => {
            let _ = writeln!(out, "flight log v{} recorded {} UTC", h.version, h.created);
            let then: Tuning = h
                .tuning
                .iter()
                .filter(|(name, _)| !is_airframe_constant(name))
                .map(|(k, v)| (k.clone(), *v))
                .collect();
            let changes = tuning_changes(&then, &to_tuning(crate::flight::tuning()));
            if changes.is_empty() {
                let _ = writeln!(out, "controller tuning: unchanged since the recording");
            } else {
                let _ = writeln!(out, "controller tuning changed since the recording:");
                for c in changes {
                    let _ = writeln!(out, "{c}");
                }
            }
        }
        None => {
            let _ = writeln!(out, "flight log without header (tuning unknown)");
        }
    }
    if log.skipped > 0 {
        let _ = writeln!(out, "skipped {} unreadable line(s)", log.skipped);
    }
    let _ = writeln!(
        out,
        "aim source: {}",
        if mouse {
            "re-derived from mouse events (--mouse)"
        } else {
            "recorded per tick"
        }
    );

    let mut csv_rows = String::new();
    for (i, segment) in log.segments.iter().enumerate() {
        let recorded: Vec<Sample> = segment.ticks.iter().map(Sample::recorded).collect();
        let (airframe, description) = airframe_for(segment, recorded_airframe);
        let (replayed, divergence) = replay(segment, &airframe, mouse);
        let mouse_counts: f64 = segment
            .frames
            .iter()
            .flat_map(|f| &f.mouse)
            .map(|m| f64::from(Vec2::from(*m).length()))
            .sum();
        let _ = writeln!(
            out,
            "\nsegment {} — {} ticks, {} frames, {:.0} mouse counts",
            i + 1,
            segment.ticks.len(),
            segment.frames.len(),
            mouse_counts
        );
        let _ = writeln!(out, "aircraft: {description}");
        // The airframe as it was flown: its record, or (older logs) its
        // numbers in the header's tuning table.
        let then: Tuning = match &segment.spawn.aircraft {
            Some(logged) => to_tuning(airframe_tuning(&logged.airframe)),
            None => log
                .header
                .iter()
                .flat_map(|h| &h.tuning)
                .filter(|(name, _)| is_airframe_constant(name))
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
        };
        let changes = tuning_changes(&then, &to_tuning(airframe_tuning(&airframe)));
        if changes.is_empty() {
            let _ = writeln!(out, "airframe: unchanged since the recording");
        } else {
            let _ = writeln!(out, "airframe changed since the recording:");
            for c in changes {
                let _ = writeln!(out, "{c}");
            }
        }
        if divergence.exact {
            let _ = writeln!(out, "replay: exact (bit-identical to the recording)");
        } else {
            let _ = writeln!(
                out,
                "replay: diverged — max {:.2} m / {:.2}°{}",
                divergence.max_position,
                divergence.max_attitude.to_degrees(),
                divergence
                    .first_tick
                    .map(|t| format!(", from tick {t}"))
                    .unwrap_or_default()
            );
        }
        metrics_table(&mut out, &recorded, &replayed);

        let (first, last) = match (recorded.first(), recorded.last()) {
            (Some(a), Some(b)) => (a.t, b.t),
            _ => continue,
        };
        for mark in log.marks.iter().filter(|m| (first..=last).contains(&m.t)) {
            let window = |s: &[Sample]| -> Vec<Sample> {
                s.iter()
                    .filter(|x| (mark.t - 2.0..=mark.t + 1.0).contains(&x.t))
                    .copied()
                    .collect()
            };
            let _ = writeln!(
                out,
                "\n  mark at t = {:.2} s (tick {}), window -2 s .. +1 s:",
                mark.t, mark.tick
            );
            metrics_table(&mut out, &window(&recorded), &window(&replayed));
        }

        if csv.is_some() {
            for (r, p) in recorded.iter().zip(&replayed) {
                csv_rows.push_str(&csv_row(i + 1, r, p));
            }
        }
    }

    if let Some(path) = csv {
        let mut file =
            std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        file.write_all(csv_header().as_bytes())
            .and_then(|()| file.write_all(csv_rows.as_bytes()))
            .map_err(|e| e.to_string())?;
        let _ = writeln!(out, "\nwrote {}", path.display());
    }
    Ok(out)
}

const CSV_FIELDS: [&str; 16] = [
    "err_deg",
    "bank_deg",
    "roll_rate",
    "pitch_rate",
    "yaw_rate",
    "alpha_deg",
    "beta_deg",
    "g",
    "speed",
    "elevator_cmd",
    "aileron_cmd",
    "rudder_cmd",
    "w",
    "x",
    "y",
    "z",
];

fn csv_header() -> String {
    let mut header = String::from("segment,tick,t");
    for side in ["rec", "rep"] {
        for field in CSV_FIELDS {
            let _ = write!(header, ",{side}_{field}");
        }
    }
    header.push('\n');
    header
}

fn csv_row(segment: usize, recorded: &Sample, replayed: &Sample) -> String {
    let mut row = format!("{segment},{},{:.4}", recorded.tick, recorded.t);
    for s in [recorded, replayed] {
        let st = &s.state;
        let values = [
            s.debug.err.to_degrees(),
            s.debug.bank.to_degrees(),
            st.omega.roll,
            st.omega.pitch,
            st.omega.yaw,
            st.alpha.to_degrees(),
            st.beta.to_degrees(),
            st.g_load,
            st.speed(),
            s.command.elevator,
            s.command.aileron,
            s.command.rudder,
            s.debug.w,
            st.pos.x,
            st.pos.y,
            st.pos.z,
        ];
        for v in values {
            let _ = write!(row, ",{v}");
        }
    }
    row.push('\n');
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reversals_count_direction_changes_past_the_threshold() {
        assert_eq!(reversals([0.0, 0.5, 1.0, 0.5, 0.0, 0.5], 0.1), 2);
        // Wiggles inside the threshold are not reversals.
        assert_eq!(reversals([0.0, 0.05, 0.0, 0.05, 0.0], 0.1), 0);
        assert_eq!(reversals([0.0, -1.0, 1.0, -1.0], 0.1), 2);
    }
}
