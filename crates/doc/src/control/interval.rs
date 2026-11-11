//! Range updates and overflow lookup for one captured effective control stream.

use std::collections::BTreeSet;

const NONE: i64 = i64::MIN / 4;

#[derive(Clone)]
struct Node {
    min_fresh: i64,
    at_min: i64,
    above_min: i64,
    total_delta: i64,
    fresh_delta: i64,
    candidates: BTreeSet<(usize, usize)>,
}

impl Default for Node {
    fn default() -> Self {
        Self { min_fresh: 0, at_min: 0, above_min: NONE, total_delta: 0, fresh_delta: 0, candidates: BTreeSet::new() }
    }
}

impl Node {
    fn peak(&self) -> i64 {
        self.at_min.max(self.above_min)
    }

    fn eligible(&self) -> i64 {
        if self.min_fresh > 0 { self.peak() } else { self.above_min }
    }

    fn add(&mut self, total: i64, fresh: i64) {
        self.min_fresh = self.min_fresh.saturating_add(fresh);
        self.at_min = self.at_min.saturating_add(total);
        if self.above_min != NONE {
            self.above_min = self.above_min.saturating_add(total);
        }
        self.total_delta = self.total_delta.saturating_add(total);
        self.fresh_delta = self.fresh_delta.saturating_add(fresh);
    }
}

pub(super) struct Intervals {
    nodes: Vec<Node>,
    len: usize,
}

impl Intervals {
    pub fn new(values: &[usize]) -> Self {
        let len = values.len().max(1);
        let mut out = Self { nodes: vec![Node::default(); len.saturating_mul(4).saturating_add(4)], len };
        out.build(1, 0, len, values);
        out
    }

    fn build(&mut self, node: usize, from: usize, to: usize, values: &[usize]) {
        if to - from == 1 {
            if let Some(slot) = self.nodes.get_mut(node) {
                slot.at_min = i64::try_from(values.get(from).copied().unwrap_or(0)).unwrap_or(i64::MAX / 4);
            }
            return;
        }
        let middle = from + (to - from) / 2;
        self.build(node * 2, from, middle, values);
        self.build(node * 2 + 1, middle, to, values);
        self.pull(node);
    }

    fn pull(&mut self, node: usize) {
        let (Some(left), Some(right)) = (self.nodes.get(node * 2), self.nodes.get(node * 2 + 1)) else { return };
        let minimum = left.min_fresh.min(right.min_fresh);
        let mut at_min = NONE;
        let mut above_min = NONE;
        for child in [left, right] {
            if child.min_fresh == minimum {
                at_min = at_min.max(child.at_min);
                above_min = above_min.max(child.above_min);
            } else {
                above_min = above_min.max(child.peak());
            }
        }
        if let Some(slot) = self.nodes.get_mut(node) {
            slot.min_fresh = minimum;
            slot.at_min = at_min;
            slot.above_min = above_min;
        }
    }

    fn push(&mut self, node: usize) {
        let (total, fresh) = self.nodes.get(node).map(|value| (value.total_delta, value.fresh_delta)).unwrap_or_default();
        if total != 0 || fresh != 0 {
            for child in [node * 2, node * 2 + 1] {
                if let Some(slot) = self.nodes.get_mut(child) {
                    slot.add(total, fresh);
                }
            }
            if let Some(slot) = self.nodes.get_mut(node) {
                slot.total_delta = 0;
                slot.fresh_delta = 0;
            }
        }
    }

    pub fn change(&mut self, from: usize, to: usize, delta: i64, fresh: bool, index: usize) {
        if from < to {
            self.update(1, 0, self.len, from, to.min(self.len), delta, fresh, (from, index));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn update(&mut self, node: usize, from: usize, to: usize, first: usize, last: usize, delta: i64, fresh: bool, key: (usize, usize)) {
        #[cfg(test)]
        super::note_nesting_work(|work| work.interval_steps = work.interval_steps.saturating_add(1));
        if first >= to || last <= from {
            return;
        }
        if first <= from && to <= last {
            if let Some(slot) = self.nodes.get_mut(node) {
                slot.add(delta, if fresh { delta } else { 0 });
                if fresh {
                    if delta > 0 {
                        slot.candidates.insert(key);
                    } else {
                        slot.candidates.remove(&key);
                    }
                }
            }
            return;
        }
        self.push(node);
        let middle = from + (to - from) / 2;
        self.update(node * 2, from, middle, first, last, delta, fresh, key);
        self.update(node * 2 + 1, middle, to, first, last, delta, fresh, key);
        self.pull(node);
    }

    pub fn overflow(&mut self, room: usize) -> Option<(usize, usize)> {
        let limit = i64::try_from(room).unwrap_or(i64::MAX);
        if self.nodes.get(1).is_none_or(|node| node.eligible() <= limit) {
            return None;
        }
        self.find(1, 0, self.len, limit, None)
    }

    fn find(&mut self, node: usize, from: usize, to: usize, limit: i64, candidate: Option<(usize, usize)>) -> Option<(usize, usize)> {
        #[cfg(test)]
        super::note_nesting_work(|work| work.interval_steps = work.interval_steps.saturating_add(1));
        let candidate = self.nodes.get(node).and_then(|value| value.candidates.last().copied()).into_iter().chain(candidate).max();
        if to - from == 1 {
            return candidate.map(|(_, index)| (from, index));
        }
        self.push(node);
        let middle = from + (to - from) / 2;
        if self.nodes.get(node * 2).is_some_and(|value| value.eligible() > limit) {
            self.find(node * 2, from, middle, limit, candidate)
        } else {
            self.find(node * 2 + 1, middle, to, limit, candidate)
        }
    }

    pub fn depth(&mut self, point: usize) -> usize {
        self.at(1, 0, self.len, point.min(self.len.saturating_sub(1)))
    }

    fn at(&mut self, node: usize, from: usize, to: usize, point: usize) -> usize {
        #[cfg(test)]
        super::note_nesting_work(|work| work.interval_steps = work.interval_steps.saturating_add(1));
        if to - from == 1 {
            return self.nodes.get(node).map(|value| usize::try_from(value.peak()).unwrap_or(0)).unwrap_or(0);
        }
        self.push(node);
        let middle = from + (to - from) / 2;
        if point < middle { self.at(node * 2, from, middle, point) } else { self.at(node * 2 + 1, middle, to, point) }
    }
}
