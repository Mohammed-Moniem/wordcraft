use std::collections::BTreeMap;
use std::sync::Arc;

use crate::control::{self, ContentControl, ControlKind, ControlLock, MAX_CONTROL_NESTING};
use crate::edit::Fragment;
use crate::para::{OBJ, ShapeKind};
use crate::props::CharProps;
use crate::{Block, Blocks, Document, InlineObject, Paragraph, Part, PartKind, Path, Pos, StoryRef, Table};

fn shape(id: u32) -> InlineObject {
    InlineObject::Shape {
        kind: ShapeKind::TextBox,
        w: 144.0,
        h: 72.0,
        fill: None,
        stroke: None,
        stroke_width: 0.75,
        float: Default::default(),
        story: Some(id),
        freeform: None,
        effects: Default::default(),
        extra: Default::default(),
    }
}

fn control(id: usize) -> ContentControl {
    ContentControl {
        id: Some(id as i64),
        tag: format!("tag-{id}"),
        title: format!("Title {id}"),
        lock: ControlLock::SdtLocked,
        placeholder: format!("Placeholder {id}"),
        kind: ControlKind::RichText,
        extra: vec![format!(r#"<w:dataBinding w:xpath="/root/item{id}" w:storeItemID="{{11111111-1111-1111-1111-111111111111}}"/>"#)],
        ..Default::default()
    }
}

fn wrapped(depth: usize, middle: Vec<InlineObject>) -> Blocks {
    let mut objects: Vec<_> = (0..depth).map(|i| InlineObject::ControlStart { control: Box::new(control(i)) }).collect();
    objects.extend(middle);
    objects.extend((0..depth).map(|_| InlineObject::ControlEnd));
    vec![Arc::new(Block::Para(super::para_of_objects(objects)))]
}

#[test]
fn nesting_shared_story_walk_is_bounded_by_input_not_path_expansion() {
    let fanout = 160;
    let mut d = Document::new();
    let mut inner = d.add_part(PartKind::TextBox, wrapped(1, Vec::new()));
    for _ in 0..3 {
        inner = d.add_part(PartKind::TextBox, wrapped(1, (0..fanout).map(|_| shape(inner)).collect()));
    }
    d.body = wrapped(1, (0..fanout).map(|_| shape(inner)).collect());
    control::take_nesting_work();
    assert_eq!(control::nesting_peak(&d.body, &d.parts), 5);
    let work = control::take_nesting_work();
    assert_eq!(work.story_scans, 4, "one scan per distinct story/hop: {work:?}");
    assert_eq!(work.cache_entries, 4);
    assert!(work.objects <= 5 * (fanout + 2), "no 160^4 expansion: {work:?}");
    assert!(work.blocks <= 5);
}

#[test]
fn nesting_counts_every_existing_paste_depth_including_beyond_four() {
    let mut d = Document::new();
    let mut inner = d.add_part(PartKind::TextBox, wrapped(1, Vec::new()));
    for _ in 1..crate::edit::MAX_BOX_DEPTH {
        inner = d.add_part(PartKind::TextBox, wrapped(1, vec![shape(inner)]));
    }
    d.body = wrapped(0, vec![shape(inner)]);
    assert_eq!(control::nesting_peak(&d.body, &d.parts), crate::edit::MAX_BOX_DEPTH);
}

#[test]
fn nesting_shared_story_is_trimmed_to_its_most_restrictive_anchor() {
    let mut d = Document::new();
    let shared = d.add_part(PartKind::TextBox, wrapped(30, Vec::new()));
    let outer = d.add_part(PartKind::TextBox, wrapped(50, vec![shape(shared)]));
    d.body = wrapped(0, vec![shape(shared), shape(outer)]);
    let (mut body, mut parts) = (d.body.clone(), d.parts.clone());
    control::take_nesting_work();
    assert_eq!(control::trim_nesting(&mut body, &mut parts, MAX_CONTROL_NESTING), 16);
    let work = control::take_nesting_work();
    assert!(work.trim_queue_max <= parts.len(), "deduplicated pending stories: {work:?}");
    assert!(work.trim_story_scans <= (MAX_CONTROL_NESTING + 1) * parts.len());
    assert_eq!(control::nesting_peak(&body, &parts), MAX_CONTROL_NESTING);
    let shared_part = &parts[&shared];
    let mut kept = Vec::new();
    control::walk_controls(&shared_part.blocks, &mut |c| kept.push(c.clone()));
    assert_eq!(kept, (0..14).map(control).collect::<Vec<_>>(), "all retained locks/tags/bindings unchanged");
    assert_eq!(d.parts[&shared].blocks, wrapped(30, Vec::new()), "source remains untouched");
}

#[test]
fn nesting_marker_filter_preserves_text_runs_noncontrol_objects_and_binding_bytes() {
    let mut p = Paragraph::new();
    for i in 0..4 {
        let props = CharProps { bold: Some(i % 2 == 0), ..Default::default() };
        p.insert_object(p.len(), InlineObject::ControlStart { control: Box::new(control(i)) }, &props).unwrap();
    }
    let bold = CharProps { bold: Some(true), ..Default::default() };
    let italic = CharProps { italic: Some(true), ..Default::default() };
    p.insert_text(p.len(), "café", &bold).unwrap();
    p.insert_object(p.len(), InlineObject::BookmarkStart { name: "keep-me".into() }, &italic).unwrap();
    p.insert_text(p.len(), " 🚀 tail", &italic).unwrap();
    for _ in 0..4 {
        p.insert_object(p.len(), InlineObject::ControlEnd, &bold).unwrap();
    }
    let mut expected = p.clone();
    let cuts: Vec<_> = p.objects_with_offsets().enumerate().filter(|(i, _)| [2, 3, 5, 6].contains(i)).map(|(_, (off, _))| off).collect();
    for off in cuts.into_iter().rev() {
        expected.delete(off, off + OBJ.len_utf8()).unwrap();
    }
    let mut blocks = vec![Arc::new(Block::Para(p))];
    assert_eq!(control::trim_nesting(&mut blocks, &mut BTreeMap::new(), 2), 2);
    assert_eq!(blocks, vec![Arc::new(Block::Para(expected))]);
    let mut d = Document::new();
    d.body = blocks;
    let bytes = serde_json::to_vec(&d).unwrap();
    let reopened: Document = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(reopened, d, "native model, formatting and opaque binding bytes survive");
    assert_eq!(d.plain_text(StoryRef::Body), "café 🚀 tail");
}

#[test]
fn nesting_marker_removal_has_linear_steps_and_input_bounded_auxiliary_storage() {
    let measure = |count: usize| {
        let mut body = wrapped(count, vec![InlineObject::BookmarkStart { name: "keep".into() }]);
        control::take_nesting_work();
        let mut removed = 0;
        let steps = crate::work::count(|| removed = control::trim_nesting(&mut body, &mut BTreeMap::new(), MAX_CONTROL_NESTING));
        let work = control::take_nesting_work();
        assert_eq!(removed, count - MAX_CONTROL_NESTING);
        assert_eq!(work.trim_objects, count * 2 + 1);
        assert!(work.marker_stack_max <= count);
        assert_eq!(work.trim_queue_max, 0);
        assert_eq!(control::nesting_peak(&body, &BTreeMap::new()), MAX_CONTROL_NESTING);
        steps
    };
    let small = measure(256);
    let large = measure(2048);
    assert!(large <= small * 9, "8x input must not take quadratic steps: {small} -> {large}");
    // A nearby valid fragment does not lose any marker, property or formatting.
    let mut ordinary = wrapped(16, Vec::new());
    let before = ordinary.clone();
    assert_eq!(control::trim_nesting(&mut ordinary, &mut BTreeMap::new(), MAX_CONTROL_NESTING), 0);
    assert_eq!(ordinary, before);
}

#[test]
fn nesting_table_row_and_cell_wraps_retain_their_exact_properties() {
    let mut table = Table::new(1, 1, 144.0);
    table.controls.open = vec![control(100), control(101)];
    table.controls.close = 2;
    table.rows[0].controls.open = vec![control(102), control(103)];
    table.rows[0].controls.close = 2;
    table.rows[0].cells[0].controls.open = vec![control(104), control(105)];
    table.rows[0].cells[0].controls.close = 2;
    table.rows[0].cells[0].blocks = wrapped(64, Vec::new());
    let mut body = vec![Arc::new(Block::Table(table.clone()))];
    assert_eq!(control::nesting_peak(&body, &BTreeMap::new()), 70);
    assert_eq!(control::trim_nesting(&mut body, &mut BTreeMap::new(), MAX_CONTROL_NESTING), 6);
    let Block::Table(after) = &*body[0] else { panic!("table retained") };
    assert_eq!(after.controls, table.controls);
    assert_eq!(after.rows[0].controls, table.rows[0].controls);
    assert_eq!(after.rows[0].cells[0].controls, table.rows[0].cells[0].controls);
    let mut kept = Vec::new();
    control::walk_controls(&after.rows[0].cells[0].blocks, &mut |c| kept.push(c.clone()));
    assert_eq!(kept, (0..58).map(control).collect::<Vec<_>>());
    assert_eq!(control::nesting_peak(&body, &BTreeMap::new()), MAX_CONTROL_NESTING);
}

#[test]
fn nesting_large_table_wrap_closes_do_not_orphan_retained_controls() {
    let mut table = Table::new(1, 1, 144.0);
    let count = 10_001;
    table.controls.open = (0..count).map(control).collect();
    table.controls.close = count as u32;
    let mut body = vec![Arc::new(Block::Table(table))];
    assert_eq!(control::trim_nesting(&mut body, &mut BTreeMap::new(), MAX_CONTROL_NESTING), count - MAX_CONTROL_NESTING);
    let Block::Table(after) = &*body[0] else { panic!("table retained") };
    assert_eq!(after.controls.open, (0..MAX_CONTROL_NESTING).map(control).collect::<Vec<_>>());
    assert_eq!(after.controls.close, MAX_CONTROL_NESTING as u32);
}

#[test]
fn nesting_cycles_and_deep_shared_ancestry_have_deterministic_work_bounds() {
    let mut d = Document::new();
    let a = d.add_part(PartKind::TextBox, wrapped(1, Vec::new()));
    let b = d.add_part(PartKind::TextBox, wrapped(1, vec![shape(a)]));
    d.parts.get_mut(&a).unwrap().blocks = wrapped(1, vec![shape(b)]);
    d.body = wrapped(0, vec![shape(a)]);
    control::take_nesting_work();
    let at = Pos { story: StoryRef::Part(a), path: Path::top(0), off: OBJ.len_utf8() };
    assert!(d.control_nesting(&at, &at) > MAX_CONTROL_NESTING);
    let work = control::take_nesting_work();
    assert!(work.story_scans <= (MAX_CONTROL_NESTING + 1) * d.parts.len());
    assert!(work.cache_entries <= (MAX_CONTROL_NESTING + 1) * d.parts.len());
    assert!(work.outer_edge_visits <= (MAX_CONTROL_NESTING + 2) * work.outer_edges);
    assert!(work.outer_queue_max <= d.parts.len() + 1);
    let (mut body, mut parts) = (d.body.clone(), d.parts.clone());
    control::take_nesting_work();
    assert_eq!(control::trim_nesting(&mut body, &mut parts, 0), 2);
    let work = control::take_nesting_work();
    assert!(work.trim_story_scans <= (MAX_CONTROL_NESTING + 1) * parts.len());
    assert!(work.trim_queue_max <= parts.len());
    assert_eq!(control::nesting_peak(&body, &parts), 0);

    // The edited story has many ancestors beyond the paste-copy depth; no recursive anchor walk.
    let mut deep = Document::new();
    let inner = deep.add_part(PartKind::TextBox, wrapped(1, Vec::new()));
    let mut next = inner;
    for _ in 0..40 {
        next = deep.add_part(PartKind::TextBox, wrapped(2, vec![shape(next)]));
    }
    deep.body = wrapped(0, vec![shape(next)]);
    let at = Pos { story: StoryRef::Part(inner), path: Path::top(0), off: OBJ.len_utf8() };
    control::take_nesting_work();
    assert!(deep.control_nesting(&at, &at) > MAX_CONTROL_NESTING);
    let work = control::take_nesting_work();
    assert!(work.outer_edge_visits <= (MAX_CONTROL_NESTING + 2) * work.outer_edges);
    assert!(work.outer_queue_max <= deep.parts.len() + 1);
}

#[test]
fn nesting_fragment_supplied_story_counts_its_missing_source_story_before_copying() {
    let mut d = Document::new();
    let inner = d.add_part(PartKind::TextBox, wrapped(10, Vec::new()));
    let original = d.parts[&inner].clone();
    d.body = wrapped(50, Vec::new());
    let outer = Part { kind: PartKind::TextBox, blocks: wrapped(20, vec![shape(inner)]), ..Default::default() };
    let fragment = Fragment { blocks: vec![Block::Para(super::para_of_objects(vec![shape(100)]))], parts: BTreeMap::from([(100, outer)]) };
    let before = fragment.clone();
    let at = Pos::body(0, 50 * OBJ.len_utf8());
    d.insert_fragment(&at, &fragment).unwrap();
    assert_eq!(control::nesting_peak(&d.body, &d.parts), MAX_CONTROL_NESTING);
    assert_eq!(d.parts[&inner], original, "source part's metadata and text are not trimmed in place");
    assert_eq!(fragment, before);
}

#[test]
fn nesting_query_normalizes_reversed_ranges_and_refuses_unknown_positions() {
    let mut d = Document::new();
    d.body = wrapped(3, Vec::new());
    let (a, b) = (Pos::body(0, 0), Pos::body(0, 6 * OBJ.len_utf8()));
    assert_eq!(d.control_nesting(&a, &b), 3);
    assert_eq!(d.control_nesting(&b, &a), 3);
    for bad in [Pos::body(0, 1), Pos::body(0, 1000), Pos::body(99, 0), Pos { story: StoryRef::Part(999), ..a.clone() }] {
        assert!(d.control_nesting(&bad, &bad) > MAX_CONTROL_NESTING, "unknown paths do not grant nesting room");
    }
    let missing = super::para_of_objects(vec![shape(999)]);
    d.body = vec![Arc::new(Block::Para(missing))];
    assert!(d.control_nesting(&Pos::body(0, 0), &Pos::body(0, OBJ.len_utf8())) > MAX_CONTROL_NESTING);
}

#[test]
fn nesting_edit_range_counts_imported_descendants_beyond_the_paste_copy_depth() {
    let mut d = Document::new();
    let mut inner = d.add_part(PartKind::TextBox, wrapped(2, Vec::new()));
    for _ in 1..40 {
        inner = d.add_part(PartKind::TextBox, wrapped(2, vec![shape(inner)]));
    }
    d.body = wrapped(0, vec![shape(inner)]);
    assert!(d.control_nesting(&Pos::body(0, 0), &Pos::body(0, OBJ.len_utf8())) > MAX_CONTROL_NESTING);
}

#[test]
fn nesting_invalid_paste_offsets_leave_the_complete_model_and_parts_unchanged() {
    let mut source = Document::new();
    let story = source.add_part(PartKind::TextBox, wrapped(70, Vec::new()));
    let fragment = Fragment { blocks: vec![Block::Para(super::para_of_objects(vec![shape(story)]))], parts: source.parts.clone() };
    let fragment_before = fragment.clone();
    for at in [Pos::body(0, 1), Pos::body(0, 1000), Pos::body(99, 0), Pos::new(StoryRef::Part(999), Path::top(0), 0)] {
        let mut d = Document::from_text("é seed");
        let before = d.clone();
        assert!(d.insert_fragment(&at, &fragment).is_err());
        assert_eq!(d, before, "invalid position cannot leave trimmed or adopted stories behind");
        assert_eq!(fragment, fragment_before);
    }
}

#[test]
fn nesting_empty_table_row_and_cell_wraps_are_counted_and_trimmed() {
    for level in 0..3 {
        let mut table = Table::new(1, 1, 144.0);
        let wrap = crate::control::ControlWrap { open: (0..70).map(control).collect(), close: 70 };
        match level {
            0 => {
                table.controls = wrap;
                table.rows.clear();
            }
            1 => {
                table.rows[0].controls = wrap;
                // A row needs a cell to be serialized; keep the no-paragraph case.
                table.rows[0].cells[0].blocks.clear();
            }
            _ => {
                table.rows[0].cells[0].controls = wrap;
                table.rows[0].cells[0].blocks.clear();
            }
        }
        let mut body = vec![Arc::new(Block::Table(table))];
        assert_eq!(control::nesting_peak(&body, &BTreeMap::new()), 70, "empty wrapper level {level}");
        assert_eq!(control::trim_nesting(&mut body, &mut BTreeMap::new(), MAX_CONTROL_NESTING), 6);
        assert_eq!(control::nesting_peak(&body, &BTreeMap::new()), MAX_CONTROL_NESTING);
        let Block::Table(table) = &*body[0] else { panic!("table retained") };
        let wrap = match level {
            0 => &table.controls,
            1 => &table.rows[0].controls,
            _ => &table.rows[0].cells[0].controls,
        };
        assert_eq!(wrap.open, (0..MAX_CONTROL_NESTING).map(control).collect::<Vec<_>>());
        assert_eq!(wrap.close, MAX_CONTROL_NESTING as u32);
    }
}

fn malformed_close_query(kind: usize) {
    if kind == 0 {
        let mut table = Table::new(3, 1, 144.0);
        table.rows[0].controls.open = (0..64).map(control).collect();
        table.rows[1].controls.open = vec![control(900)];
        table.rows[1].controls.close = u32::MAX;
        table.rows[1].cells.clear();
        table.rows[2].controls.close = 64;
        table.rows[2].cells[0].blocks = wrapped(1, Vec::new());
        let ignored = table.rows[1].controls.clone();
        let mut d = Document::new();
        d.body = vec![Arc::new(Block::Table(table))];
        let at = Pos::new(StoryRef::Body, Path(vec![0, 2, 0, 0]), 0);
        assert_eq!(d.control_nesting(&at, &at), 64, "empty-row close must not release serialized row wrappers");
        assert_eq!(control::nesting_peak(&d.body, &d.parts), 65);
        let mut body = d.body.clone();
        assert_eq!(control::trim_nesting(&mut body, &mut BTreeMap::new(), 64), 1);
        assert_eq!(control::nesting_peak(&body, &BTreeMap::new()), 64);
        let Block::Table(table) = &*body[0] else { panic!("table retained") };
        assert_eq!(table.rows[1].controls, ignored, "unserialized metadata is not edited by the guard/trim");
        assert_eq!(table.rows[0].controls.open, (0..64).map(control).collect::<Vec<_>>());
    }
    for depth in [63, 64] {
        let mut table = Table::new(if kind == 0 { 2 } else { 1 }, if kind == 1 { 2 } else { 1 }, 144.0);
        table.controls.open = (0..depth).map(control).collect();
        table.controls.close = depth as u32;
        for row in &mut table.rows {
            for cell in &mut row.cells {
                cell.blocks = Document::from_text("seed").body;
            }
        }
        let mut at = Pos::new(StoryRef::Body, Path(vec![0, 0, 0, 0]), 1);
        match kind {
            0 => {
                table.rows[0].controls.close = u32::MAX;
                at.path = Path(vec![0, 1, 0, 0]);
            }
            1 => {
                table.rows[0].cells[0].controls.close = u32::MAX;
                at.path = Path(vec![0, 0, 1, 0]);
            }
            2 => {
                let Block::Para(p) = Arc::make_mut(&mut table.rows[0].cells[0].blocks[0]) else { panic!("paragraph") };
                p.insert_object(0, InlineObject::ControlEnd, &CharProps::default()).unwrap();
                at.off += OBJ.len_utf8();
            }
            _ => {
                let mut inner = Table::new(1, 1, 72.0);
                inner.controls.close = u32::MAX;
                table.rows[0].cells[0].blocks.insert(0, Arc::new(Block::Table(inner)));
                at.path = Path(vec![0, 0, 0, 1]);
            }
        }
        let mut d = Document::new();
        d.body = vec![Arc::new(Block::Table(table))];
        let before = d.clone();
        assert_eq!(d.control_nesting(&at, &at), depth, "orphan close kind {kind}");
        assert_eq!(control::nesting_peak(&d.body, &d.parts), depth);
        assert_eq!(d, before, "a query preserves malformed imported data");
    }
    // Nearby valid same-level wraps may span sibling rows/cells; their matching close
    // releases only their own level while the 63 enclosing table controls remain active.
    if kind <= 1 {
        let mut table = Table::new(if kind == 0 { 3 } else { 1 }, if kind == 1 { 3 } else { 1 }, 144.0);
        table.controls.open = (0..63).map(control).collect();
        table.controls.close = 63;
        for row in &mut table.rows {
            for cell in &mut row.cells {
                cell.blocks = Document::from_text("seed").body;
            }
        }
        let (inside, after) = if kind == 0 {
            table.rows[0].controls.open = vec![control(100)];
            table.rows[1].controls.close = 1;
            (Path(vec![0, 1, 0, 0]), Path(vec![0, 2, 0, 0]))
        } else {
            table.rows[0].cells[0].controls.open = vec![control(100)];
            table.rows[0].cells[1].controls.close = 1;
            (Path(vec![0, 0, 1, 0]), Path(vec![0, 0, 2, 0]))
        };
        let mut d = Document::new();
        d.body = vec![Arc::new(Block::Table(table))];
        let inside = Pos::new(StoryRef::Body, inside, 1);
        let after = Pos::new(StoryRef::Body, after, 1);
        assert_eq!(d.control_nesting(&inside, &inside), 64);
        assert_eq!(d.control_nesting(&after, &after), 63);
    }
}

#[test]
fn nesting_orphan_row_close_cannot_release_table_room() {
    malformed_close_query(0);
}

#[test]
fn nesting_orphan_cell_close_cannot_release_row_or_table_room() {
    malformed_close_query(1);
}

#[test]
fn nesting_orphan_inline_end_cannot_release_inherited_room() {
    malformed_close_query(2);
}

#[test]
fn nesting_orphan_table_close_cannot_release_parent_cell_room() {
    malformed_close_query(3);
}

#[test]
fn nesting_empty_path_alias_matches_the_resolved_paragraph() {
    let mut d = Document::new();
    d.body = wrapped(64, Vec::new());
    let part = d.add_part(PartKind::TextBox, d.body.clone());
    for story in [StoryRef::Body, StoryRef::Part(part)] {
        let alias = Pos::new(story, Path(Vec::new()), 64 * OBJ.len_utf8());
        let canonical = Pos::new(story, Path::top(0), alias.off);
        assert_eq!(d.para_at(&alias), d.para_at(&canonical));
        assert_eq!(d.control_nesting(&alias, &alias), 64);
        assert_eq!(d.control_nesting(&canonical, &canonical), 64);
    }
}

#[test]
fn nesting_empty_path_range_uses_resolved_endpoint_order() {
    let mut d = Document::new();
    d.body = wrapped(64, Vec::new());
    let Block::Para(p) = Arc::make_mut(&mut d.body[0]) else { panic!("paragraph") };
    p.insert_text(64 * OBJ.len_utf8(), "seed", &CharProps::default()).unwrap();
    let a = Pos::new(StoryRef::Body, Path(Vec::new()), 64 * OBJ.len_utf8());
    let b = Pos::new(StoryRef::Body, Path(Vec::new()), a.off + 4);
    assert_eq!(d.control_nesting(&a, &b), 64);
    assert_eq!(d.control_nesting(&b, &a), 64);
    let canonical = Pos::body(0, b.off);
    assert_eq!(d.control_nesting(&a, &canonical), 64);
    assert_eq!(d.control_nesting(&canonical, &a), 64);
}

fn promoted_sibling(depth: usize) -> Document {
    let mut d = Document::new();
    d.body = wrapped(depth, Vec::new());
    let Block::Para(first) = Arc::make_mut(&mut d.body[0]) else { panic!("paragraph") };
    first.insert_text(depth * OBJ.len_utf8(), "old", &CharProps { bold: Some(true), ..Default::default() }).unwrap();
    first.insert_text(first.len(), " boundary ", &CharProps::default()).unwrap();
    let mut wrapper = control(900);
    wrapper.block = true;
    first.insert_object(first.len(), InlineObject::ControlStart { control: Box::new(wrapper) }, &CharProps::default()).unwrap();
    first.insert_text(first.len(), "first", &CharProps::default()).unwrap();
    let mut last = Paragraph::new();
    last.insert_text(0, "last", &CharProps::default()).unwrap();
    last.insert_object(last.len(), InlineObject::ControlEnd, &CharProps::default()).unwrap();
    d.body.push(Arc::new(Block::Para(last)));
    d
}

fn block_fragment() -> Fragment {
    let mut first = Paragraph::new();
    let mut wrapper = control(900);
    wrapper.block = true;
    first.insert_object(0, InlineObject::ControlStart { control: Box::new(wrapper) }, &CharProps::default()).unwrap();
    first.insert_text(first.len(), "left", &CharProps::default()).unwrap();
    let mut last = Paragraph::new();
    last.insert_text(0, "right", &CharProps::default()).unwrap();
    last.insert_object(last.len(), InlineObject::ControlEnd, &CharProps::default()).unwrap();
    Fragment { blocks: vec![Block::Para(first), Block::Para(last)], parts: BTreeMap::new() }
}

#[test]
fn nesting_effective_block_promotion_counts_controls_before_raw_start() {
    for depth in [63, 62] {
        let d = promoted_sibling(depth);
        let before = d.clone();
        let at = Pos::body(0, depth * OBJ.len_utf8() + 1);
        assert_eq!(d.control_nesting(&at, &at), depth + 1, "whole-paragraph placement includes the earlier inline controls");
        assert_eq!(control::nesting_peak(&d.body, &d.parts), depth + 1);
        assert_eq!(d, before, "effective placement remains a read-only query");
    }
}

#[test]
fn nesting_effective_trim_counts_promoted_sibling_ranges() {
    for depth in [64, 63] {
        let d = promoted_sibling(depth);
        let mut body = d.body.clone();
        assert_eq!(control::nesting_peak(&body, &d.parts), depth + 1);
        assert_eq!(control::trim_nesting(&mut body, &mut BTreeMap::new(), 64), usize::from(depth == 64));
        assert_eq!(control::nesting_peak(&body, &d.parts), 64);
        let mut kept = Vec::new();
        control::walk_controls(&body, &mut |c| kept.push(c.clone()));
        let mut expected: Vec<_> = (0..63).map(control).collect();
        let mut wrapper = control(900);
        wrapper.block = true;
        expected.push(wrapper);
        assert_eq!(kept, expected, "only the excess new pair is removed, with all surviving properties exact");
        if depth == 63 {
            assert_eq!(body, d.body, "nearby valid placement is a complete no-op");
        }
        let mut result = d.clone();
        result.body = body;
        assert_eq!(result.plain_text(StoryRef::Body), d.plain_text(StoryRef::Body));
    }
}

#[test]
fn nesting_paste_block_promotion_protects_both_destination_endpoints() {
    let fragment = block_fragment();
    for before in [false, true] {
        for depth in [64, 63] {
            let mut d = Document::new();
            d.body = wrapped(depth, Vec::new());
            let Block::Para(p) = Arc::make_mut(&mut d.body[0]) else { panic!("paragraph") };
            p.insert_text(depth * OBJ.len_utf8(), "old", &CharProps { italic: Some(true), ..Default::default() }).unwrap();
            let at = Pos::body(0, if before { 0 } else { p.len() });
            let source = fragment.clone();
            let end = d.insert_fragment(&at, &fragment).unwrap();
            let mut kept = Vec::new();
            control::walk_controls(&d.body, &mut |c| kept.push(c.clone()));
            assert_eq!(kept.len(), 64, "promotion at endpoint before={before}, old depth={depth}");
            for id in 0..depth {
                assert!(kept.contains(&control(id)), "destination metadata is protected");
            }
            assert_eq!(kept.iter().any(|c| c.tag == "tag-900"), depth == 63);
            assert_eq!(control::nesting_peak(&d.body, &d.parts), 64);
            assert_eq!(d.plain_text(StoryRef::Body), if before { "left\nrightold" } else { "oldleft\nright" });
            let p = d.para_at(&end).unwrap();
            assert!(end.off <= p.len() && p.text.is_char_boundary(end.off), "returned paste position tracks removed markers");
            assert_eq!(fragment, source);
        }
    }
}

#[test]
fn nesting_paste_block_promotion_protects_existing_shared_shape_stories() {
    for depth in [64, 63] {
        let mut d = Document::new();
        let shared = d.add_part(PartKind::TextBox, wrapped(depth, Vec::new()));
        let original = d.parts[&shared].clone();
        d.body = vec![
            Arc::new(Block::Para(super::para_of_objects(vec![shape(shared)]))),
            Arc::new(Block::Para(super::para_of_objects(vec![shape(shared)]))),
        ];
        let unaffected = d.body[1].clone();
        let fragment = block_fragment();
        d.insert_fragment(&Pos::body(0, OBJ.len_utf8()), &fragment).unwrap();
        let mut pasted = Vec::new();
        control::walk_controls(&d.body, &mut |c| pasted.push(c.clone()));
        assert_eq!(pasted.len(), usize::from(depth == 63), "the promoted wrapper reserves the existing story's full depth");
        assert_eq!(d.parts[&shared], original, "shared destination story is never trimmed");
        assert_eq!(d.body[2], unaffected, "the separate shallow anchor is preserved");
        assert_eq!(control::nesting_peak(&d.body, &d.parts), 64);
    }
}
