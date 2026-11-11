//! Trim fresh controls against the effective serialized tree, preserving every existing pair.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound::{Excluded, Included, Unbounded};
use std::sync::Arc;

use super::{ControlEvent, ControlWrap, MAX_CONTROL_NESTING, MAX_NESTING_WALK, NestingStories, control_plan, control_wrap_plan, cut_control_markers};
use crate::{Block, Blocks, Document, InlineObject, Part, Path, Pos, StoryRef};

type WrapEvents = BTreeMap<Path, Vec<usize>>;

#[derive(Clone, Debug)]
enum BoundaryKind {
    Marker(usize),
    Open(usize),
    Close,
    ImplicitClose,
}

#[derive(Clone, Debug)]
struct Boundary {
    path: Path,
    kind: BoundaryKind,
}

struct Range {
    start: Boundary,
    end: Boundary,
    fresh: bool,
    block: bool,
    from: usize,
    to: usize,
    source: Option<Placement>,
    parent: Option<usize>,
    live: bool,
}

#[derive(Clone, Copy)]
struct Placement {
    first: usize,
    last: usize,
    open: usize,
    close: usize,
    can_block: bool,
    prefix_ready: bool,
    suffix_ready: bool,
}

#[derive(Default)]
struct Group {
    from: usize,
    to: usize,
    closes: BTreeSet<(usize, usize)>,
    starts: BTreeSet<(usize, usize)>,
}

#[derive(Default)]
struct ParagraphMarkers {
    starts: BTreeSet<usize>,
    ends: BTreeSet<usize>,
    prefixes: BTreeSet<(usize, usize)>,
    suffixes: BTreeSet<(usize, usize)>,
}

enum Event {
    Open(usize),
    Close(usize),
    Box(u32),
    Unknown,
    Point,
}

#[derive(Default)]
struct Stream {
    ranges: Vec<Range>,
    events: Vec<Event>,
    groups: Vec<Group>,
    paragraphs: BTreeMap<Path, ParagraphMarkers>,
}

impl Stream {
    fn range(&mut self, start: Boundary, end: Boundary, fresh: &dyn Fn(&Boundary) -> bool, block: bool) -> usize {
        let index = self.ranges.len();
        self.ranges.push(Range { fresh: fresh(&start), start, end, block, from: 0, to: 0, source: None, parent: None, live: true });
        index
    }

    fn list(&mut self, bl: &Blocks, path: &mut Vec<u32>, fresh: &dyn Fn(&Boundary) -> bool, level: usize) {
        if level > MAX_NESTING_WALK {
            self.events.push(Event::Unknown);
            return;
        }
        let plan = control_plan(bl);
        #[cfg(test)]
        super::note_nesting_work(|work| {
            work.planner_blocks = work.planner_blocks.saturating_add(bl.len());
            work.planner_ranges = work.planner_ranges.saturating_add(plan.ranges.len());
        });
        let group_base = self.groups.len();
        self.groups.extend((0..bl.len()).map(|_| Group::default()));
        let mut block_opens = vec![Vec::new(); bl.len()];
        let mut block_closes = vec![Vec::new(); bl.len()];
        let mut inline = BTreeMap::new();
        let mut table_indices = BTreeMap::<usize, usize>::new();
        let mut points = Vec::with_capacity(plan.events.len());
        for event in &plan.events {
            let mut at = path.clone();
            at.push(u32::try_from(event.block()).unwrap_or(u32::MAX));
            let kind = if let Some(off) = event.off() {
                BoundaryKind::Marker(off)
            } else if matches!(event, ControlEvent::Open { .. }) {
                let next = table_indices.entry(event.block()).or_default();
                let kind = BoundaryKind::Open(*next);
                *next = next.saturating_add(1);
                kind
            } else {
                BoundaryKind::Close
            };
            points.push(Boundary { path: Path(at), kind });
        }
        let mut indices = Vec::with_capacity(plan.ranges.len());
        for range in &plan.ranges {
            let (Some(start), Some(end)) = (points.get(range.open), points.get(range.close)) else { continue };
            let index = self.range(start.clone(), end.clone(), fresh, range.block_span.is_some());
            indices.push(index);
            let parent = range.parent.and_then(|parent| indices.get(parent)).copied();
            if let Some(slot) = self.ranges.get_mut(index) {
                slot.parent = parent;
                slot.source = Some(Placement {
                    first: group_base + range.raw_span.0,
                    last: group_base + range.raw_span.1,
                    open: range.open,
                    close: range.close,
                    can_block: range.can_block,
                    prefix_ready: false,
                    suffix_ready: false,
                });
            }
            if let Some((from, to)) = range.block_span {
                if let Some(opens) = block_opens.get_mut(from) {
                    opens.push(index);
                }
                if let Some(closes) = block_closes.get_mut(to) {
                    closes.push(index);
                }
            } else if let (BoundaryKind::Marker(from), BoundaryKind::Marker(to)) = (&start.kind, &end.kind) {
                inline.insert((start.path.last(), *from), Event::Open(index));
                inline.insert((end.path.last(), *to), Event::Close(index));
            }
        }
        for (i, block) in bl.iter().enumerate() {
            path.push(u32::try_from(i).unwrap_or(u32::MAX));
            if let Some(group) = self.groups.get_mut(group_base + i) {
                group.from = self.events.len();
            }
            self.events.push(Event::Point);
            for &index in block_opens.get(i).into_iter().flatten() {
                self.events.push(Event::Open(index));
            }
            match &**block {
                Block::Para(para) => {
                    let markers = self.paragraphs.entry(Path(path.clone())).or_default();
                    for (off, object) in para.objects_with_offsets() {
                        match object {
                            InlineObject::ControlStart { .. } => {
                                markers.starts.insert(off);
                            }
                            InlineObject::ControlEnd => {
                                markers.ends.insert(off);
                            }
                            _ => {}
                        }
                    }
                    for (off, object) in para.objects_with_offsets() {
                        #[cfg(test)]
                        super::note_nesting_work(|work| work.trim_objects = work.trim_objects.saturating_add(1));
                        #[cfg(test)]
                        super::note_nesting_work(|work| work.planner_objects = work.planner_objects.saturating_add(1));
                        if let Some(event) = inline.remove(&(i, off)) {
                            self.events.push(event);
                        } else if !matches!(object, InlineObject::ControlStart { .. } | InlineObject::ControlEnd) {
                            self.events.extend(object.text_boxes().into_iter().map(Event::Box));
                        }
                    }
                }
                Block::Table(table) => {
                    let rows: Vec<_> = table
                        .rows
                        .iter()
                        .enumerate()
                        .filter(|(_, row)| !row.cells.is_empty())
                        .map(|(r, row)| {
                            let mut row_path = path.clone();
                            row_path.push(u32::try_from(r).unwrap_or(u32::MAX));
                            (Path(row_path), &row.controls)
                        })
                        .collect();
                    let (row_opens, row_closes) = self.wraps(&rows, fresh);
                    for (r, row) in table.rows.iter().enumerate() {
                        if row.cells.is_empty() {
                            continue;
                        }
                        path.push(u32::try_from(r).unwrap_or(u32::MAX));
                        self.events.extend(row_opens.get(&Path(path.clone())).into_iter().flatten().copied().map(Event::Open));
                        let cells: Vec<_> = row
                            .cells
                            .iter()
                            .enumerate()
                            .map(|(c, cell)| {
                                let mut cell_path = path.clone();
                                cell_path.push(u32::try_from(c).unwrap_or(u32::MAX));
                                (Path(cell_path), &cell.controls)
                            })
                            .collect();
                        let (cell_opens, cell_closes) = self.wraps(&cells, fresh);
                        for (c, cell) in row.cells.iter().enumerate() {
                            path.push(u32::try_from(c).unwrap_or(u32::MAX));
                            self.events.extend(cell_opens.get(&Path(path.clone())).into_iter().flatten().copied().map(Event::Open));
                            self.list(&cell.blocks, path, fresh, level + 1);
                            self.events.extend(cell_closes.get(&Path(path.clone())).into_iter().flatten().copied().map(Event::Close));
                            path.pop();
                        }
                        self.events.extend(row_closes.get(&Path(path.clone())).into_iter().flatten().copied().map(Event::Close));
                        path.pop();
                    }
                }
            }
            if let Some(group) = self.groups.get_mut(group_base + i) {
                group.to = self.events.len().saturating_add(1);
            }
            self.events.push(Event::Point);
            for &index in block_closes.get(i).into_iter().flatten().rev() {
                self.events.push(Event::Close(index));
            }
            path.pop();
        }
    }

    fn wraps(&mut self, wraps: &[(Path, &ControlWrap)], fresh: &dyn Fn(&Boundary) -> bool) -> (WrapEvents, WrapEvents) {
        let (mut opens, mut closes) = (BTreeMap::<Path, Vec<usize>>::new(), BTreeMap::<Path, Vec<usize>>::new());
        let plan = control_wrap_plan(&wraps.iter().map(|(_, wrap)| *wrap).collect::<Vec<_>>());
        for range in plan.ranges {
            let (Some((first, _)), Some((last, _))) = (wraps.get(range.first), wraps.get(range.last)) else { continue };
            let start = Boundary { path: first.clone(), kind: BoundaryKind::Open(range.open) };
            let end = Boundary { path: last.clone(), kind: if range.explicit { BoundaryKind::Close } else { BoundaryKind::ImplicitClose } };
            let index = self.range(start, end, fresh, true);
            opens.entry(first.clone()).or_default().push(index);
            closes.entry(last.clone()).or_default().push(index);
        }
        // Pairing visits closes inside-out; serialization opens in original outer-first order.
        for ranges in opens.values_mut() {
            ranges.sort_unstable_by_key(|&index| match self.ranges.get(index).map(|range| &range.start.kind) {
                Some(BoundaryKind::Open(nth)) => *nth,
                _ => 0,
            });
        }
        (opens, closes)
    }
}

#[derive(Default)]
struct Cuts {
    markers: BTreeSet<usize>,
    opens: BTreeSet<usize>,
    closes: usize,
}

fn cut(cuts: &mut BTreeMap<Path, Cuts>, boundary: &Boundary) {
    let entry = cuts.entry(boundary.path.clone()).or_default();
    match boundary.kind {
        BoundaryKind::Marker(off) => {
            entry.markers.insert(off);
        }
        BoundaryKind::Open(nth) => {
            entry.opens.insert(nth);
        }
        BoundaryKind::Close => entry.closes = entry.closes.saturating_add(1),
        // The writer creates this end; no stored close belongs to it.
        BoundaryKind::ImplicitClose => {}
    }
}

fn apply_wrap(wrap: &mut ControlWrap, cuts: Option<&Cuts>) {
    let Some(cuts) = cuts else { return };
    let mut nth = 0;
    wrap.open.retain(|_| {
        let keep = !cuts.opens.contains(&nth);
        nth += 1;
        keep
    });
    wrap.close = wrap.close.saturating_sub(u32::try_from(cuts.closes).unwrap_or(u32::MAX));
}

fn apply(bl: &mut Blocks, path: &mut Vec<u32>, cuts: &BTreeMap<Path, Cuts>, level: usize) {
    if level > MAX_NESTING_WALK {
        return;
    }
    for (i, block) in bl.iter_mut().enumerate() {
        path.push(u32::try_from(i).unwrap_or(u32::MAX));
        let own = cuts.get(&Path(path.clone()));
        match &**block {
            Block::Para(para) => {
                if let Some(own) = own {
                    let markers: Vec<_> = para
                        .objects_with_offsets()
                        .enumerate()
                        .filter(|(_, (off, _))| own.markers.contains(off))
                        .map(|(index, (off, _))| (off, index))
                        .collect();
                    if !markers.is_empty()
                        && let Block::Para(para) = Arc::make_mut(block)
                    {
                        cut_control_markers(para, &markers);
                    }
                }
            }
            Block::Table(_) => {
                if let Block::Table(table) = Arc::make_mut(block) {
                    apply_wrap(&mut table.controls, own);
                    for (r, row) in table.rows.iter_mut().enumerate() {
                        if row.cells.is_empty() {
                            continue;
                        }
                        path.push(u32::try_from(r).unwrap_or(u32::MAX));
                        apply_wrap(&mut row.controls, cuts.get(&Path(path.clone())));
                        for (c, cell) in row.cells.iter_mut().enumerate() {
                            path.push(u32::try_from(c).unwrap_or(u32::MAX));
                            apply_wrap(&mut cell.controls, cuts.get(&Path(path.clone())));
                            apply(&mut cell.blocks, path, cuts, level + 1);
                            path.pop();
                        }
                        path.pop();
                    }
                }
            }
        }
        path.pop();
    }
}

struct Admission {
    stream: Stream,
    intervals: super::interval::Intervals,
    inline_parent: Vec<usize>,
    dependents: Vec<BTreeSet<usize>>,
    ready: BTreeSet<usize>,
    cuts: BTreeMap<Path, Cuts>,
    removed: usize,
}

impl Admission {
    fn new(mut stream: Stream, inherited: usize, copied: &BTreeSet<u32>, stories: &mut NestingStories<'_>) -> Self {
        let mut values = vec![inherited; stream.events.len()];
        for (point, event) in stream.events.iter().enumerate() {
            match *event {
                Event::Open(index) => {
                    if let Some(range) = stream.ranges.get_mut(index) {
                        range.from = point;
                    }
                }
                Event::Close(index) => {
                    if let Some(range) = stream.ranges.get_mut(index) {
                        range.to = point;
                    }
                }
                Event::Box(id) if !copied.contains(&id) => {
                    if let Some(slot) = values.get_mut(point) {
                        *slot = slot.saturating_add(stories.peak(id, 0));
                    }
                }
                Event::Unknown => {
                    if let Some(slot) = values.get_mut(point) {
                        *slot = slot.saturating_add(MAX_CONTROL_NESTING + 1);
                    }
                }
                _ => {}
            }
        }
        for (index, range) in stream.ranges.iter().enumerate() {
            if let Some(source) = range.source {
                if source.can_block
                    && let Some(group) = stream.groups.get_mut(source.first)
                {
                    group.starts.insert((source.open, index));
                }
                if range.block
                    && let Some(group) = stream.groups.get_mut(source.last)
                {
                    group.closes.insert((source.close, index));
                }
            }
        }
        for (index, range) in stream.ranges.iter_mut().enumerate() {
            if let Some(source) = &mut range.source {
                if source.first != source.last {
                    source.prefix_ready = true;
                    source.suffix_ready = true;
                } else if source.can_block
                    && let (BoundaryKind::Marker(start), BoundaryKind::Marker(end)) = (&range.start.kind, &range.end.kind)
                    && let Some(markers) = stream.paragraphs.get_mut(&range.start.path)
                {
                    source.prefix_ready = markers.ends.first().is_none_or(|first| *first > *start);
                    source.suffix_ready = markers.starts.last().is_none_or(|last| *last < *end);
                    markers.prefixes.insert((*start, index));
                    markers.suffixes.insert((*end, index));
                }
                if range.block {
                    let shifted =
                        stream.groups.get(source.first).and_then(|group| group.closes.first()).is_some_and(|(close, _)| *close < source.open);
                    let first = if shifted { source.first.saturating_add(1).min(source.last) } else { source.first };
                    range.from = stream.groups.get(first).map_or(range.from, |group| group.from);
                    range.to = stream.groups.get(source.last).map_or(range.to, |group| group.to);
                }
            }
        }
        let sentinel = stream.ranges.len();
        let mut inline_parent: Vec<_> = (0..=sentinel).collect();
        let mut intervals = super::interval::Intervals::new(&values);
        for (index, range) in stream.ranges.iter().enumerate() {
            if range.block
                && let Some(slot) = inline_parent.get_mut(index)
            {
                *slot = range.parent.unwrap_or(sentinel);
            }
            intervals.change(range.from, range.to, 1, range.fresh, index);
        }
        let mut out = Self {
            stream,
            intervals,
            inline_parent,
            dependents: vec![BTreeSet::new(); sentinel + 1],
            ready: BTreeSet::new(),
            cuts: BTreeMap::new(),
            removed: 0,
        };
        for index in 0..sentinel {
            if out.stream.ranges.get(index).is_some_and(|range| !range.block) {
                let parent = out.stream.ranges.get(index).and_then(|range| range.parent).unwrap_or(sentinel);
                let ancestor = out.inline_ancestor(parent);
                if ancestor != sentinel
                    && let Some(dependents) = out.dependents.get_mut(ancestor)
                {
                    dependents.insert(index);
                }
                #[cfg(test)]
                super::note_nesting_work(|work| work.dependent_visits = work.dependent_visits.saturating_add(1));
            }
        }
        out
    }

    fn inline_ancestor(&mut self, mut index: usize) -> usize {
        let mut visited = Vec::new();
        while let Some(&parent) = self.inline_parent.get(index) {
            #[cfg(test)]
            super::note_nesting_work(|work| work.dependent_visits = work.dependent_visits.saturating_add(1));
            if parent == index {
                break;
            }
            visited.push(index);
            index = parent;
        }
        for child in visited {
            if let Some(slot) = self.inline_parent.get_mut(child) {
                *slot = index;
            }
        }
        index
    }

    fn move_range(&mut self, index: usize) {
        let Some(range) = self.stream.ranges.get(index) else { return };
        let Some(source) = range.source else { return };
        if !range.live || !range.block {
            return;
        }
        let shifted = self.stream.groups.get(source.first).and_then(|group| group.closes.first()).is_some_and(|(close, _)| *close < source.open);
        let first = if shifted { source.first.saturating_add(1).min(source.last) } else { source.first };
        let from = self.stream.groups.get(first).map_or(range.from, |group| group.from);
        let to = self.stream.groups.get(source.last).map_or(range.to, |group| group.to);
        if (from, to) == (range.from, range.to) {
            return;
        }
        self.intervals.change(range.from, range.to, -1, range.fresh, index);
        self.intervals.change(from, to, 1, range.fresh, index);
        if let Some(range) = self.stream.ranges.get_mut(index) {
            range.from = from;
            range.to = to;
        }
        #[cfg(test)]
        super::note_nesting_work(|work| work.placement_updates = work.placement_updates.saturating_add(1));
    }

    fn change_close(&mut self, index: usize, insert: bool) {
        let Some(source) = self.stream.ranges.get(index).and_then(|range| range.source) else { return };
        let Some(group) = self.stream.groups.get_mut(source.last) else { return };
        let before = group.closes.first().map_or(usize::MAX, |(close, _)| *close);
        if insert {
            group.closes.insert((source.close, index));
        } else {
            group.closes.remove(&(source.close, index));
        }
        let after = group.closes.first().map_or(usize::MAX, |(close, _)| *close);
        if before == after {
            return;
        }
        let candidates: Vec<_> = group
            .starts
            .range((Excluded((before.min(after), usize::MAX)), Included((before.max(after), usize::MAX))))
            .map(|(_, range)| *range)
            .collect();
        for candidate in candidates {
            #[cfg(test)]
            super::note_nesting_work(|work| work.dependent_visits = work.dependent_visits.saturating_add(1));
            self.move_range(candidate);
        }
    }

    fn remove_marker(&mut self, boundary: &Boundary, start: bool) {
        let BoundaryKind::Marker(off) = boundary.kind else { return };
        let Some(markers) = self.stream.paragraphs.get_mut(&boundary.path) else { return };
        if start {
            let before = markers.starts.last().copied();
            markers.starts.remove(&off);
            let after = markers.starts.last().copied();
            if before != after
                && let Some(before) = before
            {
                let lower = after.map_or(Unbounded, |after| Excluded((after, usize::MAX)));
                for &(_, index) in markers.suffixes.range((lower, Included((before, usize::MAX)))) {
                    if let Some(source) = self.stream.ranges.get_mut(index).and_then(|range| range.source.as_mut()) {
                        source.suffix_ready = true;
                    }
                    self.ready.insert(index);
                }
            }
        } else {
            let before = markers.ends.first().copied();
            markers.ends.remove(&off);
            let after = markers.ends.first().copied();
            if before != after
                && let Some(before) = before
            {
                let upper = after.map_or(Unbounded, |after| Excluded((after, 0)));
                for &(_, index) in markers.prefixes.range((Included((before, 0)), upper)) {
                    if let Some(source) = self.stream.ranges.get_mut(index).and_then(|range| range.source.as_mut()) {
                        source.prefix_ready = true;
                    }
                    self.ready.insert(index);
                }
            }
        }
    }

    fn unlink_inline(&mut self, index: usize) {
        let sentinel = self.stream.ranges.len();
        let parent = self.stream.ranges.get(index).and_then(|range| range.parent).unwrap_or(sentinel);
        let ancestor = self.inline_ancestor(parent);
        if let Some(dependents) = self.dependents.get_mut(ancestor) {
            dependents.remove(&index);
        }
        if let Some(slot) = self.inline_parent.get_mut(index) {
            *slot = ancestor;
        }
        let mut children = self.dependents.get_mut(index).map(std::mem::take).unwrap_or_default();
        if ancestor == sentinel {
            for child in children {
                self.ready.insert(child);
                #[cfg(test)]
                super::note_nesting_work(|work| work.dependent_visits = work.dependent_visits.saturating_add(1));
            }
        } else if let Some(target) = self.dependents.get_mut(ancestor) {
            // Merge smaller into larger. Dead inline chains never require walking every
            // descendant again, and a surviving dependent moves only logarithmically often.
            if target.len() < children.len() {
                std::mem::swap(target, &mut children);
            }
            for child in children {
                target.insert(child);
                #[cfg(test)]
                super::note_nesting_work(|work| work.dependent_visits = work.dependent_visits.saturating_add(1));
            }
        }
    }

    fn promote_ready(&mut self) {
        while let Some(index) = self.ready.pop_first() {
            #[cfg(test)]
            super::note_nesting_work(|work| work.dependent_visits = work.dependent_visits.saturating_add(1));
            let Some(range) = self.stream.ranges.get(index) else { continue };
            let Some(source) = range.source else { continue };
            if !range.live || range.block || !source.can_block || !source.prefix_ready || !source.suffix_ready {
                continue;
            }
            let parent = range.parent.unwrap_or(self.stream.ranges.len());
            if self.inline_ancestor(parent) != self.stream.ranges.len() {
                continue;
            }
            if let Some(range) = self.stream.ranges.get_mut(index) {
                range.block = true;
            }
            self.unlink_inline(index);
            self.move_range(index);
            self.change_close(index, true);
        }
    }

    fn remove(&mut self, index: usize) {
        let Some(range) = self.stream.ranges.get(index) else { return };
        if !range.live || !range.fresh {
            return;
        }
        self.intervals.change(range.from, range.to, -1, true, index);
        let (start, end, block) = (range.start.clone(), range.end.clone(), range.block);
        if let Some(range) = self.stream.ranges.get_mut(index) {
            range.live = false;
        }
        cut(&mut self.cuts, &start);
        cut(&mut self.cuts, &end);
        self.removed = self.removed.saturating_add(1);
        if block {
            self.change_close(index, false);
        } else {
            self.unlink_inline(index);
        }
        self.remove_marker(&start, true);
        self.remove_marker(&end, false);
        self.promote_ready();
    }
}

#[allow(clippy::too_many_arguments)]
fn trim_stream(
    bl: &mut Blocks,
    inherited: usize,
    room: usize,
    fresh: &dyn Fn(&Boundary) -> bool,
    copied: &BTreeSet<u32>,
    stories: &mut NestingStories<'_>,
    boxes: &mut BTreeMap<u32, usize>,
    end: Option<&mut Pos>,
) -> usize {
    let mut stream = Stream::default();
    stream.list(bl, &mut Vec::new(), fresh, 0);
    let mut admission = Admission::new(stream, inherited, copied, stories);
    while let Some((_, index)) = admission.intervals.overflow(room) {
        admission.remove(index);
    }
    for (point, event) in admission.stream.events.iter().enumerate() {
        if let Event::Box(id) = event
            && copied.contains(id)
        {
            let left = room.saturating_sub(admission.intervals.depth(point));
            boxes.entry(*id).and_modify(|current| *current = (*current).min(left)).or_insert(left);
        }
    }
    if let Some(end) = end
        && let Some(cuts) = admission.cuts.get(&end.path)
    {
        end.off = end.off.saturating_sub(cuts.markers.range(..end.off).count().saturating_mul(crate::para::OBJ.len_utf8()));
    }
    if !admission.cuts.is_empty() {
        apply(bl, &mut Vec::new(), &admission.cuts, 0);
    }
    admission.removed
}

fn has_orphan_open(bl: &Blocks, level: usize) -> bool {
    if level > MAX_NESTING_WALK {
        return true;
    }
    let events = super::control_events(bl);
    let (_, orphans) = super::pair_events(&events);
    if orphans.iter().any(|&index| matches!(events.get(index), Some(ControlEvent::Open { .. }))) {
        return true;
    }
    bl.iter().any(|block| match &**block {
        Block::Para(_) => false,
        Block::Table(table) => {
            table.rows.iter().filter(|row| !row.cells.is_empty()).flat_map(|row| &row.cells).any(|cell| has_orphan_open(&cell.blocks, level + 1))
        }
    })
}

fn trim_all(bl: &mut Blocks, room: usize, copied: &BTreeSet<u32>, boxes: &mut BTreeMap<u32, usize>) -> usize {
    if has_orphan_open(bl, 0) {
        // The marker API bounds hostile table-close expansion. Retain its conservative
        // syntactic treatment for unpaired inputs; whole valid ranges use the shared plan.
        super::trim_control_list(bl, 0, room, 0, boxes)
    } else {
        trim_stream(bl, 0, room, &|_| true, copied, &mut NestingStories::new(&BTreeMap::new()), boxes, None)
    }
}

pub(super) fn trim_effective(bl: &mut Blocks, parts: &mut BTreeMap<u32, Part>, room: usize) -> usize {
    let copied: BTreeSet<_> = parts.keys().copied().collect();
    let mut boxes = BTreeMap::new();
    let mut removed = trim_all(bl, room, &copied, &mut boxes);
    let mut done = BTreeMap::<u32, usize>::new();
    let mut pending: BTreeSet<_> = boxes.keys().copied().collect();
    #[cfg(test)]
    super::note_nesting_work(|work| work.trim_queue_max = work.trim_queue_max.max(pending.len()));
    while let Some(id) = pending.pop_first() {
        let Some(&room) = boxes.get(&id) else { continue };
        if done.get(&id).is_some_and(|previous| *previous <= room) {
            continue;
        }
        done.insert(id, room);
        if let Some(part) = parts.get_mut(&id) {
            #[cfg(test)]
            super::note_nesting_work(|work| work.trim_story_scans = work.trim_story_scans.saturating_add(1));
            let mut inner = BTreeMap::new();
            removed = removed.saturating_add(trim_all(&mut part.blocks, room, &copied, &mut inner));
            for (next, room) in inner {
                boxes.entry(next).and_modify(|current| *current = (*current).min(room)).or_insert(room);
                pending.insert(next);
            }
            #[cfg(test)]
            super::note_nesting_work(|work| work.trim_queue_max = work.trim_queue_max.max(pending.len()));
        }
    }
    removed
}

impl Document {
    /// Bound only controls introduced by a staged paste, using its actual merged placement.
    /// Existing deep imports and shared destination stories are protected, never rewritten.
    pub(crate) fn trim_pasted_controls(&mut self, from: &Pos, mut end: Pos, original_parts: &BTreeSet<u32>) -> Pos {
        let copied: BTreeSet<_> = self.parts.keys().filter(|id| !original_parts.contains(*id)).copied().collect();
        let inherited = match from.story {
            StoryRef::Body => 0,
            StoryRef::Part(id) => self.outer_control_nesting(id),
        };
        let snapshot = self.parts.clone();
        let mut stories = NestingStories::for_edit(&snapshot);
        let mut boxes = BTreeMap::new();
        let from_key = (&from.path, from.off);
        let to_key = (&end.path, end.off);
        let fresh = |boundary: &Boundary| {
            let off = match boundary.kind {
                BoundaryKind::Marker(off) => off,
                _ => 0,
            };
            (&boundary.path, off) >= from_key && (&boundary.path, off) < to_key
        };
        let mut adjusted = end.clone();
        if let Ok(bl) = self.story_mut(from.story) {
            trim_stream(bl, inherited, MAX_CONTROL_NESTING, &fresh, &copied, &mut stories, &mut boxes, Some(&mut adjusted));
        }
        end = adjusted;
        let mut done = BTreeMap::<u32, usize>::new();
        let mut pending: BTreeSet<_> = boxes.keys().copied().collect();
        while let Some(id) = pending.pop_first() {
            let Some(&room) = boxes.get(&id) else { continue };
            if done.get(&id).is_some_and(|previous| *previous <= room) {
                continue;
            }
            done.insert(id, room);
            if let Some(part) = self.parts.get_mut(&id) {
                let mut inner = BTreeMap::new();
                trim_all(&mut part.blocks, room, &copied, &mut inner);
                for (next, room) in inner {
                    boxes.entry(next).and_modify(|current| *current = (*current).min(room)).or_insert(room);
                    pending.insert(next);
                }
            }
        }
        end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Paragraph;
    use crate::control::{ContentControl, ControlKind, nesting_peak, take_nesting_work, walk_controls};
    use crate::para::{OBJ, ShapeKind};
    use crate::props::CharProps;

    fn marker(id: usize, block: bool) -> InlineObject {
        InlineObject::ControlStart {
            control: Box::new(ContentControl { tag: format!("range-{id}"), block, kind: ControlKind::RichText, ..Default::default() }),
        }
    }

    fn protected_paragraph() -> Paragraph {
        let mut para = Paragraph::new();
        for id in 0..64 {
            para.insert_object(para.len(), marker(id, false), &CharProps::default()).unwrap();
        }
        para.insert_text(para.len(), "x", &CharProps::default()).unwrap();
        for _ in 0..64 {
            para.insert_object(para.len(), InlineObject::ControlEnd, &CharProps::default()).unwrap();
        }
        para
    }

    fn imported(depth: usize, block: bool, text: &str) -> Paragraph {
        let mut para = Paragraph::new();
        para.text = format!("{}{text}{}", OBJ.to_string().repeat(depth), OBJ.to_string().repeat(depth));
        para.objects = (0..depth).map(|index| marker(index, block)).chain((0..depth).map(|_| InlineObject::ControlEnd)).collect();
        para
    }

    #[test]
    fn nesting_effective_interval_admission_has_one_plan_and_bounded_wide_cascade_work() {
        let measure = |width: usize| {
            let protected = protected_paragraph();
            let boundary = protected.len();
            let mut body = Vec::new();
            for i in 0..=width {
                let mut para = protected.clone();
                if i > 0 {
                    para.insert_object(para.len(), InlineObject::ControlEnd, &CharProps::default()).unwrap();
                }
                if i < width {
                    para.insert_object(para.len(), marker(1000 + i, true), &CharProps::default()).unwrap();
                }
                body.push(Arc::new(Block::Para(para)));
            }
            take_nesting_work();
            let removed = trim_stream(
                &mut body,
                0,
                64,
                &|point| matches!(point.kind, BoundaryKind::Marker(off) if off >= boundary),
                &BTreeSet::new(),
                &mut NestingStories::new(&BTreeMap::new()),
                &mut BTreeMap::new(),
                None,
            );
            let work = take_nesting_work();
            assert_eq!(removed, width, "each unshifted block exposes the next protected 64-level paragraph");
            assert_eq!(work.planner_blocks, width + 1, "the source list is planned once, never once per dropped range: {work:?}");
            assert_eq!(work.planner_objects, 128 * (width + 1) + 2 * width);
            assert!(work.placement_updates >= width.saturating_sub(1), "exercise the actual first-close cascade: {work:?}");
            assert!(work.placement_updates <= width * 2);
            assert_eq!(nesting_peak(&body, &BTreeMap::new()), 64);
            let mut controls = 0;
            walk_controls(&body, &mut |_| controls += 1);
            assert_eq!(controls, 64 * (width + 1));
            work.interval_steps
        };
        let small = measure(32);
        let wide = measure(256);
        assert!(wide <= small * 13, "8x the input permits logarithmic interval overhead, not repeated document scans: {small} -> {wide}");

        let boundary_work = |depth: usize| {
            let mut doc = Document::new();
            doc.body = vec![Arc::new(Block::Para(imported(depth, true, "x")))];
            let at = Pos::body(0, depth * OBJ.len_utf8());
            take_nesting_work();
            let mut observed = 0;
            let steps = crate::work::count(|| observed = doc.control_nesting(&at, &at));
            let work = take_nesting_work();
            assert_eq!(observed, depth, "existing deep block-hinted imports remain fully counted");
            assert_eq!(work.boundary_steps, 2 * depth + 1, "one actual character/object boundary pass: {work:?}");
            steps
        };
        let boundary_small = boundary_work(128);
        let boundary_large = boundary_work(1024);
        assert!(
            boundary_large <= boundary_small * 9,
            "8x deep imported block hints must not rescan their prefixes: {boundary_small} -> {boundary_large}"
        );

        let nested_work = |depth: usize| {
            let mut body = vec![Arc::new(Block::Para(imported(depth, true, "keep")))];
            take_nesting_work();
            assert_eq!(
                trim_stream(&mut body, 64, 64, &|_| true, &BTreeSet::new(), &mut NestingStories::new(&BTreeMap::new()), &mut BTreeMap::new(), None),
                depth
            );
            let work = take_nesting_work();
            assert_eq!(work.planner_blocks, 1);
            assert_eq!(work.boundary_steps, 2 * depth + 4);
            assert!(work.dependent_visits <= depth * 8, "block removal never revisits dead descendants: {work:?}");
            assert_eq!(body[0].as_para().unwrap().text, "keep");
            work.interval_steps + work.dependent_visits
        };
        let nested_small = nested_work(128);
        let nested_large = nested_work(1024);
        assert!(
            nested_large <= nested_small * 13,
            "8x nested fresh block ranges permit bounded interval/dependent work: {nested_small} -> {nested_large}"
        );

        let inline_work = |depth: usize| {
            let mut para = Paragraph::new();
            para.text = OBJ.to_string().repeat(5 * depth);
            para.objects.extend((0..depth).map(|index| marker(index, false)));
            for index in 0..depth {
                para.objects.push(marker(100000 + index, false));
                para.objects.push(InlineObject::Shape {
                    kind: ShapeKind::TextBox,
                    w: 20.0,
                    h: 20.0,
                    fill: None,
                    stroke: None,
                    stroke_width: 0.0,
                    float: Default::default(),
                    story: Some(1),
                    freeform: None,
                    effects: Default::default(),
                    extra: Default::default(),
                });
                para.objects.push(InlineObject::ControlEnd);
            }
            para.objects.extend((0..depth).map(|_| InlineObject::ControlEnd));
            let parts = BTreeMap::from([(
                1,
                Part { blocks: vec![Arc::new(Block::Para(imported(63, false, "x")))], kind: crate::PartKind::TextBox, ..Default::default() },
            )]);
            let mut body = vec![Arc::new(Block::Para(para))];
            take_nesting_work();
            assert_eq!(
                trim_stream(
                    &mut body,
                    0,
                    64,
                    &|point| matches!(point.kind, BoundaryKind::Marker(off) if off < depth * OBJ.len_utf8()),
                    &BTreeSet::new(),
                    &mut NestingStories::new(&parts),
                    &mut BTreeMap::new(),
                    None
                ),
                depth
            );
            let work = take_nesting_work();
            assert_eq!(work.planner_blocks, 1);
            assert_eq!(work.story_scans, 1, "one shared protected story summary: {work:?}");
            assert!(work.dependent_visits <= 32 * depth, "dead inline chains meld live dependents without rescanning them per ancestor: {work:?}");
            assert_eq!(nesting_peak(&body, &parts), 64);
            let mut kept = Vec::new();
            walk_controls(&body, &mut |control| kept.push(control.tag.clone()));
            assert_eq!(kept, (0..depth).map(|index| format!("range-{}", 100000 + index)).collect::<Vec<_>>());
            work.interval_steps + work.dependent_visits
        };
        let inline_small = inline_work(128);
        let inline_large = inline_work(1024);
        assert!(
            inline_large <= inline_small * 13,
            "8x nested inline ancestors/shared protected siblings permit bounded work: {inline_small} -> {inline_large}"
        );

        // Compare actual placement and future promotion eligibility with the former writer's
        // predicates on raw OBJ/no-object, extra-object and mixed-marker paragraphs.
        let mut fixtures = vec![imported(4, true, "x"), imported(4, true, "")];
        let mut mixed = imported(4, true, "x");
        mixed.text.insert(0, 'é');
        mixed.text.push('🚀');
        fixtures.push(mixed);
        let mut missing = imported(4, true, "x");
        missing.objects.pop();
        fixtures.push(missing);
        let mut extra = imported(4, true, "x");
        extra.objects.push(InlineObject::BookmarkStart { name: "unused".into() });
        fixtures.push(extra);
        let mut raw = imported(4, true, "x");
        raw.text.insert(0, OBJ);
        fixtures.push(raw);
        let mut noncontrol = imported(4, true, "x");
        noncontrol.text.insert(0, OBJ);
        noncontrol.objects.insert(0, InlineObject::BookmarkStart { name: "visible".into() });
        fixtures.push(noncontrol);
        for para in fixtures {
            let body = vec![Arc::new(Block::Para(para.clone()))];
            let plan = control_plan(&body);
            let mut modes = Vec::<bool>::new();
            for range in &plan.ranges {
                let (Some(ControlEvent::Open { off: Some(start), control, .. }), Some(ControlEvent::Close { off: Some(end), .. })) =
                    (plan.events.get(range.open), plan.events.get(range.close))
                else {
                    panic!("paragraph pair")
                };
                let from = end + OBJ.len_utf8();
                let prefix = para.text.get(..*start).is_some_and(|text| text.chars().all(|ch| ch == OBJ));
                let suffix = para.text.get(from..).is_some_and(|text| text.chars().all(|ch| ch == OBJ));
                let starts_only =
                    (0..*start).step_by(OBJ.len_utf8()).all(|off| matches!(para.object_at(off), Some(InlineObject::ControlStart { .. })));
                let ends_only = (from..para.len()).step_by(OBJ.len_utf8()).all(|off| matches!(para.object_at(off), Some(InlineObject::ControlEnd)));
                let marker = |off| matches!(para.object_at(off), Some(InlineObject::ControlStart { .. } | InlineObject::ControlEnd));
                let eligible = control.block
                    && prefix
                    && suffix
                    && (0..*start).step_by(OBJ.len_utf8()).all(marker)
                    && (from..para.len()).step_by(OBJ.len_utf8()).all(marker);
                let inline_parent = range.parent.and_then(|parent| modes.get(parent)).is_some_and(|block| !*block);
                let block = !inline_parent && control.block && prefix && suffix && starts_only && ends_only;
                assert_eq!(range.block_span.is_some(), block);
                assert_eq!(range.can_block, eligible);
                modes.push(block);
            }
        }

        // A newly pasted implicit outer wrap can be the only fresh ancestor of protected
        // deep content. Its synthetic end shares a location with an existing explicit end.
        // Removing it must leave that unrelated stored close and every imported field intact.
        for cells in [false, true] {
            let mut table = crate::Table::new(if cells { 1 } else { 2 }, if cells { 2 } else { 1 }, 144.0);
            table.rows[0].cells[0].blocks = vec![Arc::new(Block::Para(protected_paragraph()))];
            let fresh_control = ContentControl { tag: "fresh-implicit".into(), block: true, ..Default::default() };
            let retained = ContentControl { tag: "retained-explicit".into(), title: "existing metadata".into(), block: true, ..Default::default() };
            if cells {
                table.rows[0].cells[0].controls.open.push(fresh_control);
                table.rows[0].cells[1].controls = ControlWrap { open: vec![retained], close: 1 };
            } else {
                table.rows[0].controls.open.push(fresh_control);
                table.rows[1].controls = ControlWrap { open: vec![retained], close: 1 };
            }
            let mut expected = table.clone();
            if cells {
                expected.rows[0].cells[0].controls.open.clear();
            } else {
                expected.rows[0].controls.open.clear();
            }
            let mut body = vec![Arc::new(Block::Table(table))];
            let fresh_path = Path(if cells { vec![0, 0, 0] } else { vec![0, 0] });
            assert_eq!(nesting_peak(&body, &BTreeMap::new()), 65);
            assert_eq!(
                trim_stream(
                    &mut body,
                    0,
                    64,
                    &|boundary| boundary.path == fresh_path,
                    &BTreeSet::new(),
                    &mut NestingStories::new(&BTreeMap::new()),
                    &mut BTreeMap::new(),
                    None
                ),
                1
            );
            assert_eq!(body, vec![Arc::new(Block::Table(expected))], "implicit cut never decrements the surviving explicit close");
            assert_eq!(nesting_peak(&body, &BTreeMap::new()), 64);
        }

        let scope_work = |cells: bool, depth: usize, room: usize| {
            let mut table = crate::Table::new(if cells { 1 } else { 2 }, if cells { 2 } else { 1 }, 144.0);
            let wrap = if cells { &mut table.rows[0].cells[0].controls } else { &mut table.rows[0].controls };
            wrap.open = (0..depth).map(|id| ContentControl { tag: format!("scope-{id}"), block: true, ..Default::default() }).collect();
            let n = wrap.open.len();
            assert_eq!(n, depth);
            let mut body = vec![Arc::new(Block::Table(table))];
            take_nesting_work();
            let mut removed = 0;
            let steps = crate::work::count(|| removed = trim_all(&mut body, room, &BTreeSet::new(), &mut BTreeMap::new()));
            let work = take_nesting_work();
            assert_eq!(removed, n - room);
            assert_eq!(work.planner_blocks, 3, "one outer table and two blank cell paragraphs are each planned once");
            assert!(steps >= (2 * depth) as u64);
            assert!(steps <= (2 * n + 16) as u64, "scope pairing stays linear in actual stored opens: {steps}");
            assert!(work.interval_steps <= depth * 256, "implicit scope updates are bounded: {work:?}");
            // Each paired wrap contributes two events; each actual planned block two points.
            // An interval update visits at most four nodes per tree level and one root;
            // each successful overflow search follows one root-to-leaf branch.
            let events = 2 * n + 2 * work.planner_blocks;
            let height = (events - 1).ilog2() as usize + 1;
            let node_bound = (n + removed) * (4 * height + 1) + removed * (height + 1);
            assert!(
                work.interval_steps <= node_bound,
                "action-sensitive node bound: n={n}, k={removed}, m={events}, h={height}, bound={node_bound}, {work:?}"
            );
            assert_eq!(work.dependent_visits, 0, "wrap scopes have no inline promotion dependents");
            assert_eq!(nesting_peak(&body, &BTreeMap::new()), room);
            eprintln!(
                "implicit scope work: cells={cells},room={room},n={n},k={removed},m={events},h={height},plan={steps},nodes={},bound={node_bound}",
                work.interval_steps
            );
            steps + work.interval_steps as u64
        };
        for cells in [false, true] {
            for depth in [128, 1024] {
                scope_work(cells, depth, 64);
            }
            let small = scope_work(cells, 128, 0);
            let large = scope_work(cells, 1024, 0);
            assert!(large <= small * 12, "8x opens and removals bound actual pairing and interval visits: {small} -> {large}");
        }

        // A protected deep region elsewhere is irrelevant to a separate valid pasted sibling.
        let mut body = vec![Arc::new(Block::Para(protected_paragraph())), Arc::new(Block::Para(Paragraph::new()))];
        let Block::Para(last) = Arc::make_mut(&mut body[1]) else { panic!("paragraph") };
        last.insert_object(0, marker(9000, false), &CharProps::default()).unwrap();
        last.insert_text(last.len(), "keep", &CharProps::default()).unwrap();
        last.insert_object(last.len(), InlineObject::ControlEnd, &CharProps::default()).unwrap();
        let before = body.clone();
        assert_eq!(
            trim_stream(
                &mut body,
                0,
                64,
                &|point| point.path == Path::top(1),
                &BTreeSet::new(),
                &mut NestingStories::new(&BTreeMap::new()),
                &mut BTreeMap::new(),
                None
            ),
            0
        );
        assert_eq!(body, before);
    }
}
