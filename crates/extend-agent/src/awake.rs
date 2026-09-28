//! Whether this computer is awake, as the `awake` frame tells Extend (`docs/device-protocol.md`).
//!
//! The screen watch reads the computer every few seconds; [`AwakeTracker`] turns those readings
//! into the states worth sending, and [`AwakeReporter`] numbers them for every pair's connection:
//! `run` is random for each app process and `seq` increases across all of its connections, so the
//! service keeps the newest state whichever connection it came on.
//!
//! * Awake: nothing blocks the screen, or only an admin prompt does (the Carbon is at it).
//! * Not awake: locked, another account on screen, or asleep (a Mac's display is off).
//! * A computer the watch can't read (a headless server) is always awake.
//!
//! `input_seen` says whether the change came with a sign of the Carbon: an unlock, or switching
//! back to this account, always is; a display that wakes by itself (a scheduled wake, a
//! notification) is not unless input followed within 10 seconds. A wake without that sign is sent
//! with `input_seen: false`, and `awake` goes again with `true` at the first input after it, so a
//! wake request resolves only when the Carbon actually came. Awake is information only: nothing
//! is withheld for it, and Extend never wakes the computer.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use extend_protocol::frames::DeviceFrame;
use extend_protocol::model::SleepState;
use tokio::sync::watch;
use uuid::Uuid;

use crate::drivers::screen_lock::{ScreenBlock, ScreenReading};

/// Input this recent counts as the Carbon being at the computer.
pub const RECENT_INPUT: Duration = Duration::from_secs(10);

/// What the computer is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AwakeNow {
    pub awake: bool,
    pub sleep_state: Option<SleepState>,
    pub input_seen: Option<bool>,
}

impl AwakeNow {
    /// A computer the watch can't read: always awake.
    pub const UNKNOWN_IS_AWAKE: AwakeNow = AwakeNow {
        awake: true,
        sleep_state: None,
        input_seen: None,
    };
}

/// Turns screen readings into the changes worth reporting.
#[derive(Debug, Default)]
pub struct AwakeTracker {
    last: Option<AwakeNow>,
    /// Woke without a sign of the Carbon: the first input sends `awake` again.
    awaiting_input: bool,
}

fn recent(idle: Option<Duration>) -> Option<bool> {
    idle.map(|d| d < RECENT_INPUT)
}

impl AwakeTracker {
    /// The state to send after `reading`, when it changed (always for the first reading).
    pub fn observe(&mut self, reading: &ScreenReading) -> Option<AwakeNow> {
        let sleep_state = reading.block.and_then(ScreenBlock::sleep_state);
        let awake = sleep_state.is_none();
        let Some(last) = self.last else {
            // The first look: nothing changed, so nothing is known about input.
            let now = AwakeNow {
                awake,
                sleep_state,
                input_seen: None,
            };
            self.last = Some(now);
            return Some(now);
        };
        let now = if awake && !last.awake {
            // Coming back from a lock screen or another account means the Carbon unlocked it.
            let input_seen = match last.sleep_state {
                Some(SleepState::Locked | SleepState::OtherSession) => Some(true),
                _ => recent(reading.input_idle),
            };
            self.awaiting_input = input_seen == Some(false);
            AwakeNow {
                awake,
                sleep_state: None,
                input_seen,
            }
        } else if awake && self.awaiting_input {
            if recent(reading.input_idle) != Some(true) {
                return None;
            }
            self.awaiting_input = false;
            AwakeNow {
                awake,
                sleep_state: None,
                input_seen: Some(true),
            }
        } else if !awake && (last.awake || last.sleep_state != sleep_state) {
            self.awaiting_input = false;
            AwakeNow {
                awake,
                sleep_state,
                input_seen: None,
            }
        } else {
            return None;
        };
        self.last = Some(now);
        Some(now)
    }
}

/// The app-wide state and numbering for `awake` frames.
pub struct AwakeReporter {
    run: Uuid,
    seq: AtomicU64,
    state: watch::Sender<AwakeNow>,
}

impl Default for AwakeReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl AwakeReporter {
    pub fn new() -> Self {
        Self {
            run: Uuid::new_v4(),
            seq: AtomicU64::new(0),
            state: watch::channel(AwakeNow::UNKNOWN_IS_AWAKE).0,
        }
    }

    pub fn set(&self, now: AwakeNow) {
        self.state.send_replace(now);
    }

    pub fn now(&self) -> AwakeNow {
        *self.state.borrow()
    }

    pub fn subscribe(&self) -> watch::Receiver<AwakeNow> {
        self.state.subscribe()
    }

    /// The frame for the current state, with the next number.
    pub fn frame(&self) -> DeviceFrame {
        let now = self.now();
        DeviceFrame::Awake {
            awake: now.awake,
            sleep_state: now.sleep_state,
            input_seen: now.input_seen,
            run: Some(self.run),
            seq: Some(self.seq.fetch_add(1, Ordering::SeqCst) + 1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(block: Option<ScreenBlock>, idle_s: Option<u64>) -> ScreenReading {
        ScreenReading {
            block,
            input_idle: idle_s.map(Duration::from_secs),
        }
    }

    #[test]
    fn lock_and_unlock() {
        let mut t = AwakeTracker::default();
        let first = t.observe(&reading(None, Some(400))).unwrap();
        assert_eq!(first, AwakeNow::UNKNOWN_IS_AWAKE);
        assert_eq!(t.observe(&reading(None, Some(1))), None);
        let locked = t.observe(&reading(Some(ScreenBlock::Locked), None)).unwrap();
        assert_eq!(
            (locked.awake, locked.sleep_state, locked.input_seen),
            (false, Some(SleepState::Locked), None)
        );
        assert_eq!(t.observe(&reading(Some(ScreenBlock::Locked), None)), None);
        // Unlocking is the Carbon, whatever the idle time says.
        let unlocked = t.observe(&reading(None, Some(300))).unwrap();
        assert_eq!(
            (unlocked.awake, unlocked.sleep_state, unlocked.input_seen),
            (true, None, Some(true))
        );
    }

    #[test]
    fn a_wake_without_input_is_reported_again_at_the_first_input() {
        let mut t = AwakeTracker::default();
        t.observe(&reading(None, None));
        let asleep = t.observe(&reading(Some(ScreenBlock::Asleep), Some(900))).unwrap();
        assert_eq!(asleep.sleep_state, Some(SleepState::Asleep));
        // The display came on by itself: nobody touched anything for 15 minutes.
        let woke = t.observe(&reading(None, Some(900))).unwrap();
        assert_eq!((woke.awake, woke.input_seen), (true, Some(false)));
        assert_eq!(t.observe(&reading(None, Some(905))), None);
        // Then the Carbon moves the mouse.
        let touched = t.observe(&reading(None, Some(2))).unwrap();
        assert_eq!((touched.awake, touched.input_seen), (true, Some(true)));
        assert_eq!(t.observe(&reading(None, Some(1))), None);
        // A wake with input straight away is the Carbon at once; one the OS can't time is unknown.
        t.observe(&reading(Some(ScreenBlock::Asleep), None));
        assert_eq!(t.observe(&reading(None, Some(3))).unwrap().input_seen, Some(true));
        t.observe(&reading(Some(ScreenBlock::Asleep), None));
        assert_eq!(t.observe(&reading(None, None)).unwrap().input_seen, None);
    }

    #[test]
    fn an_admin_prompt_is_awake_and_other_states_change_the_reason() {
        let mut t = AwakeTracker::default();
        let first = t.observe(&reading(Some(ScreenBlock::AdminPrompt), None)).unwrap();
        assert!(first.awake);
        let other = t.observe(&reading(Some(ScreenBlock::OtherSession), None)).unwrap();
        assert_eq!(other.sleep_state, Some(SleepState::OtherSession));
        let locked = t.observe(&reading(Some(ScreenBlock::Locked), None)).unwrap();
        assert_eq!(locked.sleep_state, Some(SleepState::Locked));
        assert_eq!(t.observe(&reading(Some(ScreenBlock::Locked), None)), None);
    }

    #[test]
    fn frames_carry_one_run_and_rising_numbers() {
        let r = AwakeReporter::new();
        let (DeviceFrame::Awake { run: a, seq: s1, .. }, DeviceFrame::Awake { run: b, seq: s2, .. }) =
            (r.frame(), r.frame())
        else {
            panic!()
        };
        assert_eq!(a, b);
        assert!(a.is_some());
        assert!(s2 > s1);
        r.set(AwakeNow {
            awake: false,
            sleep_state: Some(SleepState::Locked),
            input_seen: None,
        });
        let v = serde_json::to_value(r.frame()).unwrap();
        assert_eq!(v["awake"], false);
        assert_eq!(v["sleep_state"], "locked");
        assert!(v.get("input_seen").is_none());
        assert_eq!(v["seq"], 3);
    }
}
