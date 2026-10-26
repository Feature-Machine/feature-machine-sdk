// Rate limit for the pipe to the game: axis and delta updates of one control go at most once per
// WINDOW_MS (the last axis value, the sum of deltas); buttons are never merged or dropped, and a
// button event first sends what waits for its control, so the order per control stays (plan,
// feature API). A mouse gives up to 8000 moves per second; the game needs one per frame.
use crate::engine::{Out, Value};
use std::collections::BTreeMap;

pub const WINDOW_MS: u64 = 8;

#[derive(Default)]
pub struct Coalescer {
    waiting: BTreeMap<(String, String), Value>,
    since: Option<u64>,
}

impl Coalescer {
    /// Takes engine output; returns what to send now.
    pub fn push(&mut self, events: Vec<Out>, now: u64) -> Vec<Out> {
        let mut send = vec![];
        for ev in events {
            match ev {
                Out::Control { feature, control, value: v @ (Value::Axis(_) | Value::Delta(_)) } => {
                    let slot = self.waiting.entry((feature, control)).or_insert(match v { Value::Delta(_) => Value::Delta(0.0), _ => v });
                    *slot = match (*slot, v) { (Value::Delta(a), Value::Delta(b)) => Value::Delta(a + b), _ => v };
                    self.since.get_or_insert(now);
                }
                Out::Control { feature, control, value } => {
                    let key = (feature, control);
                    if let Some(w) = self.waiting.remove(&key) {
                        send.push(Out::Control { feature: key.0.clone(), control: key.1.clone(), value: w });
                    }
                    send.push(Out::Control { feature: key.0, control: key.1, value });
                }
                other => send.push(other),
            }
        }
        send.extend(self.flush(now));
        send
    }

    /// What waited for WINDOW_MS; call it on a timer as well.
    pub fn flush(&mut self, now: u64) -> Vec<Out> {
        match self.since {
            Some(t) if now >= t + WINDOW_MS => self.drain(),
            _ => vec![],
        }
    }

    /// All that waits, now: before a focus change, so values reach the game they belong to.
    pub fn drain(&mut self) -> Vec<Out> {
        self.since = None;
        std::mem::take(&mut self.waiting).into_iter().map(|((feature, control), value)| Out::Control { feature, control, value }).collect()
    }

    /// When the next flush is due, if something waits.
    pub fn due(&self) -> Option<u64> { self.since.map(|t| t + WINDOW_MS) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(c: &str, value: Value) -> Out { Out::Control { feature: "f".into(), control: c.into(), value } }

    #[test]
    fn merges_axes_and_deltas_and_never_buttons() {
        let mut c = Coalescer::default();
        assert!(c.push(vec![ev("a", Value::Axis(0.1)), ev("d", Value::Delta(2.0))], 0).is_empty());
        assert!(c.push(vec![ev("a", Value::Axis(0.3)), ev("d", Value::Delta(-5.0))], 4).is_empty());
        let out = c.push(vec![ev("b", Value::Button(true)), ev("b", Value::Button(false))], 5);
        assert_eq!(out, vec![ev("b", Value::Button(true)), ev("b", Value::Button(false))]);
        assert_eq!(c.flush(8), vec![ev("a", Value::Axis(0.3)), ev("d", Value::Delta(-3.0))]);
        assert!(c.flush(20).is_empty());
    }
}
