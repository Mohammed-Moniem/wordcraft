//! The effective inline/block placement shared by editing and DOCX serialization.

use super::{ContentControl, ControlEvent, ControlWrap, control_events, pair_events};
use crate::para::OBJ;
use crate::{Blocks, InlineObject, Paragraph};

struct ParagraphEdges {
    start_prefix: usize,
    marker_prefix: usize,
    end_suffix: usize,
    marker_suffix: usize,
}

impl ParagraphEdges {
    fn scan(para: &Paragraph) -> Self {
        let mut edges = Self { start_prefix: para.len(), marker_prefix: para.len(), end_suffix: 0, marker_suffix: 0 };
        let mut index = 0usize;
        for (off, ch) in para.text.char_indices() {
            crate::work::steps(1);
            #[cfg(test)]
            super::note_nesting_work(|work| work.boundary_steps = work.boundary_steps.saturating_add(1));
            let object = if ch == OBJ {
                let object = para.objects.get(index);
                index = index.saturating_add(1);
                object
            } else {
                None
            };
            let start = matches!(object, Some(InlineObject::ControlStart { .. }));
            let end = matches!(object, Some(InlineObject::ControlEnd));
            if !start {
                edges.start_prefix = edges.start_prefix.min(off);
            }
            if !end {
                edges.end_suffix = off.saturating_add(ch.len_utf8());
            }
            if !start && !end {
                edges.marker_prefix = edges.marker_prefix.min(off);
                edges.marker_suffix = off.saturating_add(ch.len_utf8());
            }
        }
        edges
    }
}

/// Where a block list's whole controls are serialized. Orphan markers are omitted.
#[derive(Default)]
pub struct ControlPlan<'a> {
    /// Controls opening before each block, outermost first.
    pub opens: Vec<Vec<&'a ContentControl>>,
    /// Controls closing after each block.
    pub closes: Vec<u32>,
    /// Marker byte offsets serialized inline in each paragraph.
    pub inline: Vec<Vec<usize>>,
    pub(super) ranges: Vec<PlannedRange>,
    pub(super) events: Vec<ControlEvent<'a>>,
}

pub(super) struct PlannedRange {
    pub open: usize,
    pub close: usize,
    pub block_span: Option<(usize, usize)>,
    pub raw_span: (usize, usize),
    pub parent: Option<usize>,
    pub can_block: bool,
}

/// Effective closures of a serialized sequence of rows, or cells in one row.
/// Every stored open is written; unmatched opens close implicitly at the scope's end.
#[derive(Default)]
pub struct ControlWrapPlan {
    /// Closures after each element, including implicit closures after the last element.
    pub closes: Vec<usize>,
    pub(super) ranges: Vec<PlannedWrapRange>,
}

pub(super) struct PlannedWrapRange {
    pub first: usize,
    pub open: usize,
    pub last: usize,
    pub explicit: bool,
}

/// Plan exactly the wraps of the elements the writer emits. Omitted rows are excluded by
/// the caller; each cell sequence is a separate scope. Work is bounded by stored opens.
pub fn control_wrap_plan(wraps: &[&ControlWrap]) -> ControlWrapPlan {
    let mut plan = ControlWrapPlan { closes: vec![0; wraps.len()], ..Default::default() };
    let mut pending = Vec::new();
    for (index, wrap) in wraps.iter().enumerate() {
        crate::work::steps(1);
        for open in 0..wrap.open.len() {
            crate::work::steps(1);
            pending.push((index, open));
        }
        let closes = (wrap.close as usize).min(pending.len());
        if let Some(slot) = plan.closes.get_mut(index) {
            *slot = closes;
        }
        for _ in 0..closes {
            crate::work::steps(1);
            if let Some((first, open)) = pending.pop() {
                plan.ranges.push(PlannedWrapRange { first, open, last: index, explicit: true });
            }
        }
    }
    if let Some(last) = wraps.len().checked_sub(1) {
        if let Some(slot) = plan.closes.get_mut(last) {
            *slot = slot.saturating_add(pending.len());
        }
        while let Some((first, open)) = pending.pop() {
            crate::work::steps(1);
            plan.ranges.push(PlannedWrapRange { first, open, last, explicit: false });
        }
    }
    plan
}

impl ControlPlan<'_> {
    /// Whether a block-level control surrounds block `i`.
    pub fn covers(&self, i: usize) -> bool {
        let mut open = 0i64;
        for k in 0..=i {
            open += self.opens.get(k).map(|v| v.len() as i64).unwrap_or(0);
            if k < i {
                open -= i64::from(self.closes.get(k).copied().unwrap_or(0));
            }
        }
        open > 0
    }

    /// Marker byte offsets serialized inline in paragraph `i`.
    pub fn inline_in(&self, i: usize) -> Vec<usize> {
        self.inline.get(i).cloned().unwrap_or_default()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Inline,
    Block,
}

/// Plan one block list. Cells have independent lists, and are planned separately.
pub fn control_plan(bl: &Blocks) -> ControlPlan<'_> {
    let n = bl.len();
    let mut out = ControlPlan { opens: vec![Vec::new(); n], closes: vec![0; n], inline: vec![Vec::new(); n], ..Default::default() };
    let events = control_events(bl);
    if events.is_empty() {
        return out;
    }
    let (pairs, _) = pair_events(&events);
    let mut ranges: Vec<(usize, usize)> = pairs.iter().map(|(o, c, _)| (*o, *c)).collect();
    ranges.sort_unstable();
    let mut parent: Vec<Option<usize>> = vec![None; ranges.len()];
    let mut stack: Vec<usize> = Vec::new();
    for (k, (o, _)) in ranges.iter().enumerate() {
        while let Some(&top) = stack.last() {
            if ranges.get(top).is_some_and(|(_, tc)| tc < o) {
                stack.pop();
            } else {
                break;
            }
        }
        if let Some(slot) = parent.get_mut(k) {
            *slot = stack.last().copied();
        }
        stack.push(k);
    }
    // One text/object pass per paragraph, including raw OBJ characters with no object.
    // Per-range prefix scans and object_at lookups would make deep imported queries quadratic.
    let edges: Vec<_> = bl.iter().map(|block| block.as_para().map(ParagraphEdges::scan)).collect();
    let at_start = |b: usize, off: usize| edges.get(b).and_then(Option::as_ref).is_some_and(|edge| off <= edge.start_prefix);
    let at_end =
        |b: usize, off: usize| edges.get(b).and_then(Option::as_ref).is_some_and(|edge| off.saturating_add(OBJ.len_utf8()) >= edge.end_suffix);
    let mut mode = vec![Mode::Inline; ranges.len()];
    let mut span = vec![(0, 0); ranges.len()];
    let mut can_block = vec![false; ranges.len()];
    for (k, (o, c)) in ranges.iter().enumerate() {
        let (Some(ControlEvent::Open { block: sb, off: so, control }), Some(e)) = (events.get(*o), events.get(*c)) else { continue };
        let (eb, eo) = (e.block(), e.off());
        let eligible = match (so, eo) {
            (Some(so), Some(eo)) if *sb == eb => {
                control.block
                    && edges
                        .get(*sb)
                        .and_then(Option::as_ref)
                        .is_some_and(|edge| *so <= edge.marker_prefix && eo.saturating_add(OBJ.len_utf8()) >= edge.marker_suffix)
            }
            _ => true,
        };
        if let Some(slot) = can_block.get_mut(k) {
            *slot = eligible;
        }
        let m = match (so, eo) {
            (Some(so), Some(eo)) if *sb == eb => {
                let parent_inline = parent.get(k).copied().flatten().and_then(|p| mode.get(p)).is_some_and(|m| *m == Mode::Inline);
                if !parent_inline && control.block && at_start(*sb, *so) && at_end(eb, eo) { Mode::Block } else { Mode::Inline }
            }
            _ => Mode::Block,
        };
        if let Some(slot) = mode.get_mut(k) {
            *slot = m;
        }
        if let Some(slot) = span.get_mut(k) {
            *slot = (*sb, eb);
        }
    }
    // A block range beginning in a previous sibling's closing paragraph starts at the next
    // block. Inline ranges do not cause this shift, even when their markers precede it.
    let mut first_close = vec![usize::MAX; n + 1];
    let mut second_close = vec![usize::MAX; n + 1];
    let mut first_owner = vec![usize::MAX; n + 1];
    for (q, (_, qc)) in ranges.iter().enumerate() {
        if mode.get(q) != Some(&Mode::Block) {
            continue;
        }
        let Some(&(_, qe)) = span.get(q) else { continue };
        let e = qe.min(n);
        if *qc < first_close[e] {
            second_close[e] = first_close[e];
            first_close[e] = *qc;
            first_owner[e] = q;
        } else if *qc < second_close[e] {
            second_close[e] = *qc;
        }
    }
    for k in 0..ranges.len() {
        if mode.get(k) != Some(&Mode::Block) {
            continue;
        }
        let Some(&(o, _)) = ranges.get(k) else { continue };
        let Some(&(sb, eb)) = span.get(k) else { continue };
        let e = sb.min(n);
        let close = if first_owner[e] == k { second_close[e] } else { first_close[e] };
        if close < o
            && let Some(s) = span.get_mut(k)
        {
            s.0 = sb.saturating_add(1).min(eb);
        }
    }
    for (k, (o, c)) in ranges.iter().enumerate() {
        let Some(ControlEvent::Open { control, .. }) = events.get(*o) else { continue };
        let block_span = if mode.get(k) == Some(&Mode::Block) {
            let Some(&(sb, eb)) = span.get(k) else { continue };
            if let Some(v) = out.opens.get_mut(sb) {
                v.push(*control);
            }
            if let Some(x) = out.closes.get_mut(eb) {
                *x = x.saturating_add(1);
            }
            Some((sb, eb))
        } else {
            for e in [events.get(*o), events.get(*c)].into_iter().flatten() {
                if let Some(off) = e.off()
                    && let Some(v) = out.inline.get_mut(e.block())
                {
                    v.push(off);
                }
            }
            None
        };
        out.ranges.push(PlannedRange {
            open: *o,
            close: *c,
            block_span,
            raw_span: span.get(k).copied().map(|(_, end)| (events.get(*o).map_or(0, ControlEvent::block), end)).unwrap_or_default(),
            parent: parent.get(k).copied().flatten(),
            can_block: can_block.get(k).copied().unwrap_or(false),
        });
    }
    for v in &mut out.inline {
        v.sort_unstable();
        v.dedup();
    }
    out.events = events;
    out
}
