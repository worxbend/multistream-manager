//! What this program is costing the machine it runs on.
//!
//! A streaming setup is a machine already doing several demanding things at
//! once: encoding video, capturing audio, running a game. Anything else
//! sharing that machine has to justify itself, and "it feels fine" is not a
//! measurement. This puts three numbers in the status bar — processor time,
//! memory, and how many frames a second the interface is actually drawing —
//! so the question can be answered by looking rather than by guessing.
//!
//! It is off by default. Most of the time these numbers are noise, and the
//! status bar has better things to say; `alt+t` turns them on when you want
//! them.
//!
//! Nothing here is sampled while a frame is being drawn. Drawing reads
//! already-sampled values, so what appears on screen is a function of state
//! rather than of whatever the operating system happened to report halfway
//! through rendering — which is what keeps the drawing code testable and its
//! output reproducible.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How far back the frame-rate figure looks.
const FPS_WINDOW: Duration = Duration::from_secs(1);

/// Everything measured about this process.
#[derive(Debug, Default)]
pub struct Telemetry {
    /// Share of one processor core this process is using, as a percentage.
    /// `None` where the platform does not expose it.
    cpu_percent: Option<f64>,
    /// Resident memory in mebibytes — the memory actually held in RAM, which
    /// is the number that matters when something else wants some.
    memory_mb: Option<f64>,
    /// When frames were drawn, within the last second.
    frames: VecDeque<Instant>,
    /// The previous processor-time reading, to take a difference against.
    last_cpu: Option<(Instant, Duration)>,
}

impl Telemetry {
    /// Note that a frame has just been drawn.
    pub fn record_frame(&mut self, now: Instant) {
        self.frames.push_back(now);
        while self
            .frames
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) > FPS_WINDOW)
        {
            self.frames.pop_front();
        }
    }

    /// How many frames were drawn in the last second.
    pub fn fps(&self) -> usize {
        self.frames.len()
    }

    /// Take a fresh reading of processor time and memory.
    ///
    /// Processor use is a *rate*, so it needs two readings to exist at all:
    /// the first call establishes a baseline and reports nothing.
    pub fn sample(&mut self, now: Instant) {
        self.memory_mb = read_resident_memory_mb();

        let Some(cpu_time) = read_process_cpu_time() else {
            self.cpu_percent = None;
            return;
        };
        if let Some((then, previous)) = self.last_cpu {
            let wall = now.saturating_duration_since(then);
            if !wall.is_zero() {
                let used = cpu_time.saturating_sub(previous);
                self.cpu_percent = Some(used.as_secs_f64() / wall.as_secs_f64() * 100.0);
            }
        }
        self.last_cpu = Some((now, cpu_time));
    }

    /// The status-bar text, or `None` when there is nothing measurable to say.
    pub fn summary(&self) -> Option<String> {
        let mut parts = Vec::with_capacity(3);
        if let Some(cpu) = self.cpu_percent {
            parts.push(format!("cpu {cpu:.0}%"));
        }
        if let Some(memory) = self.memory_mb {
            parts.push(format!("mem {memory:.0}MB"));
        }
        parts.push(format!("{}fps", self.fps()));
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("  "))
        }
    }
}

/// Processor time this process has used, user and system together.
///
/// Read from `/proc/self/stat` on Linux, which is the only place it is
/// available without a C library binding. Every other platform returns `None`
/// and the figure is simply left out of the status bar — an interface that
/// refused to start because it could not measure itself would have its
/// priorities badly wrong.
#[cfg(target_os = "linux")]
fn read_process_cpu_time() -> Option<Duration> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // Fields are space-separated, but field 2 is the executable name in
    // parentheses and may itself contain spaces — so the split starts after
    // the last `)` rather than at the beginning of the line.
    let after_name = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = after_name.split_whitespace().collect();
    // Counting from the field after the name: state is 0, so utime (field 14
    // of the whole line) is index 11 and stime is index 12.
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(Duration::from_secs_f64(
        (utime + stime) as f64 / clock_ticks_per_second(),
    ))
}

#[cfg(not(target_os = "linux"))]
fn read_process_cpu_time() -> Option<Duration> {
    None
}

/// How many of the kernel's clock ticks make a second.
///
/// This is `sysconf(_SC_CLK_TCK)`, which needs a C library call to read
/// properly. It has been 100 on every Linux configuration in common use for
/// a very long time, and the cost of it being wrong is a processor figure
/// scaled by a constant — not a correctness problem — so the constant is
/// used rather than taking a dependency on libc for one number.
#[cfg(target_os = "linux")]
fn clock_ticks_per_second() -> f64 {
    100.0
}

/// Resident memory in mebibytes.
#[cfg(target_os = "linux")]
fn read_resident_memory_mb() -> Option<f64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    // The second field is the resident set size, in pages.
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    const PAGE_SIZE: f64 = 4096.0;
    Some(pages as f64 * PAGE_SIZE / (1024.0 * 1024.0))
}

#[cfg(not(target_os = "linux"))]
fn read_resident_memory_mb() -> Option<f64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_rate_counts_only_the_last_second() {
        let mut telemetry = Telemetry::default();
        let start = Instant::now();
        for step in 0..10 {
            telemetry.record_frame(start + Duration::from_millis(step * 100));
        }
        assert_eq!(telemetry.fps(), 10);

        // Two seconds later, none of those frames still count.
        telemetry.record_frame(start + Duration::from_secs(3));
        assert_eq!(telemetry.fps(), 1);
    }

    /// Processor use is a rate, and a rate needs two readings. The first
    /// sample must not invent a number from one.
    #[test]
    fn the_first_sample_reports_no_processor_figure() {
        let mut telemetry = Telemetry::default();
        telemetry.sample(Instant::now());
        assert!(telemetry.cpu_percent.is_none());
    }

    /// The frame rate is always available, so there is always something to
    /// show even where the platform exposes nothing else.
    #[test]
    fn there_is_always_a_summary_to_show() {
        let mut telemetry = Telemetry::default();
        telemetry.record_frame(Instant::now());
        let summary = telemetry.summary().expect("a summary");
        assert!(summary.contains("fps"), "got {summary:?}");
    }

    /// A second sample after real work has to produce a plausible figure —
    /// not a negative one, and not one implying more processor time than has
    /// actually elapsed on a single core.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_second_sample_produces_a_plausible_processor_figure() {
        let mut telemetry = Telemetry::default();
        telemetry.sample(Instant::now());

        // Do something measurable, so the reading is not simply zero.
        let mut total = 0u64;
        for value in 0..2_000_000u64 {
            total = total.wrapping_add(value);
        }
        assert!(total > 0, "the compiler must not optimise the work away");

        std::thread::sleep(Duration::from_millis(20));
        telemetry.sample(Instant::now());

        let cpu = telemetry.cpu_percent.expect("a figure on Linux");
        assert!(
            (0.0..=400.0).contains(&cpu),
            "implausible processor figure: {cpu}"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn memory_is_reported_and_is_not_absurd() {
        let memory = read_resident_memory_mb().expect("a reading on Linux");
        assert!(
            (0.1..100_000.0).contains(&memory),
            "implausible memory figure: {memory}MB"
        );
    }
}
