use crate::diff::Hints;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A batch closes once this long passes with no new relevant event.
pub const QUIET: Duration = Duration::from_millis(200);

/// A batch closes this long after its first event even if events never stop (a checkout, a
/// formatter run).
pub const MAX_WAIT: Duration = Duration::from_secs(2);

/// One global quiet window over every path (spec 7.2), so a burst of saves spread over a few
/// hundred milliseconds is one batch. The clock is passed in, so the tests drive it.
pub struct Batcher {
    quiet: Duration,
    max_wait: Duration,
    first: Option<Instant>,
    last: Option<Instant>,
    hints: Hints,
}

impl Batcher {
    pub fn new(quiet: Duration, max_wait: Duration) -> Batcher {
        Batcher {
            quiet,
            max_wait,
            first: None,
            last: None,
            hints: Hints::default(),
        }
    }

    /// Adds an event's paths to the open batch, opening one if none is.
    pub fn push(&mut self, now: Instant, paths: impl IntoIterator<Item = PathBuf>, full: bool) {
        self.first.get_or_insert(now);
        self.last = Some(now);
        self.hints.paths.extend(paths);
        self.hints.full |= full;
    }

    /// When the open batch closes; `None` while no batch is open.
    pub fn deadline(&self) -> Option<Instant> {
        let (first, last) = (self.first?, self.last?);
        Some((last + self.quiet).min(first + self.max_wait))
    }

    /// The merged hints of the open batch once its deadline has passed, which empties it.
    pub fn take(&mut self, now: Instant) -> Option<Hints> {
        if now < self.deadline()? {
            return None;
        }

        self.first = None;
        self.last = None;
        Some(std::mem::take(&mut self.hints))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn five_quick_pushes_are_one_batch_after_quiet() {
        let t0 = Instant::now();
        let mut b = Batcher::new(QUIET, MAX_WAIT);

        for i in 0..5 {
            b.push(t0 + ms(40 * i), [PathBuf::from(format!("/p/f{i}"))], false);
        }

        let last = t0 + ms(160);
        assert!(b.take(last + ms(199)).is_none());
        let h = b.take(last + ms(200)).unwrap();
        assert_eq!(h.paths.len(), 5);
        assert!(!h.full);
        assert!(b.deadline().is_none());
        assert!(b.take(last + ms(5000)).is_none());
    }

    #[test]
    fn a_stream_that_never_pauses_closes_at_max_wait() {
        let t0 = Instant::now();
        let mut b = Batcher::new(QUIET, MAX_WAIT);
        let mut closed = None;

        for i in 0..40 {
            let now = t0 + ms(100 * i);

            if let Some(h) = b.take(now) {
                closed = Some((i, h));
                break;
            }

            b.push(now, [PathBuf::from("/p/x")], false);
        }

        // t0 + 2 s
        let (i, h) = closed.unwrap();
        assert_eq!(i, 20);
        assert_eq!(h.paths.len(), 1);
    }

    #[test]
    fn the_deadline_is_the_earlier_of_quiet_and_max_wait() {
        let t0 = Instant::now();
        let mut b = Batcher::new(QUIET, MAX_WAIT);
        assert!(b.deadline().is_none());

        b.push(t0, [PathBuf::from("/p/a")], false);
        assert_eq!(b.deadline(), Some(t0 + QUIET));

        b.push(t0 + ms(1900), [PathBuf::from("/p/b")], false);
        assert_eq!(b.deadline(), Some(t0 + MAX_WAIT));
    }

    #[test]
    fn full_rescan_flag_survives_merging() {
        let t0 = Instant::now();
        let mut b = Batcher::new(QUIET, MAX_WAIT);
        b.push(t0, [], true);
        b.push(t0 + ms(50), [PathBuf::from("/p/a")], false);

        let h = b.take(t0 + ms(250)).unwrap();
        assert!(h.full);
        assert_eq!(h.paths.len(), 1);

        b.push(t0 + ms(300), [PathBuf::from("/p/b")], false);
        assert!(!b.take(t0 + ms(500)).unwrap().full);
    }
}
