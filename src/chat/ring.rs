//! A bounded scrollback buffer.
//!
//! Chat runs for hours; an unbounded `Vec` of messages is a slow memory leak.
//! Both source apps trim their history to a configurable cap (twi and yc:
//! `scrollback_limit`, default 2000); this port defaults to 1000 per chat and
//! makes the cap a constructor argument so the config can set it.

use std::collections::VecDeque;

/// A FIFO ring: pushing beyond the cap evicts the oldest entry.
#[derive(Debug, Clone)]
pub struct Ring<T> {
    items: VecDeque<T>,
    cap: usize,
}

/// The default per-chat scrollback cap.
pub const DEFAULT_SCROLLBACK: usize = 1000;

impl<T> Ring<T> {
    /// A ring holding at most `cap` items. A cap of 0 would make every push a
    /// no-op, which can only be a configuration accident, so it is raised to 1.
    pub fn new(cap: usize) -> Self {
        Self {
            items: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    /// Append, evicting the oldest item when full. Returns `true` when an old
    /// item was evicted — callers use this to keep scroll offsets stable.
    pub fn push(&mut self, item: T) -> bool {
        let evicted = self.items.len() == self.cap;
        if evicted {
            self.items.pop_front();
        }
        self.items.push_back(item);
        evicted
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.items.iter()
    }

    /// Mutable access, for marking messages deleted in place.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.items.iter_mut()
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        self.items.get(index)
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Remove the first item matching the predicate and return it.
    pub fn remove_first(&mut self, mut matches: impl FnMut(&T) -> bool) -> Option<T> {
        let index = self.items.iter().position(&mut matches)?;
        self.items.remove(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushing_past_the_cap_evicts_the_oldest() {
        let mut ring = Ring::new(3);
        assert!(!ring.push(1));
        assert!(!ring.push(2));
        assert!(!ring.push(3));
        // The fourth push must evict 1, not grow the buffer.
        assert!(ring.push(4));
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.iter().copied().collect::<Vec<_>>(), [2, 3, 4]);
    }

    #[test]
    fn memory_stays_bounded_over_a_long_session() {
        let mut ring = Ring::new(100);
        for i in 0..10_000 {
            ring.push(i);
        }
        assert_eq!(ring.len(), 100);
        assert_eq!(ring.get(0), Some(&9_900));
        assert_eq!(ring.get(99), Some(&9_999));
    }

    #[test]
    fn a_zero_cap_is_raised_rather_than_swallowing_everything() {
        let mut ring = Ring::new(0);
        ring.push("only");
        assert_eq!(ring.len(), 1);
    }

    #[test]
    fn remove_first_takes_only_the_first_match() {
        let mut ring = Ring::new(10);
        for value in ["a", "b", "a"] {
            ring.push(value);
        }
        assert_eq!(ring.remove_first(|v| *v == "a"), Some("a"));
        assert_eq!(ring.iter().copied().collect::<Vec<_>>(), ["b", "a"]);
    }
}
