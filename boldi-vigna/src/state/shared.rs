// Copyright (C) 2026 PlanetScale
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.
use std::cell::RefCell;
use std::rc::Rc;

use super::NodeState;
use crate::interval::Interval;
use crate::positions::TermPositions;

/// One compiled child that several walks read: the order walks of a gap-read
/// `Unordered` (`NodeState::compile_gaps`). The child runs once per document
/// and keeps what it produced, each interval with its gap count, so every
/// reader replays the stream from its own place exactly as a private copy
/// would produce it. A state's output is a function of the document's
/// positions and its call count alone, and `reset` reaches every state from
/// the root before a document's first read. A child returns no interval
/// after its first `None`, so the source asks it no further.
pub(crate) struct SharedSource {
    child: NodeState,
    produced: Vec<(Interval, Option<u64>)>,
    exhausted: bool,
    /// Reset, and nothing read since: every reader resets the source at a
    /// document's start, and only the first reaches the child. Resetting the
    /// child once per reader would double the resets at every level of a
    /// NEAR chain.
    reset: bool,
}

impl SharedSource {
    pub(crate) fn new(child: NodeState) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            child,
            produced: Vec::new(),
            exhausted: false,
            reset: false,
        }))
    }
}

/// A reader of a [`SharedSource`], at its own place in the child's stream.
pub(crate) struct SharedState {
    source: Rc<RefCell<SharedSource>>,
    next: usize,
    last_gaps: Option<u64>,
}

impl SharedState {
    pub(crate) fn new(source: &Rc<RefCell<SharedSource>>) -> Self {
        Self {
            source: Rc::clone(source),
            next: 0,
            last_gaps: None,
        }
    }

    /// Every reader resets the source at a document's start, before any
    /// reader reads; the first reset reaches the child, the rest find the
    /// source reset already.
    pub(crate) fn reset(&mut self) {
        let mut source = self.source.borrow_mut();
        if !source.reset {
            source.child.reset();
            source.produced.clear();
            source.exhausted = false;
            source.reset = true;
        }
        self.next = 0;
        self.last_gaps = None;
    }

    pub(crate) fn next_interval(&mut self, positions: &impl TermPositions) -> Option<Interval> {
        let mut source = self.source.borrow_mut();
        source.reset = false;
        if self.next == source.produced.len() {
            if source.exhausted {
                return None;
            }
            let Some(interval) = source.child.next_interval(positions) else {
                source.exhausted = true;
                return None;
            };
            let gaps = source.child.gaps();
            source.produced.push((interval, gaps));
        }
        let (interval, gaps) = source.produced[self.next];
        self.next += 1;
        self.last_gaps = gaps;
        Some(interval)
    }

    pub(crate) fn gaps(&self) -> Option<u64> {
        self.last_gaps
    }
}
