//! What an `update --apply` run shows while it works.
//!
//! On a terminal every stage is one line with a gauge and the overall percent,
//! redrawn in place while the stage runs; anywhere else (a pipe, a log) a
//! stage prints once, when it ends. Independently of that, a record file lets
//! the dashboard follow the run. The API restarts at the end of an update, so
//! its own memory cannot carry the progress.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use firecrab_api_types::{UpdatePhase, UpdateProgressResponse};

/// The stages of a run, in order.
const STAGES: [UpdatePhase; 5] = [
    UpdatePhase::Checking,
    UpdatePhase::Downloading,
    UpdatePhase::Verifying,
    UpdatePhase::Applying,
    UpdatePhase::Restarting,
];
const GAUGE_WIDTH: usize = 24;
/// A redraw per chunk would flood the terminal; ten a second is smooth enough.
const DRAW_EVERY: Duration = Duration::from_millis(100);
/// How often a download is written to the record file.
const RECORD_EVERY: Duration = Duration::from_millis(250);

/// Where `phase` begins and ends on the overall 0-100 scale. The download is
/// most of the wait, so it takes most of the scale.
fn span(phase: UpdatePhase) -> (u8, u8) {
    match phase {
        UpdatePhase::Checking => (0, 5),
        UpdatePhase::Downloading => (5, 80),
        UpdatePhase::Verifying => (80, 85),
        UpdatePhase::Applying => (85, 97),
        UpdatePhase::Restarting => (97, 100),
        UpdatePhase::Done => (100, 100),
        UpdatePhase::Idle | UpdatePhase::Failed => (0, 0),
    }
}

/// The overall percent while `phase` runs. Only the download is measured; every
/// other stage is at its start until it ends.
pub fn percent(phase: UpdatePhase, downloaded: u64, total: Option<u64>) -> u8 {
    let (start, end) = span(phase);
    match (phase, total) {
        (UpdatePhase::Downloading, Some(total)) if total > 0 => {
            let within =
                u128::from(end - start) * u128::from(downloaded.min(total)) / u128::from(total);
            start + u8::try_from(within).unwrap_or(end - start)
        }
        _ => start,
    }
}

fn stage_label(phase: UpdatePhase) -> &'static str {
    match phase {
        UpdatePhase::Checking => "Check",
        UpdatePhase::Downloading => "Download",
        UpdatePhase::Verifying => "Verify",
        UpdatePhase::Applying => "Apply",
        UpdatePhase::Restarting => "Restart",
        UpdatePhase::Idle | UpdatePhase::Done | UpdatePhase::Failed => "",
    }
}

fn stage_number(phase: UpdatePhase) -> usize {
    STAGES
        .iter()
        .position(|stage| *stage == phase)
        .map_or(0, |index| index + 1)
}

pub fn render_gauge(percent: u8, width: usize) -> String {
    let filled = (usize::from(percent.min(100)) * width + 50) / 100;
    format!("[{}{}]", "█".repeat(filled), "░".repeat(width - filled))
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The terminal half: where to write, and whether it can redraw a line.
pub struct Terminal {
    out: Box<dyn Write>,
    live: bool,
    /// When the stage that is running began, for the download rate.
    began: Instant,
    drawn: Option<Instant>,
}

impl Terminal {
    /// `live` is whether `out` is a terminal that understands `\r` and `ESC [ K`.
    pub fn new(out: Box<dyn Write>, live: bool) -> Self {
        Self {
            out,
            live,
            began: Instant::now(),
            drawn: None,
        }
    }

    fn line(phase: UpdatePhase, percent: u8, detail: &str) -> String {
        format!(
            "[{}/{}] {:<10}{} {percent:>3}%  {detail}",
            stage_number(phase),
            STAGES.len(),
            stage_label(phase),
            render_gauge(percent, GAUGE_WIDTH),
        )
    }

    /// Redraws the stage's line in place, or does nothing off a terminal.
    fn draw(&mut self, phase: UpdatePhase, percent: u8, detail: &str) {
        if !self.live {
            return;
        }
        let _ = write!(self.out, "\r{}\x1b[K", Self::line(phase, percent, detail));
        let _ = self.out.flush();
        self.drawn = Some(Instant::now());
    }

    /// Ends the stage's line, with the final text.
    fn finish(&mut self, phase: UpdatePhase, percent: u8, detail: &str) {
        let line = Self::line(phase, percent, detail);
        let _ = if self.live {
            writeln!(self.out, "\r{line}\x1b[K")
        } else {
            writeln!(self.out, "{line}")
        };
        let _ = self.out.flush();
        self.drawn = None;
    }

    fn download(&mut self, phase: UpdatePhase, percent: u8, bytes: u64, total: Option<u64>) {
        if self.drawn.is_some_and(|at| at.elapsed() < DRAW_EVERY) {
            return;
        }
        let seconds = self.began.elapsed().as_secs_f64().max(0.001);
        let rate = (bytes as f64 / seconds) as u64;
        let of = total.map_or_else(|| "?".to_owned(), format_bytes);
        let detail = format!("{}/{of}  {}/s", format_bytes(bytes), format_bytes(rate));
        self.draw(phase, percent, &detail);
    }
}

/// The record file the dashboard polls, written whole and renamed into place
/// so a reader never sees half a record.
struct Record {
    path: PathBuf,
    state: UpdateProgressResponse,
    written: Option<Instant>,
}

impl Record {
    fn write(&mut self, force: bool) {
        if !force && self.written.is_some_and(|at| at.elapsed() < RECORD_EVERY) {
            return;
        }
        self.state.updated_at_ms = now_ms();
        // The run is not over until the API that answers is the new one, and
        // only the API can tell: until then 100% would be a promise.
        let shown = UpdateProgressResponse {
            percent: if self.state.phase == UpdatePhase::Done {
                100
            } else {
                self.state.percent.min(99)
            },
            ..self.state.clone()
        };
        let Ok(json) = serde_json::to_vec(&shown) else {
            return;
        };
        let temporary = self.path.with_extension("json.tmp");
        let written = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .open(&temporary)
            .and_then(|mut file| file.write_all(&json))
            .and_then(|()| fs::rename(&temporary, &self.path));
        if written.is_err() {
            // Progress is a courtesy: a record that cannot be written must
            // never fail the update it describes.
            let _ = fs::remove_file(&temporary);
            return;
        }
        self.written = Some(Instant::now());
    }
}

/// What a run shows and records as it goes.
pub struct Progress {
    terminal: Option<Terminal>,
    record: Option<Record>,
    /// The stage that is running.
    stage: Option<UpdatePhase>,
}

impl Progress {
    /// A run that shows and records nothing.
    #[cfg(test)]
    pub fn silent() -> Self {
        Self::new(None, None)
    }

    pub fn new(terminal: Option<Terminal>, record: Option<PathBuf>) -> Self {
        Self {
            terminal,
            record: record.map(|path| Record {
                path,
                state: UpdateProgressResponse::idle(),
                written: None,
            }),
            stage: None,
        }
    }

    /// The run begins, installing `target` (without a leading `v`).
    pub fn begin(&mut self, target: Option<&str>) {
        if let Some(record) = &mut self.record {
            record.state = UpdateProgressResponse {
                phase: UpdatePhase::Checking,
                target: target.map(str::to_owned),
                pid: Some(std::process::id()),
                ..UpdateProgressResponse::idle()
            };
            record.write(true);
        }
    }

    /// A stage starts; `detail` says what it is doing.
    pub fn stage(&mut self, phase: UpdatePhase, detail: &str) {
        self.stage = Some(phase);
        let percent = percent(phase, 0, None);
        if let Some(terminal) = &mut self.terminal {
            terminal.began = Instant::now();
            terminal.draw(phase, percent, detail);
        }
        self.record_stage(phase, percent, None, None);
    }

    /// The download of the running stage has `bytes` of `total`.
    pub fn downloaded(&mut self, bytes: u64, total: Option<u64>) {
        let Some(phase) = self.stage else {
            return;
        };
        let percent = percent(phase, bytes, total);
        if let Some(terminal) = &mut self.terminal {
            terminal.download(phase, percent, bytes, total);
        }
        if let Some(record) = &mut self.record {
            record.state.percent = percent;
            record.state.downloaded_bytes = Some(bytes);
            record.state.total_bytes = total;
            record.write(false);
        }
    }

    /// The running stage ends; `detail` is what it leaves on its line.
    pub fn stage_done(&mut self, detail: &str) {
        let Some(phase) = self.stage.take() else {
            return;
        };
        let percent = span(phase).1;
        if let Some(terminal) = &mut self.terminal {
            terminal.finish(phase, percent, detail);
        }
        if let Some(record) = &mut self.record {
            record.state.percent = percent;
            record.write(true);
        }
    }

    /// The run stops with `error`. A stage in the middle of its line is ended.
    pub fn failed(&mut self, error: &str) {
        let running = self.stage.take();
        let reached = self
            .record
            .as_ref()
            .map_or(0, |record| record.state.percent);
        if let (Some(terminal), Some(phase)) = (&mut self.terminal, running) {
            terminal.finish(phase, reached.max(span(phase).0), "failed");
        }
        if let Some(record) = &mut self.record {
            record.state.phase = UpdatePhase::Failed;
            record.state.error = Some(error.to_owned());
            record.write(true);
        }
    }

    /// There was nothing to install: the host already runs the newest release.
    pub fn up_to_date(&mut self) {
        if let Some(record) = &mut self.record {
            record.state.phase = UpdatePhase::Done;
            record.state.percent = 100;
            record.write(true);
        }
    }

    fn record_stage(
        &mut self,
        phase: UpdatePhase,
        percent: u8,
        downloaded: Option<u64>,
        total: Option<u64>,
    ) {
        if let Some(record) = &mut self.record {
            record.state.phase = phase;
            record.state.percent = percent;
            record.state.downloaded_bytes = downloaded;
            record.state.total_bytes = total;
            record.write(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// A terminal whose output the test can read.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Capture {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }

        fn terminal(&self, live: bool) -> Terminal {
            Terminal::new(Box::new(self.clone()), live)
        }
    }

    fn read_record(path: &std::path::Path) -> UpdateProgressResponse {
        serde_json::from_slice(&fs::read(path).expect("record exists")).expect("record parses")
    }

    #[test]
    fn percent_follows_the_stage_spans() {
        use UpdatePhase::*;
        assert_eq!(percent(Checking, 0, None), 0);
        assert_eq!(percent(Downloading, 0, Some(100)), 5);
        assert_eq!(percent(Downloading, 50, Some(100)), 42);
        assert_eq!(percent(Downloading, 100, Some(100)), 80);
        assert_eq!(percent(Verifying, 0, None), 80);
        assert_eq!(percent(Applying, 0, None), 85);
        assert_eq!(percent(Restarting, 0, None), 97);
        assert_eq!(percent(Done, 0, None), 100);
        // Every stage ends where the next one begins.
        let ends: Vec<u8> = STAGES.iter().map(|stage| span(*stage).1).collect();
        let starts: Vec<u8> = STAGES.iter().skip(1).map(|stage| span(*stage).0).collect();
        assert_eq!(ends[..ends.len() - 1], starts[..]);
    }

    #[test]
    fn a_download_of_unknown_or_overrun_size_stays_in_its_span() {
        use UpdatePhase::Downloading;
        assert_eq!(percent(Downloading, 5_000, None), 5);
        assert_eq!(percent(Downloading, 5_000, Some(0)), 5);
        assert_eq!(percent(Downloading, 9_999, Some(100)), 80);
        assert_eq!(percent(Downloading, u64::MAX / 2, Some(u64::MAX / 2)), 80);
    }

    #[test]
    fn the_gauge_fills_in_proportion() {
        assert_eq!(render_gauge(0, 4), "[░░░░]");
        assert_eq!(render_gauge(50, 4), "[██░░]");
        assert_eq!(render_gauge(100, 4), "[████]");
        assert_eq!(render_gauge(200, 4), "[████]");
        // Rounded to the nearest cell rather than cut.
        assert_eq!(render_gauge(3, 24), format!("[█{}]", "░".repeat(23)));
        assert_eq!(render_gauge(2, 24), format!("[{}]", "░".repeat(24)));
    }

    #[test]
    fn bytes_are_binary_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(11_178_942), "10.7 MiB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }

    #[test]
    fn a_terminal_redraws_a_stage_in_place_and_ends_it_with_a_newline() {
        let capture = Capture::default();
        let mut progress = Progress::new(Some(capture.terminal(true)), None);
        progress.stage(UpdatePhase::Downloading, "firecrab-host-x86_64-gnu.tar.gz");
        progress.downloaded(50, Some(100));
        // The first chunk is drawn at once; a second one right after is skipped.
        progress.stage_done("10.7 MiB");
        let text = capture.text();

        assert!(text.starts_with("\r[2/5] Download "), "{text:?}");
        assert!(text.contains("firecrab-host-x86_64-gnu.tar.gz"), "{text:?}");
        assert!(text.contains("\x1b[K"), "{text:?}");
        let last = text.rsplit('\r').next().unwrap();
        assert!(last.contains(&render_gauge(80, GAUGE_WIDTH)), "{last:?}");
        assert!(last.contains(" 80%  10.7 MiB"), "{last:?}");
        assert!(last.ends_with("\x1b[K\n"), "{last:?}");
        assert_eq!(text.matches('\n').count(), 1, "one line per stage");
    }

    #[test]
    fn a_pipe_gets_one_plain_line_per_finished_stage() {
        let capture = Capture::default();
        let mut progress = Progress::new(Some(capture.terminal(false)), None);
        progress.stage(UpdatePhase::Checking, "looking");
        progress.stage_done("v0.3.0 → v0.3.1");
        progress.stage(UpdatePhase::Downloading, "bundle");
        for done in (0..=100).step_by(10) {
            progress.downloaded(done, Some(100));
        }
        progress.stage_done("10.7 MiB");
        let text = capture.text();

        assert!(!text.contains('\r') && !text.contains('\x1b'), "{text:?}");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text:?}");
        assert!(lines[0].starts_with("[1/5] Check     "), "{text:?}");
        assert!(lines[0].ends_with("  5%  v0.3.0 → v0.3.1"), "{text:?}");
        assert!(lines[1].starts_with("[2/5] Download  "), "{text:?}");
        assert!(lines[1].ends_with(" 80%  10.7 MiB"), "{text:?}");
    }

    #[test]
    fn redraws_during_a_download_are_throttled() {
        let capture = Capture::default();
        let mut progress = Progress::new(Some(capture.terminal(true)), None);
        progress.stage(UpdatePhase::Downloading, "bundle");
        for done in 0..200 {
            progress.downloaded(done, Some(200));
        }
        let redraws = capture.text().matches('\r').count();
        assert!(redraws < 10, "{redraws} redraws for 200 chunks");
    }

    #[test]
    fn a_failure_ends_the_line_that_was_open() {
        let capture = Capture::default();
        let mut progress = Progress::new(Some(capture.terminal(true)), None);
        progress.stage(
            UpdatePhase::Applying,
            "handing the bundle to firecrab-helper",
        );
        progress.failed("network helper rejected the update");
        let text = capture.text();
        assert!(text.ends_with("  85%  failed\x1b[K\n"), "{text:?}");
        // Nothing is open any more, so a later failure prints nothing more.
        progress.failed("again");
        assert_eq!(capture.text(), text);
    }

    #[test]
    fn the_record_follows_the_run_and_names_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("progress.json");
        let mut progress = Progress::new(None, Some(path.clone()));

        progress.begin(Some("0.3.1"));
        let record = read_record(&path);
        assert_eq!(record.phase, UpdatePhase::Checking);
        assert_eq!(record.percent, 0);
        assert_eq!(record.target.as_deref(), Some("0.3.1"));
        assert_eq!(record.pid, Some(std::process::id()));
        assert!(record.updated_at_ms > 0);

        progress.stage(UpdatePhase::Checking, "");
        progress.stage_done("");
        progress.stage(UpdatePhase::Downloading, "bundle");
        assert_eq!(read_record(&path).percent, 5);
        progress.downloaded(50, Some(100));
        progress.stage_done("done");
        let record = read_record(&path);
        assert_eq!(record.phase, UpdatePhase::Downloading);
        assert_eq!(record.percent, 80);

        progress.stage(UpdatePhase::Applying, "helper");
        assert_eq!(read_record(&path).phase, UpdatePhase::Applying);
        assert_eq!(read_record(&path).downloaded_bytes, None);
        assert!(
            !directory.path().join("progress.json.tmp").exists(),
            "the temporary file is renamed away"
        );
    }

    #[test]
    fn the_record_never_claims_to_be_finished_before_the_api_says_so() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("progress.json");
        let mut progress = Progress::new(None, Some(path.clone()));
        progress.begin(Some("0.3.1"));
        progress.stage(UpdatePhase::Restarting, "services");
        progress.stage_done("restarting now");
        let record = read_record(&path);
        assert_eq!(record.phase, UpdatePhase::Restarting);
        assert_eq!(record.percent, 99);

        progress.up_to_date();
        let record = read_record(&path);
        assert_eq!(record.phase, UpdatePhase::Done);
        assert_eq!(record.percent, 100);
    }

    #[test]
    fn a_failed_run_keeps_where_it_got_to_and_why_it_stopped() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("progress.json");
        let mut progress = Progress::new(None, Some(path.clone()));
        progress.begin(Some("0.3.1"));
        progress.stage(UpdatePhase::Downloading, "bundle");
        progress.downloaded(50, Some(100));
        progress.failed("failed to download https://example.invalid/b: HTTP 404");
        let record = read_record(&path);
        assert_eq!(record.phase, UpdatePhase::Failed);
        assert_eq!(record.percent, 42);
        assert_eq!(
            record.error.as_deref(),
            Some("failed to download https://example.invalid/b: HTTP 404")
        );
        assert_eq!(record.target.as_deref(), Some("0.3.1"));
    }

    #[test]
    fn download_writes_to_the_record_are_throttled_and_the_end_is_not_lost() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("progress.json");
        let mut progress = Progress::new(None, Some(path.clone()));
        progress.begin(Some("0.3.1"));
        progress.stage(UpdatePhase::Downloading, "bundle");
        let started = read_record(&path).updated_at_ms;
        for done in 1..=200 {
            progress.downloaded(done, Some(200));
        }
        // Within the throttle window the record still says where the stage began.
        assert_eq!(read_record(&path).downloaded_bytes, None);
        assert!(read_record(&path).updated_at_ms >= started);
        progress.stage_done("done");
        assert_eq!(read_record(&path).percent, 80);
        assert_eq!(read_record(&path).downloaded_bytes, Some(200));
    }

    #[test]
    fn an_unwritable_record_does_not_stop_the_run() {
        let mut progress = Progress::new(
            None,
            Some("/proc/firecrab-no-such-dir/progress.json".into()),
        );
        progress.begin(Some("0.3.1"));
        progress.stage(UpdatePhase::Checking, "");
        progress.stage_done("");
        progress.failed("whatever");
    }

    #[test]
    fn a_silent_run_does_nothing() {
        let mut progress = Progress::silent();
        progress.begin(None);
        progress.stage(UpdatePhase::Downloading, "x");
        progress.downloaded(1, None);
        progress.stage_done("y");
        progress.up_to_date();
        progress.failed("z");
    }
}
