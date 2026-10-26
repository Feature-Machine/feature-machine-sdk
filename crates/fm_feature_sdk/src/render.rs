// Keeps one host render pass for a feature: asks at once, asks again each second while the pass
// is Busy or the host times out, and stops asking after any other refusal (no pass in this
// host, a missing capability). Generic over the handle and driven by frame times, so a
// feature's tests need no host. The Color filter and the Mirage features use it.
use crate::HostError;

/// Time between two acquire attempts while the pass is busy or unavailable.
pub const RETRY_US: u64 = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hold<H> {
    /// No attempt yet, or the pass was busy (`busy`) or timed out at the last attempt.
    Waiting { since_us: Option<u64>, busy: bool },
    Active(H),
    /// The host refused the pass for good.
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Keeper<H> { pub hold: Hold<H> }

impl<H: Copy> Keeper<H> {
    pub fn new() -> Self { Self { hold: Hold::Waiting { since_us: None, busy: false } } }

    /// True when the feature calls render_acquire in this frame.
    pub fn due(&self, now_us: u64) -> bool {
        match self.hold {
            Hold::Waiting { since_us: None, .. } => true,
            Hold::Waiting { since_us: Some(t), .. } => now_us.saturating_sub(t) >= RETRY_US,
            _ => false,
        }
    }

    /// Busy and Timeout can end (another feature releases the pass); other errors cannot.
    pub fn acquired(&mut self, now_us: u64, result: Result<H, HostError>) {
        self.hold = match result {
            Ok(handle) => Hold::Active(handle),
            Err(HostError::Busy) => Hold::Waiting { since_us: Some(now_us), busy: true },
            Err(HostError::Timeout) => Hold::Waiting { since_us: Some(now_us), busy: false },
            Err(_) => Hold::Failed,
        };
    }

    pub fn handle(&self) -> Option<H> { if let Hold::Active(h) = self.hold { Some(h) } else { None } }
}

impl<H: Copy> Default for Keeper<H> {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquires_at_once_then_keeps_the_pass() {
        let mut k = Keeper::<u64>::new();
        assert!(k.due(0));
        k.acquired(0, Ok(7));
        assert_eq!(k.handle(), Some(7));
        assert!(!k.due(10_000_000));
    }

    #[test]
    fn busy_retries_each_second() {
        let mut k = Keeper::<u64>::new();
        k.acquired(5_000_000, Err(HostError::Busy));
        assert_eq!(k.hold, Hold::Waiting { since_us: Some(5_000_000), busy: true });
        assert!(!k.due(5_900_000));
        assert!(k.due(6_000_000));
        k.acquired(6_000_000, Ok(1));
        assert_eq!(k.handle(), Some(1));
    }

    #[test]
    fn refusal_stops_the_attempts() {
        let mut k = Keeper::<u64>::new();
        k.acquired(0, Err(HostError::Refused));
        assert_eq!(k.hold, Hold::Failed);
        assert!(!k.due(60_000_000));
    }
}
