//! The progress display: a bar for a download, a spinner for work that has no
//! size to show.
//!
//! The bar has the shape rustup uses -
//! `[========>-------] 19.09 MiB/52.30 MiB 1.01 MiB/s` - so the filled part says
//! where the transfer is and the two sizes say exactly how much has arrived and
//! how much there is. Nothing is drawn until bytes actually arrive: a line that
//! only says `0 B 0 B/s` is noise, and on a slow start it is noise for seconds.
//! A transfer too small to watch (the download index) is never drawn at all.
//!
//! Drawing is left to `indicatif`, which redraws the line in place and stays
//! silent when stderr is not a terminal, so a redirected `fzv get` keeps a
//! readable log. The terminal cursor is left alone: showing and hiding it around
//! a redraw looks worse than letting it sit at the end of the line.

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

/// Transfers smaller than this are not worth a bar: they are over before it
/// could be read.
const BAR_THRESHOLD: u64 = 1024 * 1024;

/// Label, bar, arrived/total, throughput.
const BAR_TEMPLATE: &str = "{msg} [{bar:16}] {bytes}/{total_bytes} {bytes_per_sec}";
/// The same before the length is known: no bar, and no invented percentage.
const UNKNOWN_TEMPLATE: &str = "{msg} {bytes} {bytes_per_sec}";
/// A spinner for work with no size to show at all.
const SPINNER_TEMPLATE: &str = "{spinner} {msg} {elapsed_precise}";

/// A spinner for work whose end is not known in advance (probing mirrors,
/// unpacking an archive).
///
/// It leaves the screen when it goes out of scope, so an error path cannot leave
/// a half-drawn line behind.
pub(crate) struct Spinner {
    bar: ProgressBar,
}

impl Spinner {
    pub(crate) fn start(label: &str) -> Spinner {
        let bar = ProgressBar::new_spinner();
        bar.set_style(
            ProgressStyle::with_template(SPINNER_TEMPLATE)
                .unwrap_or_else(|_| ProgressStyle::default_spinner())
                .tick_chars("|/-\\"),
        );
        bar.set_message(label.to_string());
        bar.enable_steady_tick(Duration::from_millis(120));
        Spinner { bar }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.bar.finish_and_clear();
    }
}

/// The progress of one download, shared by every connection feeding it.
///
/// The line is created hidden and starts drawing with the first byte that
/// arrives; [`Progress::silent`] never draws, which is what the internal fetches
/// (the download index) use.
pub(crate) struct Progress {
    bar: ProgressBar,
    /// Where to draw once there is something to show. A function, because a draw
    /// target cannot be cloned and the tests point this at a buffer.
    sink: Box<dyn Fn() -> ProgressDrawTarget + Send + Sync>,
    started: AtomicBool,
    total: AtomicU64,
}

impl Progress {
    /// A download of `total` bytes, labelled `label`.
    pub(crate) fn new(total: u64, label: &str) -> Self {
        if total < BAR_THRESHOLD {
            return Progress::silent();
        }
        Progress::build(Some(total), label, Box::new(ProgressDrawTarget::stderr))
    }

    /// A download whose size the server has not reported yet.
    pub(crate) fn unknown(label: &str) -> Self {
        Progress::build(None, label, Box::new(ProgressDrawTarget::stderr))
    }

    /// A download that should not be drawn at all.
    pub(crate) fn silent() -> Self {
        Progress::build(None, "", hidden_sink())
    }

    fn build(
        total: Option<u64>,
        label: &str,
        sink: Box<dyn Fn() -> ProgressDrawTarget + Send + Sync>,
    ) -> Self {
        // A line that names nothing says nothing: whatever a caller passes, an
        // unlabelled transfer is silent rather than a floating `0 B 0 B/s`.
        let sink = if label.is_empty() {
            hidden_sink()
        } else {
            sink
        };
        // Hidden from the moment it exists. `ProgressBar::new` draws to stderr,
        // so setting a style or a message on it would paint a frame at position
        // zero - `ZLS 0 B 0 B/s` - before a single byte has arrived.
        let bar = ProgressBar::with_draw_target(total, ProgressDrawTarget::hidden());
        bar.set_style(match total {
            Some(_) => bar_style(),
            None => unknown_style(),
        });
        bar.set_message(label.to_string());
        Progress {
            bar,
            sink,
            started: AtomicBool::new(false),
            total: AtomicU64::new(total.unwrap_or(0)),
        }
    }

    /// Starts drawing, once, when there is something to show.
    fn start(&self) {
        if self.started.swap(true, Ordering::Relaxed) {
            return;
        }
        self.bar.set_draw_target((self.sink)());
    }

    /// The expected length, or zero while it is still unknown.
    pub(crate) fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.bar.position()
    }

    /// Records the length once a response reveals it, which turns the line into
    /// a proper bar. Ignored when the length was already known, or when the
    /// transfer turns out to be too small to watch.
    pub(crate) fn measure(&self, total: u64) {
        if total == 0 || self.total() > 0 {
            return;
        }
        self.total.store(total, Ordering::Relaxed);
        if total < BAR_THRESHOLD {
            // Too small to be worth a line after all.
            self.bar.set_draw_target(ProgressDrawTarget::hidden());
            return;
        }
        self.bar.set_length(total);
        self.bar.set_style(bar_style());
    }

    /// Counts bytes that have arrived.
    pub(crate) fn add(&self, count: u64) {
        if count == 0 {
            return;
        }
        self.bar.inc(count);
        self.start();
    }

    /// Counts bytes that are already on disk. A resumed transfer starts where it
    /// left off; it does not open with a burst of imaginary throughput.
    pub(crate) fn seed(&self, bytes: u64) {
        if bytes > 0 {
            self.start();
        }
        self.bar.set_position(bytes);
    }

    /// Takes back bytes counted for a range that has to be fetched again.
    pub(crate) fn rollback(&self, count: u64) {
        self.bar.dec(count);
    }

    /// Forgets everything counted so far, for a transfer that starts over.
    pub(crate) fn reset(&self) {
        self.bar.reset();
    }

    /// Takes the line off the screen: what happens next is what matters.
    pub(crate) fn finish(&self) {
        self.bar.finish_and_clear();
    }

    /// Runs `print` without the progress line in the way, redrawing it after.
    ///
    /// Printing while a bar is live glues the message onto the bar's line; a
    /// message about the transfer has to be readable on its own.
    pub(crate) fn suspend<R>(&self, print: impl FnOnce() -> R) -> R {
        self.bar.suspend(print)
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.finish();
    }
}

fn bar_style() -> ProgressStyle {
    ProgressStyle::with_template(BAR_TEMPLATE)
        .unwrap_or_else(|_| ProgressStyle::default_bar())
        .progress_chars("=>-")
}

fn unknown_style() -> ProgressStyle {
    ProgressStyle::with_template(UNKNOWN_TEMPLATE).unwrap_or_else(|_| ProgressStyle::default_bar())
}

/// A sink that draws nowhere.
fn hidden_sink() -> Box<dyn Fn() -> ProgressDrawTarget + Send + Sync> {
    Box::new(ProgressDrawTarget::hidden)
}

#[cfg(test)]
mod tests {
    use super::{BAR_TEMPLATE, Progress, SPINNER_TEMPLATE, Spinner, UNKNOWN_TEMPLATE};
    use indicatif::{InMemoryTerm, ProgressDrawTarget, ProgressStyle};

    /// A terminal that keeps what was drawn, so the display can be asserted.
    fn terminal() -> InMemoryTerm {
        InMemoryTerm::new(12, 120)
    }

    /// Draws the way a real download does, but into `term`.
    fn visible(total: Option<u64>, label: &str, term: &InMemoryTerm) -> Progress {
        let term = term.clone();
        Progress::build(
            total,
            label,
            Box::new(move || ProgressDrawTarget::term_like(Box::new(term.clone()))),
        )
    }

    /// A typo in a template would silently fall back to a default style, so the
    /// templates are checked on their own.
    #[test]
    fn the_templates_are_valid() {
        for template in [BAR_TEMPLATE, UNKNOWN_TEMPLATE, SPINNER_TEMPLATE] {
            assert!(
                ProgressStyle::with_template(template).is_ok(),
                "invalid template: {template}"
            );
        }
    }

    #[test]
    fn counts_bytes_in_both_directions() {
        let progress = Progress::new(8 * 1024 * 1024, "1.2.3");
        assert_eq!(progress.total(), 8 * 1024 * 1024);
        assert_eq!(progress.bytes(), 0);
        progress.add(1024);
        progress.add(2048);
        assert_eq!(progress.bytes(), 3072);
        progress.rollback(1024);
        assert_eq!(progress.bytes(), 2048);
        progress.seed(4 * 1024 * 1024);
        assert_eq!(progress.bytes(), 4 * 1024 * 1024);
        progress.reset();
        assert_eq!(progress.bytes(), 0);
        progress.finish();
    }

    #[test]
    fn an_unknown_transfer_counts_and_can_be_measured_later() {
        let progress = Progress::unknown("ZLS");
        assert_eq!(progress.total(), 0);
        progress.add(4096);
        assert_eq!(progress.bytes(), 4096);
        progress.measure(2 * 1024 * 1024);
        assert_eq!(progress.total(), 2 * 1024 * 1024);
        // A length that arrives twice, or a zero, changes nothing.
        progress.measure(3 * 1024 * 1024);
        assert_eq!(progress.total(), 2 * 1024 * 1024);
        progress.measure(0);
        assert_eq!(progress.total(), 2 * 1024 * 1024);
        progress.finish();
    }

    #[test]
    fn a_spinner_can_come_and_go() {
        let spinner = Spinner::start("unpacking");
        drop(spinner);
    }

    /// The mechanism behind the reported `0 B 0 B/s`: a bar whose draw target is
    /// live paints itself as soon as a style or a message is set - and
    /// `ProgressBar::new` draws to stderr, a terminal in a real run. This is why
    /// the bar has to be created hidden.
    #[test]
    fn a_live_bar_paints_itself_at_zero_which_is_why_it_is_born_hidden() {
        let term = terminal();
        let bar = indicatif::ProgressBar::with_draw_target(
            Some(0),
            ProgressDrawTarget::term_like(Box::new(term.clone())),
        );
        bar.set_style(super::unknown_style());
        bar.set_message("ZLS".to_string());
        assert_eq!(
            term.contents().trim(),
            "ZLS 0 B 0 B/s",
            "the reported line was not reproduced"
        );
        bar.finish_and_clear();

        // Through `Progress`, the same transfer paints nothing until bytes
        // arrive - the same style and message are set, but on a hidden bar.
        let term = terminal();
        let progress = visible(None, "ZLS", &term);
        assert_eq!(
            term.contents().trim(),
            "",
            "something was drawn at creation"
        );
        progress.draw_now();
        assert_eq!(
            term.contents().trim(),
            "",
            "something was drawn at creation"
        );

        // The first frame that is ever painted has bytes in it.
        progress.add(1024);
        progress.draw_now();
        let drawn = term.contents();
        assert!(
            drawn.contains("1.00 KiB") && !drawn.contains("0 B "),
            "the first frame is empty: {drawn:?}"
        );
        progress.finish();
    }

    /// An empty line - `0 B 0 B/s` - is exactly what a download should not show
    /// while it waits for its first byte.
    #[test]
    fn nothing_is_drawn_before_the_first_byte() {
        let term = terminal();
        let progress = visible(Some(8 * 1024 * 1024), "Zig 0.1.0", &term);
        progress.draw_now();
        assert_eq!(term.contents().trim(), "", "an empty line was drawn");

        progress.add(0);
        progress.draw_now();
        assert_eq!(term.contents().trim(), "", "an empty line was drawn");

        progress.add(1024 * 1024);
        progress.draw_now();
        assert!(
            term.contents().contains('['),
            "the line did not appear: {:?}",
            term.contents()
        );
        progress.finish();
    }

    /// The shape the user reads: a bar, then arrived/total, then the rate.
    #[test]
    fn the_bar_shows_how_much_of_how_much() {
        let term = terminal();
        let progress = visible(Some(8 * 1024 * 1024), "Zig 0.1.0", &term);
        progress.add(4 * 1024 * 1024);
        progress.draw_now();
        let drawn = term.contents();
        assert!(
            drawn.contains("[========>-------]"),
            "the bar is not '===>' shaped: {drawn:?}"
        );
        assert!(
            drawn.contains("4.00 MiB/8.00 MiB"),
            "arrived/total is missing: {drawn:?}"
        );
        assert!(drawn.starts_with("Zig 0.1.0 "), "{drawn:?}");
        progress.finish();
    }

    /// A server that hid the size stays barless until the response names it;
    /// from then on it is an ordinary bar. This is what a mirror that does not
    /// answer the HEAD looks like.
    #[test]
    fn an_unknown_length_becomes_a_bar_once_it_is_learned() {
        let term = terminal();
        let progress = visible(None, "Zig 0.1.0", &term);
        progress.add(1024 * 1024);
        progress.draw_now();
        let drawn = term.contents();
        assert!(!drawn.contains('['), "a bar appeared too early: {drawn:?}");
        assert!(!drawn.contains('%'), "a percentage was invented: {drawn:?}");
        assert!(
            !drawn.contains("eta"),
            "an estimate was invented: {drawn:?}"
        );
        assert!(drawn.contains("1.00 MiB"), "{drawn:?}");

        term.reset();
        progress.measure(8 * 1024 * 1024);
        progress.draw_now();
        let drawn = term.contents();
        assert!(drawn.contains("[==>"), "no bar after measuring: {drawn:?}");
        assert!(
            drawn.contains("1.00 MiB/8.00 MiB"),
            "arrived/total is missing: {drawn:?}"
        );
        progress.finish();
    }

    /// A line nobody can identify is a bug in a caller, not something to show:
    /// `0 B 0 B/s` on its own is exactly the line that looked broken.
    #[test]
    fn a_label_less_transfer_is_never_drawn() {
        let term = terminal();
        let progress = visible(Some(8 * 1024 * 1024), "", &term);
        progress.add(4 * 1024 * 1024);
        progress.draw_now();
        assert_eq!(
            term.contents().trim(),
            "",
            "a line without a label was drawn"
        );
        progress.finish();
    }

    /// Internal fetches (the download index) draw nothing at all.
    #[test]
    fn a_small_or_silent_transfer_draws_nothing() {
        let term = terminal();
        for progress in [
            Progress::new(64 * 1024, "download-index.json"),
            Progress::silent(),
            visible(Some(64 * 1024), "download-index.json", &term),
        ] {
            progress.add(4096);
            progress.draw_now();
            progress.finish();
        }
        assert_eq!(term.contents().trim(), "", "something was drawn");
    }

    impl Progress {
        /// Test-only: draw now instead of waiting for the refresh interval.
        fn draw_now(&self) {
            self.bar.force_draw();
        }
    }
}
