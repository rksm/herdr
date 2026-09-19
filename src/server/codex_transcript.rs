//! Capture the Codex transcript pager without changing ordinary terminal reads.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};

use crate::ghostty::ActiveScreen;
use crate::input::TerminalKey;
use crate::terminal::{snapshot_text, ScreenSnapshot, TerminalRuntime};

const QUIET: Duration = Duration::from_millis(10);
const STEP_TIMEOUT: Duration = Duration::from_secs(1);
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(40);
const RESTORE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Transcript {
    cols: u16,
    body: Vec<String>,
}

impl Transcript {
    fn from_screen(screen: ActiveScreen, snapshot: ScreenSnapshot) -> Option<Self> {
        if screen != ActiveScreen::Alternate {
            return None;
        }
        let text: Vec<_> = snapshot
            .rows
            .iter()
            .map(|row| {
                snapshot_text(std::slice::from_ref(row), 1, false, false)
                    .text
                    .trim_end()
                    .to_owned()
            })
            .collect();
        let lines: Vec<_> = text
            .iter()
            .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        let last = lines.iter().rposition(|line| !line.is_empty())?;
        if last < 6
            || !lines[0].starts_with("/ T R A N S C R I P T /")
            || lines[last] != "q close esc to edit prev"
            || lines[last - 1] != "↑/↓ to scroll pgup/pgdn to page home/end to jump"
            || !lines[last - 2].starts_with("───")
        {
            return None;
        }
        Some(Self {
            cols: snapshot.cols,
            body: text[1..last - 2].to_vec(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Bottom,
    VerifyBottom,
    LoadStart,
    CaptureEnd,
    Capture,
    VerifyTop,
    RestoreEnd,
    RestoreUp,
}

pub(super) struct Capture {
    initial: Transcript,
    previous: Transcript,
    history: Vec<String>,
    bottom: Option<Transcript>,
    phase: Phase,
    // Actual movement, not key counts: navigation clamps at either edge.
    initial_from_bottom: usize,
    restore_remaining: usize,
    sent_rows: usize,
    input_seq: u64,
    output_seq: u64,
    step_observed_output: bool,
    quiet_until: Instant,
    step_deadline: Instant,
    deadline: Instant,
    error: Option<String>,
}

impl Capture {
    pub(super) fn new(runtime: &TerminalRuntime, now: Instant) -> Option<Self> {
        let (screen, snapshot, seq) = runtime.screen_text_snapshot_with_seq()?;
        let initial = Transcript::from_screen(screen, snapshot)?;
        Some(Self {
            previous: initial.clone(),
            initial,
            history: Vec::new(),
            bottom: None,
            phase: Phase::Bottom,
            initial_from_bottom: 0,
            restore_remaining: 0,
            sent_rows: 0,
            input_seq: runtime.user_input_seq(),
            output_seq: seq,
            step_observed_output: false,
            quiet_until: now + QUIET,
            step_deadline: now + STEP_TIMEOUT,
            deadline: now + CAPTURE_TIMEOUT,
            error: None,
        })
    }

    pub(super) fn next_deadline(&self) -> Instant {
        self.quiet_until.min(self.deadline)
    }

    pub(super) fn poll(
        &mut self,
        runtime: &TerminalRuntime,
        now: Instant,
    ) -> Option<Result<String, String>> {
        // User navigation takes precedence. Never send restoration keys into a
        // view the user has changed or a prompt they have started editing.
        if runtime.user_input_seq() != self.input_seq {
            return Some(Err(
                "Transcript export interrupted by input; scroll position was not restored".into(),
            ));
        }
        let expired = now >= self.deadline;
        if expired && matches!(self.phase, Phase::RestoreEnd | Phase::RestoreUp) {
            return Some(Err(
                "Transcript export could not restore the scroll position".into(),
            ));
        }
        let seq = runtime.content_seq();
        if seq != self.output_seq {
            self.step_observed_output = true;
            self.output_seq = seq;
            self.quiet_until = now + QUIET;
        }
        if !expired && now < self.quiet_until {
            return None;
        }
        if runtime.synchronized_output_active() {
            self.quiet_until = now + QUIET;
            if !expired && now < self.step_deadline {
                return None;
            }
            return Some(Err(
                "Transcript did not finish drawing; scroll position was not restored".into(),
            ));
        }
        let Some((screen, snapshot, snapshot_seq)) = runtime.screen_text_snapshot_with_seq() else {
            if expired {
                return Some(Err(
                    "Transcript export timed out; scroll position was not restored".into(),
                ));
            }
            self.quiet_until = now + QUIET;
            return None;
        };
        if snapshot_seq != self.output_seq || runtime.content_seq() != snapshot_seq {
            if expired {
                return Some(Err("Transcript kept changing; export cancelled and scroll position was not restored".into()));
            }
            self.output_seq = snapshot_seq;
            self.step_observed_output = true;
            self.quiet_until = now + QUIET;
            return None;
        }
        if matches!(
            self.phase,
            Phase::VerifyBottom
                | Phase::LoadStart
                | Phase::CaptureEnd
                | Phase::VerifyTop
                | Phase::RestoreEnd
        ) && !self.step_observed_output
            && now < self.step_deadline
            && !expired
        {
            self.quiet_until = now + QUIET;
            return None;
        }
        let header = snapshot_text(
            &snapshot.rows[..snapshot.rows.len().min(1)],
            1,
            false,
            false,
        )
        .text;
        let Some(current) = Transcript::from_screen(screen, snapshot) else {
            return Some(Err(
                "Codex left the transcript view; export cancelled without sending more keys".into(),
            ));
        };
        if current.cols != self.initial.cols || current.body.len() != self.initial.body.len() {
            return Some(Err(
                "Transcript size changed; export cancelled without sending more keys".into(),
            ));
        }
        let batch = (current.body.len() / 2).max(1);
        if expired {
            self.error = Some("Transcript export timed out; no partial file was opened".into());
            // Observe the outstanding Down before reversing its movement.
            if self.phase == Phase::Bottom && current != self.previous {
                let Some(rows) =
                    scroll_distance(&self.previous.body, &current.body, self.sent_rows)
                else {
                    return Some(Err(
                        "Transcript export timed out; scroll position could not be restored".into(),
                    ));
                };
                self.initial_from_bottom += rows;
                self.previous = current;
            }
            return self.restore(runtime, now);
        }
        if self.sent_rows == 0 {
            self.previous = current;
            return self.send(runtime, KeyCode::Down, batch, now);
        }
        match self.phase {
            Phase::Bottom => {
                if current == self.previous {
                    if now < self.step_deadline {
                        self.quiet_until = self.step_deadline;
                        return None;
                    }
                    if periodic(&current.body, batch) {
                        return Some(Err("Repeated transcript rows hide the scroll position; export cancelled and position could not be restored".into()));
                    }
                    self.phase = Phase::VerifyBottom;
                    return self.send(runtime, KeyCode::End, 1, now);
                }
                match scroll_distance(&self.previous.body, &current.body, self.sent_rows) {
                    Some(rows) => self.initial_from_bottom += rows,
                    None => return Some(Err("Transcript scroll could not be measured; export cancelled, scroll position may have changed".into())),
                }
                self.previous = current;
                self.send(runtime, KeyCode::Down, batch, now)
            }
            Phase::VerifyBottom => {
                if current != self.previous {
                    return Some(Err("Transcript did not reach the expected bottom; export cancelled and position could not be restored".into()));
                }
                self.history = current.body.clone();
                self.bottom = Some(current.clone());
                self.previous = current;
                // Home asks recent Codex versions to hydrate all older pages.
                // Measure the original distance from the bottom before this jump.
                self.phase = Phase::LoadStart;
                self.send(runtime, KeyCode::Home, 1, now)
            }
            Phase::LoadStart => {
                if header.contains("history unavailable") {
                    self.error = Some(
                        "Codex could not load older history; no partial file was opened".into(),
                    );
                    return self.restore(runtime, now);
                }
                if header.contains("loading older history") || header.contains("partial history") {
                    self.quiet_until = now + QUIET;
                    return None;
                }
                self.phase = Phase::CaptureEnd;
                self.send(runtime, KeyCode::End, 1, now)
            }
            Phase::CaptureEnd => {
                if self.bottom.as_ref() != Some(&current) {
                    if now < self.step_deadline {
                        self.quiet_until = now + QUIET;
                        return None;
                    }
                    self.error = Some(
                        "Transcript changed while loading history; no partial file was opened"
                            .into(),
                    );
                    return self.restore(runtime, now);
                }
                self.previous = current;
                self.phase = Phase::Capture;
                self.send(runtime, KeyCode::Up, batch, now)
            }
            Phase::Capture => {
                if current == self.previous {
                    if now < self.step_deadline {
                        self.quiet_until = self.step_deadline;
                        return None;
                    }
                    if periodic(&current.body, batch) {
                        self.error = Some("Repeated transcript rows hide the scroll position; no partial file was opened".into());
                        return self.restore(runtime, now);
                    }
                    self.phase = Phase::VerifyTop;
                    return self.send(runtime, KeyCode::Home, 1, now);
                }
                match scroll_distance(&current.body, &self.previous.body, self.sent_rows) {
                    Some(rows) => self
                        .history
                        .splice(0..0, current.body[..rows].iter().cloned())
                        .for_each(drop),
                    None => {
                        self.error = Some("Transcript changed or scrolling was ambiguous; no partial file was opened".into());
                        return self.restore(runtime, now);
                    }
                }
                self.previous = current;
                self.send(runtime, KeyCode::Up, batch, now)
            }
            Phase::VerifyTop => {
                if current != self.previous {
                    self.error = Some(
                        "Transcript did not reach the expected top; no partial file was opened"
                            .into(),
                    );
                }
                self.restore(runtime, now)
            }
            Phase::RestoreEnd => {
                if self.bottom.as_ref() != Some(&current) {
                    if now < self.step_deadline {
                        self.quiet_until = now + QUIET;
                        return None;
                    }
                    return Some(Err("Transcript export could not return to the bottom; position was not restored".into()));
                }
                self.restore_remaining = self.initial_from_bottom;
                self.previous = current;
                self.phase = Phase::RestoreUp;
                self.restore_step(runtime, batch, now)
            }
            Phase::RestoreUp => {
                if current == self.previous && now < self.step_deadline {
                    self.quiet_until = self.step_deadline;
                    return None;
                }
                if scroll_distance(&current.body, &self.previous.body, self.sent_rows)
                    != Some(self.sent_rows)
                {
                    return Some(Err(
                        "Transcript export could not restore the scroll position".into(),
                    ));
                }
                self.restore_remaining = self.restore_remaining.saturating_sub(self.sent_rows);
                self.previous = current;
                self.restore_step(runtime, batch, now)
            }
        }
    }

    fn restore(
        &mut self,
        runtime: &TerminalRuntime,
        now: Instant,
    ) -> Option<Result<String, String>> {
        self.deadline = now + RESTORE_TIMEOUT;
        if matches!(self.phase, Phase::Bottom | Phase::VerifyBottom) {
            // We have not reached the bottom yet. Reverse only measured movement.
            self.restore_remaining = self.initial_from_bottom;
            self.phase = Phase::RestoreUp;
            return self.restore_step(runtime, (self.initial.body.len() / 2).max(1), now);
        }
        self.phase = Phase::RestoreEnd;
        self.send(runtime, KeyCode::End, 1, now)
    }

    fn restore_step(
        &mut self,
        runtime: &TerminalRuntime,
        batch: usize,
        now: Instant,
    ) -> Option<Result<String, String>> {
        if self.restore_remaining == 0 {
            if self.previous != self.initial {
                return Some(Err(
                    "Transcript export could not restore the original view".into()
                ));
            }
            return Some(match self.error.take() {
                Some(error) => Err(error),
                None => Ok(format!("{}\n", self.history.join("\n"))),
            });
        }
        self.send(runtime, KeyCode::Up, batch.min(self.restore_remaining), now)
    }

    fn send(
        &mut self,
        runtime: &TerminalRuntime,
        key: KeyCode,
        count: usize,
        now: Instant,
    ) -> Option<Result<String, String>> {
        let bytes = runtime
            .encode_terminal_key(TerminalKey::new(key, KeyModifiers::NONE))
            .repeat(count);
        self.output_seq = runtime.content_seq();
        self.step_observed_output = false;
        if runtime.try_send_terminal_control(bytes.into()).is_err() {
            return Some(Err(
                "Could not send transcript navigation; scroll position may have changed".into(),
            ));
        }
        self.sent_rows = count;
        self.quiet_until = now + QUIET;
        self.step_deadline = now + STEP_TIMEOUT;
        None
    }
}

fn periodic(rows: &[String], maximum: usize) -> bool {
    (1..=maximum.min(rows.len().saturating_sub(1)))
        .any(|shift| rows[shift..] == rows[..rows.len() - shift])
}

// A single unique overlap proves how far the pager moved, including edge clamps.
fn scroll_distance(older: &[String], newer: &[String], maximum: usize) -> Option<usize> {
    let mut matches = (1..=maximum.min(older.len().saturating_sub(1)))
        .filter(|&rows| older[rows..] == newer[..newer.len() - rows]);
    let rows = matches.next()?;
    matches.next().is_none().then_some(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY_ROWS: usize = 20;

    fn draw(position: usize, total: usize, enter: bool) -> Vec<u8> {
        let mut lines = vec!["/ T R A N S C R I P T / / /".to_owned()];
        lines.extend(
            (position..position + BODY_ROWS).map(|row| format!("transcript row {row:05} 日本語")),
        );
        lines.push(format!(
            "──────── {}% ─",
            100 * position / (total - BODY_ROWS).max(1)
        ));
        lines.push(" ↑/↓ to scroll   pgup/pgdn to page   home/end to jump".into());
        lines.push(" q close   esc to edit prev".into());
        format!(
            "{}\x1b[2J\x1b[H{}",
            if enter { "\x1b[?1049h" } else { "" },
            lines.join("\r\n")
        )
        .into_bytes()
    }

    fn navigate(runtime: &TerminalRuntime, bytes: &[u8], position: &mut usize, total: usize) {
        let keys = [KeyCode::Up, KeyCode::Down, KeyCode::End, KeyCode::Home];
        let mut rest = bytes;
        while !rest.is_empty() {
            let (key, encoded) = keys
                .iter()
                .find_map(|key| {
                    let encoded =
                        runtime.encode_terminal_key(TerminalKey::new(*key, KeyModifiers::NONE));
                    rest.starts_with(&encoded).then_some((*key, encoded))
                })
                .expect("only pager navigation keys");
            rest = &rest[encoded.len()..];
            *position = match key {
                KeyCode::Up => position.saturating_sub(1),
                KeyCode::Down => (*position + 1).min(total - BODY_ROWS),
                KeyCode::End => total - BODY_ROWS,
                KeyCode::Home => 0,
                _ => unreachable!(),
            };
        }
        runtime.test_process_pty_bytes(&draw(*position, total, false));
    }

    #[tokio::test]
    async fn codex_transcript_export_captures_over_1000_rows_and_restores_start_middle_and_end() {
        let total = 2037;
        for initial in [0, 13, 737, total - BODY_ROWS] {
            let (runtime, mut input) = TerminalRuntime::test_with_channel_and_scrollback_bytes(
                100,
                (BODY_ROWS + 4) as u16,
                10000,
                &draw(initial, total, true),
                16,
            );
            let mut now = Instant::now();
            let mut capture = Capture::new(&runtime, now).expect("transcript recognized");
            let mut position = initial;
            let text = loop {
                now = capture.next_deadline().max(now + QUIET);
                if let Some(result) = capture.poll(&runtime, now) {
                    break result.expect("complete export");
                }
                while let Ok(bytes) = input.try_recv() {
                    navigate(&runtime, &bytes, &mut position, total);
                }
            };
            assert_eq!(position, initial);
            let expected = (0..total)
                .map(|row| format!("transcript row {row:05} 日本語\n"))
                .collect::<String>();
            assert_eq!(text, expected, "initial={initial}");
        }
    }

    #[tokio::test]
    async fn codex_transcript_requires_alternate_screen_and_all_controls() {
        for bytes in [
            draw(0, 100, true),
            draw(0, 100, false),
            b"\x1b[?1049h/ T R A N S C R I P T /\r\nordinary output".to_vec(),
        ] {
            let runtime = TerminalRuntime::test_with_screen_bytes(100, 24, &bytes);
            let expected = bytes == draw(0, 100, true);
            assert_eq!(Capture::new(&runtime, Instant::now()).is_some(), expected);
        }
    }

    #[tokio::test]
    async fn codex_transcript_user_input_aborts_without_synthetic_keys() {
        let (runtime, mut input) = TerminalRuntime::test_with_channel_and_scrollback_bytes(
            100,
            24,
            10000,
            &draw(100, 200, true),
            16,
        );
        let now = Instant::now();
        let mut capture = Capture::new(&runtime, now).unwrap();
        runtime
            .try_send_bytes(bytes::Bytes::from_static(b"q"))
            .unwrap();
        assert_eq!(input.try_recv().unwrap().as_ref(), b"q");
        assert!(capture
            .poll(&runtime, now + QUIET)
            .unwrap()
            .unwrap_err()
            .contains("interrupted"));
        assert!(input.try_recv().is_err());
    }

    #[tokio::test]
    async fn codex_transcript_timeout_restores_before_reporting_incomplete() {
        let total = 333;
        let initial = 67;
        let (runtime, mut input) = TerminalRuntime::test_with_channel_and_scrollback_bytes(
            100,
            24,
            10000,
            &draw(initial, total, true),
            16,
        );
        let mut now = Instant::now();
        let mut capture = Capture::new(&runtime, now).unwrap();
        let mut position = initial;
        let mut expired = false;
        loop {
            now = capture.next_deadline().max(now + QUIET);
            if capture.phase == Phase::Capture && !expired {
                capture.deadline = now;
                expired = true;
            }
            if let Some(result) = capture.poll(&runtime, now) {
                assert!(result.unwrap_err().contains("timed out"));
                break;
            }
            while let Ok(bytes) = input.try_recv() {
                navigate(&runtime, &bytes, &mut position, total);
            }
        }
        assert_eq!(position, initial);
    }

    #[tokio::test]
    async fn codex_transcript_timeout_accounts_for_outstanding_down() {
        let initial = 67;
        let total = 333;
        let (runtime, mut input) = TerminalRuntime::test_with_channel_and_scrollback_bytes(
            100,
            24,
            10000,
            &draw(initial, total, true),
            16,
        );
        let started = Instant::now();
        let mut capture = Capture::new(&runtime, started).unwrap();
        assert!(capture.poll(&runtime, started + QUIET).is_none());
        let mut position = initial;
        navigate(&runtime, &input.try_recv().unwrap(), &mut position, total);
        assert_ne!(position, initial);
        let mut now = started + CAPTURE_TIMEOUT;
        for _ in 0..30 {
            if let Some(result) = capture.poll(&runtime, now) {
                assert!(result.unwrap_err().contains("timed out"));
                assert_eq!(position, initial);
                return;
            }
            while let Ok(bytes) = input.try_recv() {
                navigate(&runtime, &bytes, &mut position, total);
            }
            now += QUIET;
        }
        panic!("restoration did not finish");
    }

    #[tokio::test]
    async fn codex_transcript_repeated_rows_are_not_mistaken_for_eof() {
        let screen = String::from_utf8(draw(500, 2000, true)).unwrap();
        let mut lines: Vec<_> = screen.split("\r\n").map(str::to_owned).collect();
        for line in &mut lines[1..=BODY_ROWS] {
            *line = "repeated row".into();
        }
        let (runtime, mut input) = TerminalRuntime::test_with_channel_and_scrollback_bytes(
            100,
            24,
            10000,
            lines.join("\r\n").as_bytes(),
            16,
        );
        let now = Instant::now();
        let mut capture = Capture::new(&runtime, now).unwrap();
        assert!(capture.poll(&runtime, now + QUIET).is_none());
        input.try_recv().unwrap();
        let error = capture
            .poll(&runtime, now + STEP_TIMEOUT + QUIET)
            .unwrap()
            .unwrap_err();
        assert!(error.contains("Repeated"));
        assert!(error.contains("could not be restored"));
        assert!(input.try_recv().is_err());
    }

    #[tokio::test]
    async fn codex_transcript_view_exit_sends_no_more_keys() {
        for replacement in [
            b"\x1b[?1049l".as_slice(),
            b"\x1b[2J\x1b[Hdifferent view".as_slice(),
        ] {
            let (runtime, mut input) = TerminalRuntime::test_with_channel_and_scrollback_bytes(
                100,
                24,
                10000,
                &draw(0, 2000, true),
                16,
            );
            let now = Instant::now();
            let mut capture = Capture::new(&runtime, now).unwrap();
            runtime.test_process_pty_bytes(replacement);
            assert!(capture.poll(&runtime, now + QUIET).is_none());
            assert!(capture.poll(&runtime, now + QUIET * 2).unwrap().is_err());
            assert!(input.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn codex_transcript_waits_for_home_and_lazy_history_before_end() {
        let (runtime, mut input) = TerminalRuntime::test_with_channel_and_scrollback_bytes(
            100,
            24,
            10000,
            &draw(100, 200, true),
            16,
        );
        let now = Instant::now();
        let mut capture = Capture::new(&runtime, now).unwrap();
        capture.phase = Phase::LoadStart;
        capture.send(&runtime, KeyCode::Home, 1, now);
        input.try_recv().unwrap();
        assert!(capture.poll(&runtime, now + QUIET).is_none());
        assert!(input.try_recv().is_err(), "Home has not drawn yet");

        let partial = String::from_utf8(draw(0, 200, false))
            .unwrap()
            .replace("/ / /", "/ / / partial history | PgUp for earlier");
        runtime.test_process_pty_bytes(partial.as_bytes());
        assert!(capture.poll(&runtime, now + QUIET * 2).is_none());
        assert!(capture.poll(&runtime, now + QUIET * 3).is_none());
        assert!(input.try_recv().is_err(), "older history is not loaded yet");

        runtime.test_process_pty_bytes(&draw(0, 200, false));
        assert!(capture.poll(&runtime, now + QUIET * 4).is_none());
        assert!(capture.poll(&runtime, now + QUIET * 5).is_none());
        let end = runtime.encode_terminal_key(TerminalKey::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(input.try_recv().unwrap().as_ref(), end);
    }

    #[tokio::test]
    async fn codex_transcript_resize_sends_no_more_keys() {
        let (runtime, mut input) = TerminalRuntime::test_with_channel_and_scrollback_bytes(
            100,
            24,
            10000,
            &draw(0, 200, true),
            16,
        );
        let now = Instant::now();
        let mut capture = Capture::new(&runtime, now).unwrap();
        runtime.resize(24, 110, 0, 0);
        assert!(capture.poll(&runtime, now + QUIET).is_none());
        assert!(capture.poll(&runtime, now + QUIET * 2).unwrap().is_err());
        assert!(input.try_recv().is_err());
    }
}
