//! Developer › Controls: content controls (see [`wordcraft_doc::control`]) — insert each kind,
//! set their properties, tick check boxes, choose list entries and dates, Design Mode.
//!
//! Typing into a control that shows its placeholder replaces the placeholder; deleting all of a
//! control's content brings the placeholder back; locked controls refuse to be deleted
//! (`sdtLocked`) or edited (`contentLocked`). Those rules live here and are called from the
//! typing and deleting commands.

use serde_json::{Value, json};
use std::sync::Arc;
use wordcraft_doc::control::{
    ContentControl, ControlKind, ControlLock, ControlRange, ListItem, MAX_CONTROL_NESTING, MAX_CONTROL_TEXT, MAX_LIST_ITEMS,
};
use wordcraft_doc::para::OBJ;
use wordcraft_doc::props::{CharProps, Rgb, TextColor};
use wordcraft_doc::styles::{Style, StyleKind};
use wordcraft_doc::{InlineObject, Path, Pos, StoryRef};

use super::{delete_selection, sel_result};
use crate::{CmdError, CmdResult, CommandSpec, Selection, Session, p};

const LOC: &str = "Developer › Controls";
/// The character style placeholder text is shown in.
pub const PLACEHOLDER_STYLE: &str = "PlaceholderText";

pub fn specs() -> Vec<CommandSpec> {
    const INSERT: &str = r#"{"title"?: string, "tag"?: string}"#;
    const LIST: &str = r#"{"title"?: string, "tag"?: string, "items"?: [string | {"display": string, "value"?: string}]}"#;
    vec![
        CommandSpec::new("developer.richText", "Rich Text Content Control", LOC, |s, v| insert(s, v, ControlKind::RichText)).params(INSERT),
        CommandSpec::new("developer.plainText", "Plain Text Content Control", LOC, |s, v| insert(s, v, ControlKind::Text { multi_line: false }))
            .params(INSERT),
        CommandSpec::new("developer.picture", "Picture Content Control", LOC, |s, v| insert(s, v, ControlKind::Picture)).params(INSERT),
        CommandSpec::new("developer.buildingBlock", "Building Block Gallery Content Control", LOC, |s, v| {
            insert(s, v, ControlKind::Gallery { list: true, gallery: "Quick Parts".into(), category: "General".into(), unique: false })
        })
        .params(INSERT),
        CommandSpec::new("developer.checkBox", "Check Box Content Control", LOC, |s, v| insert(s, v, ControlKind::check_box()))
            .params(r#"{"title"?: string, "tag"?: string, "checked"?: bool}"#),
        CommandSpec::new("developer.comboBox", "Combo Box Content Control", LOC, |s, v| {
            insert(s, v, ControlKind::ComboBox { items: Vec::new(), last_value: String::new() })
        })
        .params(LIST),
        CommandSpec::new("developer.dropDown", "Drop-Down List Content Control", LOC, |s, v| {
            insert(s, v, ControlKind::DropDown { items: Vec::new(), last_value: String::new() })
        })
        .params(LIST),
        CommandSpec::new("developer.datePicker", "Date Picker Content Control", LOC, |s, v| {
            insert(
                s,
                v,
                ControlKind::Date { full_date: String::new(), format: "M/d/yyyy".into(), lid: "en-US".into(), calendar: "gregorian".into(), store_as: "dateTime".into() },
            )
        })
        .params(r#"{"title"?: string, "tag"?: string, "format"?: "M/d/yyyy", "date"?: "YYYY-MM-DD"}"#),
        CommandSpec::new("developer.repeatingSection", "Repeating Section Content Control", LOC, |s, v| {
            insert(s, v, ControlKind::RepeatingSection { title: String::new(), no_insert_delete: false })
        })
        .params(INSERT),
        CommandSpec::new("developer.designMode", "Design Mode", LOC, design_mode).params(r#"{"value"?: bool}"#).pure(),
        CommandSpec::new("developer.properties", "Properties", LOC, properties).params(
            r#"{"control"?: tag or title, "title"?: string, "tag"?: string, "lockDelete"?: bool, "lockEdit"?: bool, "temporary"?: bool, "multiLine"?: bool, "placeholder"?: string, "items"?: [string | {"display", "value"}], "format"?: "M/d/yyyy", "checkedSymbol"?: char, "uncheckedSymbol"?: char}"#,
        ),
        CommandSpec::new("developer.control", "Content Control at Selection", LOC, info).params(r#"{"control"?: tag or title, "at"?: pos}"#).pure(),
        CommandSpec::new("developer.toggleCheckBox", "Toggle Check Box", LOC, toggle_check_box).params(r#"{"control"?: tag or title, "at"?: pos, "value"?: bool}"#),
        CommandSpec::new("developer.chooseItem", "Choose List Item", LOC, choose_item)
            .params(r#"{"control"?: tag or title, "at"?: pos, "index"?: number, "value"?: string, "text"?: string}"#),
        CommandSpec::new("developer.setDate", "Set Date", LOC, set_date).params(r#"{"control"?: tag or title, "at"?: pos, "date": "YYYY-MM-DD"}"#),
        CommandSpec::new("developer.removeControl", "Remove Content Control", LOC, remove_control).params(r#"{"control"?: tag or title, "at"?: pos}"#),
    ]
}

fn locked(s: &mut Session, msg: &str) -> CmdError {
    s.status = msg.to_string();
    CmdError::Failed(msg.to_string())
}

const CANT_DELETE: &str = "This content control can't be deleted.";
const CANT_EDIT: &str = "The contents of this content control can't be edited.";

/// A string param, trimmed to what a control keeps.
fn short(v: &Value, k: &str) -> Option<String> {
    p::str(v, k).map(|t| t.chars().filter(|c| !c.is_control() && *c != OBJ).take(MAX_CONTROL_TEXT).collect())
}

/// List entries from `items`: strings, or `{display, value}` objects.
fn items_param(v: &Value) -> Option<Vec<ListItem>> {
    let arr = v.get("items")?.as_array()?;
    let clean = |t: &str| -> String { t.chars().filter(|c| !c.is_control() && *c != OBJ).take(MAX_CONTROL_TEXT).collect() };
    Some(
        arr.iter()
            .take(MAX_LIST_ITEMS)
            .filter_map(|i| match i {
                Value::String(t) => Some(ListItem { display: clean(t), value: clean(t) }),
                Value::Object(_) => {
                    let display = clean(i.get("display").and_then(Value::as_str)?);
                    let value = i.get("value").and_then(Value::as_str).map(clean).unwrap_or_else(|| display.clone());
                    Some(ListItem { display, value })
                }
                _ => None,
            })
            .filter(|i| !i.display.is_empty())
            .collect(),
    )
}

/// A symbol param: one character, not a control or object character.
fn symbol_param(v: &Value, k: &str) -> Option<char> {
    let t = p::str(v, k)?;
    let mut it = t.chars();
    let c = it.next()?;
    (it.next().is_none() && !c.is_control() && c != OBJ).then_some(c)
}

/// A date `YYYY-MM-DD` (a full ISO date-time is fine too): (year, month, day), checked.
pub fn parse_date(t: &str) -> Option<(i64, u32, u32)> {
    let t = t.trim();
    // Digits only: `str::parse` would also accept a leading '+'.
    let field = |r: std::ops::Range<usize>| t.get(r).filter(|s| s.bytes().all(|b| b.is_ascii_digit()));
    let y: i64 = field(0..4)?.parse().ok()?;
    let m: u32 = field(5..7)?.parse().ok()?;
    let d: u32 = field(8..10)?.parse().ok()?;
    if t.get(4..5) != Some("-") || t.get(7..8) != Some("-") || !(1..=9999).contains(&y) || !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m)
    {
        return None;
    }
    Some((y, m, d))
}

pub fn days_in_month(y: i64, m: u32) -> u32 {
    // The length of a month is the day before the first of the next month. Month lengths repeat
    // every 400 years, so the year within its Gregorian cycle (`rem_euclid(400)`) is enough and
    // always fits chrono, for any `y`. An invalid month keeps the old 31-day default.
    let y = y.rem_euclid(400);
    let first = |y: i64, m: u32| i32::try_from(y).ok().and_then(|y| chrono::NaiveDate::from_ymd_opt(y, m, 1));
    match (first(y, m), if m == 12 { first(y + 1, 1) } else { first(y, m + 1) }) {
        (Some(a), Some(b)) => u32::try_from((b - a).num_days()).unwrap_or(31),
        _ => 31,
    }
}

/// Days since 1970-01-01 for a date (inverse of [`super::civil_from_days`]), exact for any year.
///
/// The Gregorian calendar repeats every 400 years (146_097 days): chrono converts the year
/// within its cycle and whole cycles are added back as days (an i128 so the extremes can't
/// overflow). 1970-01-01 is `num_days_from_ce` 719_163; the result matches the proleptic
/// Gregorian count the old hand-rolled code gave.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let (cycles, year) = (i128::from(y.div_euclid(400)), y.rem_euclid(400));
    i32::try_from(year)
        .ok()
        .and_then(|year| chrono::NaiveDate::from_ymd_opt(year, m, d))
        .and_then(|date| {
            let days = i128::from(chrono::Datelike::num_days_from_ce(&date)) - 719_163 + cycles * 146_097;
            i64::try_from(days).ok()
        })
        .unwrap_or(0)
}

/// The built-in name of the placeholder style. Its id is localized: German Word writes
/// `Platzhaltertext`, English `PlaceholderText`.
const PLACEHOLDER_STYLE_NAME: &str = "Placeholder Text";

/// The id of the document's placeholder style: the one named "Placeholder Text", else the
/// English id.
fn placeholder_style_id(doc: &wordcraft_doc::Document) -> String {
    doc.styles
        .styles
        .iter()
        .find(|st| st.kind == StyleKind::Character && st.name.eq_ignore_ascii_case(PLACEHOLDER_STYLE_NAME))
        .map(|st| st.id.clone())
        .unwrap_or_else(|| PLACEHOLDER_STYLE.into())
}

/// The placeholder style, added to the document when it hasn't one.
fn ensure_placeholder_style(s: &mut Session) {
    if s.doc.styles.get(&placeholder_style_id(&s.doc)).is_none() {
        Arc::make_mut(&mut s.doc.styles).styles.push(Style {
            id: PLACEHOLDER_STYLE.into(),
            name: PLACEHOLDER_STYLE_NAME.into(),
            kind: StyleKind::Character,
            based_on: Some("DefaultParagraphFont".into()),
            chr: CharProps { color: Some(TextColor::Rgb(Rgb(0x66, 0x66, 0x66))), ..Default::default() },
            priority: Some(99),
            builtin: true,
            ..Default::default()
        });
    }
}

/// Formatting for placeholder text based on `base`.
fn placeholder_props(s: &mut Session, base: &CharProps) -> CharProps {
    ensure_placeholder_style(s);
    CharProps { style: Some(placeholder_style_id(&s.doc)), link: None, ..base.clone() }
}

/// `base` without the placeholder style (for real content), whatever its localized id.
fn content_props(doc: &wordcraft_doc::Document, base: &CharProps) -> CharProps {
    let mut p = base.clone();
    if p.style.as_deref().is_some_and(|id| id == PLACEHOLDER_STYLE || id == placeholder_style_id(doc)) {
        p.style = None;
    }
    p
}

/// An id no control in the document has.
fn new_id(s: &Session) -> i64 {
    let mut used = std::collections::BTreeSet::new();
    let mut add = |bl: &wordcraft_doc::Blocks| {
        wordcraft_doc::control::walk_controls(bl, &mut |c| {
            if let Some(id) = c.id {
                used.insert(id);
            }
        })
    };
    add(&s.doc.body);
    for p in s.doc.parts.values() {
        add(&p.blocks);
    }
    let mut id = 1_000_000 + used.len() as i64;
    while used.contains(&id) {
        id += 1;
    }
    id
}

// ---------------------------------------------------------------------------------------------
// Finding the control a command is about

/// The control at `p`, also when `p` is just outside its markers (a click beside a check box).
fn control_near(s: &Session, at: &Pos) -> Option<ControlRange> {
    if let Some(r) = s.doc.control_at(at) {
        return Some(r);
    }
    let para = s.doc.para_at(at)?;
    let w = OBJ.len_utf8();
    if matches!(para.object_at(at.off), Some(InlineObject::ControlStart { .. })) {
        return s.doc.control_at(&Pos { off: at.off + w, ..at.clone() });
    }
    if at.off >= w && matches!(para.object_at(at.off - w), Some(InlineObject::ControlEnd)) {
        return s.doc.control_at(&Pos { off: at.off - w, ..at.clone() });
    }
    None
}

/// The control a command acts on: `control` (a tag or title) in the caret's story, else the one
/// at `at` (a position), else the one at the caret.
fn target(s: &Session, v: &Value) -> Result<ControlRange, CmdError> {
    if let Some(name) = p::str(v, "control") {
        let story = s.sel.focus.story;
        let stories = std::iter::once(story).chain(std::iter::once(StoryRef::Body));
        for st in stories {
            if let Some(r) =
                s.doc.control_ranges(st).into_iter().filter(|r| r.control.tag == name || r.control.title == name).min_by(|a, b| a.start.cmp(&b.start))
            {
                return Ok(r);
            }
        }
        return Err(CmdError::Failed(format!("no content control is called {name:?}")));
    }
    let at = v.get("at").and_then(super::parse_pos).unwrap_or_else(|| s.sel.focus.clone());
    control_near(s, &at).ok_or_else(|| CmdError::Disabled("the selection isn't in a content control".into()))
}

/// Replace a control's content with `text` (one paragraph's worth) in `props`; returns the end.
fn set_content(s: &mut Session, r: &ControlRange, text: &str, props: &CharProps) -> Result<Pos, CmdError> {
    let a = r.content_start();
    s.doc.delete_range(&a, &r.end)?;
    let end = s.doc.insert_text(&a, text, props)?;
    Ok(end)
}

/// The formatting of a control's content (its first character's), without placeholder styling.
fn content_base(s: &Session, r: &ControlRange) -> CharProps {
    let a = r.content_start();
    let props = s
        .doc
        .para_at(&a)
        .map(|p| if r.is_empty() { p.props_of_char(r.start.off).clone() } else { p.props_of_char(a.off).clone() })
        .unwrap_or_default();
    content_props(&s.doc, &props)
}

// ---------------------------------------------------------------------------------------------
// Rules for typing and deleting

/// Before text goes in at the selection: refuse what a locked or list-only control doesn't
/// take; a control showing its placeholder gets the placeholder selected (so it is replaced)
/// and stops showing it. Returns the formatting the text should get, when it changes.
pub fn prepare_edit(s: &mut Session) -> Result<Option<CharProps>, CmdError> {
    if s.sel.is_collapsed() {
        let at = super::protect::typing_pos(s, &s.doc.typing_pos(&s.sel.focus));
        if at != s.sel.focus {
            s.sel = Selection::caret(at);
        }
    }
    // A caret at the very edge of a control that takes no typing types beside it instead.
    for _ in 0..16 {
        if !s.sel.is_collapsed() {
            break;
        }
        let f = s.sel.focus.clone();
        let Some(r) = s.doc.control_at(&f) else { break };
        if r.control.lock.can_edit() && r.control.kind.takes_typing() {
            break;
        }
        let w = OBJ.len_utf8();
        if f == r.end {
            s.sel = Selection::caret(Pos { off: r.end.off + w, ..r.end.clone() });
        } else if f == r.content_start() {
            s.sel = Selection::caret(r.start.clone());
        } else {
            break;
        }
    }
    let (a, b) = s.sel.ordered();
    let Some(r) = s.doc.control_at(&a) else { return Ok(None) };
    if !r.control.lock.can_edit() {
        return Err(locked(s, CANT_EDIT));
    }
    if !r.control.kind.takes_typing() {
        let msg = match r.control.kind {
            ControlKind::CheckBox { .. } => "Click the check box (or use Toggle Check Box) to change it.",
            ControlKind::DropDown { .. } => "Choose an entry from the list.",
            _ => "This content control holds a picture.",
        };
        return Err(locked(s, msg));
    }
    if !r.control.showing_placeholder || !r.contains(&b) {
        return Ok(None);
    }
    // Text typed in place of the placeholder takes the control's own run properties (Word, and
    // ECMA-376 §17.5.2.27), else the content's formatting without the placeholder style. The
    // showing placeholder's own character style is that style, whatever a producer called it
    // (Word localizes only the id; another program may localize the name too).
    let props = match &r.control.run_props {
        Some(rp) => content_props(&s.doc, rp),
        None => CharProps { style: None, ..content_base(s, &r) },
    };
    s.sel = Selection { anchor: r.content_start(), focus: r.end.clone() };
    if let Some(c) = s.doc.control_mut(&r.start) {
        c.showing_placeholder = false;
    }
    Ok(Some(props))
}

/// Enter in a one-line plain text control does nothing.
pub fn refuses_paragraph(s: &Session) -> bool {
    s.doc.control_at(&s.sel.focus).is_some_and(|r| {
        matches!(r.control.kind, ControlKind::Text { multi_line: false } | ControlKind::CheckBox { .. } | ControlKind::DropDown { .. })
    })
}

/// Before a deletion of `a..b`: refuse deleting a control that can't be deleted, or editing
/// content that can't be edited.
pub fn check_delete(s: &mut Session, a: &Pos, b: &Pos) -> Result<(), CmdError> {
    check_deletions(s, a.story, std::iter::once((a, b)))
}

/// Check a batch before editing, reading the story's control ranges only once. An empty
/// control list needs no per-match work, including for large Replace All operations.
pub fn check_deletions<'a>(s: &mut Session, story: StoryRef, deletions: impl IntoIterator<Item = (&'a Pos, &'a Pos)>) -> Result<(), CmdError> {
    let controls = s.doc.control_ranges(story);
    if controls.is_empty() {
        return Ok(());
    }
    let w = OBJ.len_utf8();
    for (a, b) in deletions {
        if a.story != story || b.story != story {
            continue;
        }
        for r in &controls {
            let whole = (&a.path, a.off) <= (&r.start.path, r.start.off) && (&r.end.path, r.end.off + w) <= (&b.path, b.off);
            let cs = r.content_start();
            let touches = (&a.path, a.off) < (&r.end.path, r.end.off) && (&b.path, b.off) > (&cs.path, cs.off);
            if whole && !r.control.lock.can_delete() {
                return Err(locked(s, CANT_DELETE));
            }
            if !whole && touches && !r.control.lock.can_edit() {
                return Err(locked(s, CANT_EDIT));
            }
        }
    }
    Ok(())
}

/// Before an edit that rewrites the text of `a..b` in place (Change Case, unlinking a field):
/// refuse when it touches the content of a control that can't be edited. Unlike a deletion,
/// the control itself stays, so a control locked only against deletion takes the edit.
pub fn check_edit(s: &mut Session, a: &Pos, b: &Pos) -> Result<(), CmdError> {
    if a.story != b.story {
        return Ok(());
    }
    let refused = s.doc.control_ranges(a.story).into_iter().any(|r| {
        let cs = r.content_start();
        !r.control.lock.can_edit() && (&a.path, a.off) < (&r.end.path, r.end.off) && (&b.path, b.off) > (&cs.path, cs.off)
    });
    if refused { Err(locked(s, CANT_EDIT)) } else { Ok(()) }
}

/// After the user deleted something: a control left empty shows its placeholder again.
pub fn restore_placeholder(s: &mut Session) -> Result<(), CmdError> {
    let at = s.sel.focus.clone();
    let Some(r) = s.doc.control_at(&at) else { return Ok(()) };
    if !r.is_empty() || r.control.showing_placeholder {
        return Ok(());
    }
    let text = r.control.placeholder_shown().to_string();
    if text.is_empty() {
        return Ok(());
    }
    let base = content_base(s, &r);
    let props = placeholder_props(s, &base);
    s.doc.insert_text(&r.content_start(), &text, &props)?;
    if let Some(c) = s.doc.control_mut(&r.start) {
        c.showing_placeholder = true;
    }
    s.sel = Selection::caret(r.content_start());
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Insert

fn check_nesting(s: &Session, a: &Pos, b: &Pos, adds: usize) -> Result<(), CmdError> {
    if s.doc.control_nesting(a, b).saturating_add(adds) > MAX_CONTROL_NESTING {
        return Err(CmdError::Failed(format!("content controls can't nest more than {MAX_CONTROL_NESTING} deep")));
    }
    Ok(())
}

fn insert(s: &mut Session, v: &Value, kind: ControlKind) -> CmdResult {
    let mut ctl = ContentControl::new(kind);
    ctl.id = Some(new_id(s));
    if let Some(t) = short(v, "title") {
        ctl.title = t;
    }
    if let Some(t) = short(v, "tag") {
        ctl.tag = t;
    }
    match &mut ctl.kind {
        ControlKind::ComboBox { items, .. } | ControlKind::DropDown { items, .. } => {
            if let Some(i) = items_param(v) {
                *items = i;
            }
        }
        ControlKind::Date { format, .. } => {
            if let Some(f) = short(v, "format").filter(|f| !f.trim().is_empty()) {
                *format = f;
            }
        }
        ControlKind::CheckBox { checked, .. } => *checked = p::bool(v, "checked").unwrap_or(false),
        _ => {}
    }
    // [] and [0] address the same paragraph. Resolve selection aliases before deciding
    // whether this is an inline range, a block range or a caret insertion/deletion.
    for pos in [&mut s.sel.anchor, &mut s.sel.focus] {
        if pos.path.0.is_empty() {
            pos.path = Path::top(0);
        }
    }
    let (a, b) = s.sel.ordered();
    if a.story != b.story || s.doc.para_at(&a).is_none() || s.doc.para_at(&b).is_none() {
        return Err(CmdError::Failed("no paragraph at the selection".into()));
    }
    if s.doc.control_at(&a).is_some_and(|r| !r.control.lock.can_edit()) {
        return Err(locked(s, CANT_EDIT));
    }
    let whole_para = a.path == b.path && a.off == 0 && s.doc.para_at(&b).is_some_and(|p| !p.is_empty() && b.off == p.len());
    let block = matches!(ctl.kind, ControlKind::RepeatingSection { .. } | ControlKind::Gallery { .. }) || a.path != b.path || whole_para;
    if block && matches!(ctl.kind, ControlKind::RichText | ControlKind::RepeatingSection { .. } | ControlKind::Gallery { .. }) {
        return insert_block(s, ctl, &a, &b);
    }
    // An inline control: around the selection (text and pictures), else at the caret with its
    // placeholder or value.
    let wraps = a != b && a.path == b.path && matches!(ctl.kind, ControlKind::RichText | ControlKind::Text { .. } | ControlKind::Picture);
    let base = s.doc.para_at(&a).map(|p| content_props(&s.doc, &p.props_of_char(a.off).clone())).unwrap_or_default();
    if wraps {
        // A newly constructed control wrapping existing text is not showing a placeholder.
        // Keep its default text implicit, as DOCX does; placeholder_shown retains the same
        // visible fallback. Imported/custom placeholder properties are never rewritten here.
        ctl.placeholder_text.clear();
        check_nesting(s, &a, &b, 1)?;
        s.doc.insert_object(&b, InlineObject::ControlEnd, &base)?;
        let start = a.clone();
        s.doc.insert_object(&start, InlineObject::ControlStart { control: Box::new(ctl) }, &base)?;
        let w = OBJ.len_utf8();
        s.sel = Selection { anchor: Pos { off: a.off + w, ..a.clone() }, focus: Pos { off: b.off + w, ..b.clone() } };
        return sel_result(s);
    }
    let at = delete_selection(s)?;
    let at = s.doc.typing_pos(&at);
    check_nesting(s, &at, &at, 1)?;
    let (text, props) = match &mut ctl.kind {
        ControlKind::CheckBox { checked, checked_char, unchecked_char, .. } => {
            ((if *checked { *checked_char } else { *unchecked_char }).to_string(), base.clone())
        }
        ControlKind::Date { full_date, format, .. } if p::str(v, "date").is_some() => {
            let (y, m, d) = p::str(v, "date").and_then(parse_date).ok_or_else(|| CmdError::Params("date: YYYY-MM-DD".into()))?;
            *full_date = format!("{y:04}-{m:02}-{d:02}T00:00:00Z");
            (format_date_value(format, y, m, d), base.clone())
        }
        _ => {
            ctl.showing_placeholder = true;
            let t = ctl.placeholder_shown().to_string();
            (t, placeholder_props(s, &base))
        }
    };
    let after_start = s.doc.insert_object(&at, InlineObject::ControlStart { control: Box::new(ctl) }, &base)?;
    let end = s.doc.insert_text(&after_start, &text, &props)?;
    s.doc.insert_object(&end, InlineObject::ControlEnd, &base)?;
    s.sel = Selection::caret(after_start);
    s.goal_x = None;
    sel_result(s)
}

/// A block-level control around the paragraphs from `a`'s to `b`'s (same block list).
fn insert_block(s: &mut Session, mut ctl: ContentControl, a: &Pos, b: &Pos) -> CmdResult {
    let story = a.story;
    // Paragraphs in the same block list as `a`'s, up to `b`'s.
    let last = if b.path.parent() == a.path.parent() { b.path.clone() } else { a.path.clone() };
    ctl.block = true;
    let empty = a.path == last && s.doc.para(story, &a.path).is_some_and(|p| p.is_empty());
    let base = s.doc.para(story, &a.path).map(|p| content_props(&s.doc, &p.props_of_char(0).clone())).unwrap_or_default();
    let item = matches!(ctl.kind, ControlKind::RepeatingSection { .. }).then(|| {
        let mut i = ContentControl::new(ControlKind::RepeatingSectionItem);
        i.block = true;
        i.id = ctl.id.map(|x| x + 1);
        i
    });
    let last_len = s.doc.para(story, &last).map(|p| p.len()).unwrap_or(0);
    check_nesting(s, &Pos::new(story, a.path.clone(), 0), &Pos::new(story, last.clone(), last_len), 1 + usize::from(item.is_some()))?;
    if empty && !ctl.placeholder_shown().is_empty() {
        ctl.showing_placeholder = true;
        let props = placeholder_props(s, &base);
        let text = ctl.placeholder_shown().to_string();
        s.doc.insert_text(&Pos::new(story, a.path.clone(), 0), &text, &props)?;
    }
    let end_off = s.doc.para(story, &last).map(|p| p.len()).unwrap_or(0);
    let end_props = s.doc.para(story, &last).map(|p| p.props_at(end_off).clone()).unwrap_or_default();
    let mut end = Pos::new(story, last.clone(), end_off);
    if item.is_some() {
        end = s.doc.insert_object(&end, InlineObject::ControlEnd, &end_props)?;
    }
    s.doc.insert_object(&end, InlineObject::ControlEnd, &end_props)?;
    let mut at = Pos::new(story, a.path.clone(), 0);
    at = s.doc.insert_object(&at, InlineObject::ControlStart { control: Box::new(ctl) }, &base)?;
    if let Some(i) = item {
        at = s.doc.insert_object(&at, InlineObject::ControlStart { control: Box::new(i) }, &base)?;
    }
    s.sel = Selection::caret(at);
    s.goal_x = None;
    sel_result(s)
}

// ---------------------------------------------------------------------------------------------
// Acting on a control

fn design_mode(s: &mut Session, v: &Value) -> CmdResult {
    s.view.design_mode = p::bool(v, "value").unwrap_or(!s.view.design_mode);
    Ok(json!({"designMode": s.view.design_mode}))
}

/// What a command or dialog needs to know about a control.
pub fn describe(s: &Session, r: &ControlRange) -> Value {
    let c = &r.control;
    let mut out = json!({
        "type": c.kind.name(),
        "title": c.title,
        "tag": c.tag,
        "id": c.id,
        "lockDelete": !c.lock.can_delete(),
        "lockEdit": !c.lock.can_edit(),
        "temporary": c.temporary,
        "block": c.block,
        "showingPlaceholder": c.showing_placeholder,
        "placeholder": c.placeholder_shown(),
        "text": if c.showing_placeholder { String::new() } else { s.doc.control_text(r) },
        "start": super::pos_json(&r.start),
        "end": super::pos_json(&r.end),
    });
    match &c.kind {
        ControlKind::Text { multi_line } => out["multiLine"] = json!(multi_line),
        ControlKind::CheckBox { checked, checked_char, unchecked_char, .. } => {
            out["checked"] = json!(checked);
            out["checkedSymbol"] = json!(checked_char.to_string());
            out["uncheckedSymbol"] = json!(unchecked_char.to_string());
        }
        ControlKind::ComboBox { items, last_value } | ControlKind::DropDown { items, last_value } => {
            out["items"] = json!(items.iter().map(|i| json!({"display": i.display, "value": i.value()})).collect::<Vec<_>>());
            out["value"] = json!(last_value);
        }
        ControlKind::Date { full_date, format, .. } => {
            out["date"] = json!(full_date.get(..10).unwrap_or(""));
            out["format"] = json!(format);
        }
        _ => {}
    }
    out
}

fn info(s: &mut Session, v: &Value) -> CmdResult {
    let r = target(s, v)?;
    Ok(describe(s, &r))
}

fn apply_kind_properties(c: &mut ContentControl, v: &Value) -> Option<String> {
    let mut content = None;
    match &mut c.kind {
        ControlKind::Text { multi_line } => {
            if let Some(m) = p::bool(v, "multiLine") {
                *multi_line = m;
            }
        }
        ControlKind::ComboBox { items, .. } | ControlKind::DropDown { items, .. } => {
            if let Some(i) = items_param(v) {
                *items = i;
            }
        }
        ControlKind::Date { format, full_date, .. } => {
            if let Some(f) = short(v, "format").filter(|f| !f.trim().is_empty()) {
                *format = f;
                // The shown date follows the new format.
                if let Some((y, m, d)) = parse_date(full_date)
                    && !c.showing_placeholder
                {
                    content = Some(format_date_value(format, y, m, d));
                }
            }
        }
        ControlKind::CheckBox { checked, checked_char, unchecked_char, .. } => {
            if let Some(ch) = symbol_param(v, "checkedSymbol") {
                *checked_char = ch;
            }
            if let Some(ch) = symbol_param(v, "uncheckedSymbol") {
                *unchecked_char = ch;
            }
            content = Some((if *checked { *checked_char } else { *unchecked_char }).to_string());
        }
        _ => {}
    }
    content
}

fn properties(s: &mut Session, v: &Value) -> CmdResult {
    let r = target(s, v)?;
    let mut c = r.control.clone();
    let mut content: Option<String> = None;
    if let Some(t) = short(v, "title") {
        c.title = t;
    }
    if let Some(t) = short(v, "tag") {
        c.tag = t;
    }
    let (no_delete, no_edit) = (p::bool(v, "lockDelete").unwrap_or(!c.lock.can_delete()), p::bool(v, "lockEdit").unwrap_or(!c.lock.can_edit()));
    c.lock = ControlLock::from_flags(no_delete, no_edit);
    if let Some(t) = p::bool(v, "temporary") {
        c.temporary = t;
    }
    if let Some(t) = short(v, "placeholder").filter(|t| !t.trim().is_empty()) {
        if c.showing_placeholder && t != c.placeholder_shown() {
            content = Some(t.clone());
        }
        c.placeholder_text = t;
    }
    if let Some(kc) = apply_kind_properties(&mut c, v) {
        content = Some(kc);
    }
    if let Some(text) = content {
        let base = content_base(s, &r);
        let props = if c.showing_placeholder { placeholder_props(s, &base) } else { base };
        set_content(s, &r, &text, &props)?;
    }
    if let Some(slot) = s.doc.control_mut(&r.start) {
        *slot = c;
    }
    match s.doc.control_at(&r.content_start()) {
        Some(r) => Ok(describe(s, &r)),
        None => sel_result(s),
    }
}

fn toggle_check_box(s: &mut Session, v: &Value) -> CmdResult {
    let r = target(s, v)?;
    let ControlKind::CheckBox { checked, checked_char, checked_font, unchecked_char, unchecked_font } = &r.control.kind else {
        return Err(CmdError::Disabled("the selection isn't in a check box".into()));
    };
    if !r.control.lock.can_edit() {
        return Err(locked(s, CANT_EDIT));
    }
    let now = p::bool(v, "value").unwrap_or(!checked);
    let (ch, font) = if now { (*checked_char, checked_font) } else { (*unchecked_char, unchecked_font) };
    let mut props = content_base(s, &r);
    if !font.is_empty() {
        props.font = Some(font.clone());
    }
    set_content(s, &r, &ch.to_string(), &props)?;
    if let Some(c) = s.doc.control_mut(&r.start) {
        if let ControlKind::CheckBox { checked, .. } = &mut c.kind {
            *checked = now;
        }
        c.showing_placeholder = false;
    }
    Ok(json!({"checked": now}))
}

fn choose_item(s: &mut Session, v: &Value) -> CmdResult {
    let r = target(s, v)?;
    let Some(items) = r.control.kind.items() else { return Err(CmdError::Disabled("the selection isn't in a combo box or drop-down list".into())) };
    if !r.control.lock.can_edit() {
        return Err(locked(s, CANT_EDIT));
    }
    let item = if let Some(i) = p::u64(v, "index") {
        items.get(usize::try_from(i).unwrap_or(usize::MAX))
    } else if let Some(val) = p::str(v, "value") {
        items.iter().find(|i| i.value() == val)
    } else if let Some(t) = p::str(v, "text") {
        items.iter().find(|i| i.display == t)
    } else {
        return Err(CmdError::Params("give index, value or text".into()));
    };
    let Some(item) = item.cloned() else { return Err(CmdError::Params("no such entry".into())) };
    let props = content_base(s, &r);
    let end = set_content(s, &r, &item.display, &props)?;
    if let Some(c) = s.doc.control_mut(&r.start) {
        if let ControlKind::ComboBox { last_value, .. } | ControlKind::DropDown { last_value, .. } = &mut c.kind {
            *last_value = item.value().to_string();
        }
        c.showing_placeholder = false;
    }
    s.sel = Selection::caret(after(&end));
    Ok(json!({"display": item.display, "value": item.value()}))
}

fn set_date(s: &mut Session, v: &Value) -> CmdResult {
    let r = target(s, v)?;
    let ControlKind::Date { format, .. } = &r.control.kind else { return Err(CmdError::Disabled("the selection isn't in a date picker".into())) };
    if !r.control.lock.can_edit() {
        return Err(locked(s, CANT_EDIT));
    }
    let (y, m, d) = p::str(v, "date").and_then(parse_date).ok_or_else(|| CmdError::Params("date: YYYY-MM-DD".into()))?;
    let text = format_date_value(format, y, m, d);
    let props = content_base(s, &r);
    let end = set_content(s, &r, &text, &props)?;
    if let Some(c) = s.doc.control_mut(&r.start) {
        if let ControlKind::Date { full_date, .. } = &mut c.kind {
            *full_date = format!("{y:04}-{m:02}-{d:02}T00:00:00Z");
        }
        c.showing_placeholder = false;
    }
    s.sel = Selection::caret(after(&end));
    Ok(json!({"date": format!("{y:04}-{m:02}-{d:02}"), "text": text}))
}

/// Just after the end marker at `end` (the caret leaves a control whose value was chosen).
fn after(end: &Pos) -> Pos {
    Pos { off: end.off + OBJ.len_utf8(), ..end.clone() }
}

fn remove_control(s: &mut Session, v: &Value) -> CmdResult {
    let r = target(s, v)?;
    if !r.control.lock.can_delete() {
        return Err(locked(s, CANT_DELETE));
    }
    let w = OBJ.len_utf8();
    // A placeholder goes with its control; real content stays.
    if r.control.showing_placeholder {
        s.doc.delete_range(&r.content_start(), &r.end)?;
    }
    let end = s.doc.control_at(&r.content_start()).map(|x| x.end).unwrap_or(r.end.clone());
    s.doc.para_mut(end.story, &end.path)?.delete(end.off, end.off + w)?;
    s.doc.para_mut(r.start.story, &r.start.path)?.delete(r.start.off, r.start.off + w)?;
    s.sel = Selection::caret(s.doc.clamp(&r.start));
    sel_result(s)
}

/// A date with a date picture (`M/d/yyyy`, `dddd, MMMM d, yyyy`…).
pub fn format_date_value(fmt: &str, y: i64, m: u32, d: u32) -> String {
    super::insert::format_date_parts(fmt, y, m as usize, d, (0, 0, 0), days_from_civil(y, m, d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_parse_and_format() {
        assert_eq!(parse_date("2026-10-11"), Some((2026, 10, 11)));
        assert_eq!(parse_date("2024-02-29T00:00:00Z"), Some((2024, 2, 29)));
        assert_eq!(parse_date("2025-02-29"), None);
        assert_eq!(parse_date("20x6-1-1"), None);
        assert_eq!(parse_date("é"), None);
        assert_eq!(format_date_value("dddd, MMMM d, yyyy", 2026, 10, 11), "Sunday, October 11, 2026");
        assert_eq!(format_date_value("M/d/yyyy", 1970, 1, 1), "1/1/1970");
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(super::super::civil_from_days(days_from_civil(2000, 3, 1)), (2000, 3, 1));
    }

    #[test]
    fn parse_date_rejects_signs_and_non_digits() {
        for bad in ["2024-+1-+5", "+123-01-01", "2024-01-+5", "2024-+1-05", "-024-01-01"] {
            assert_eq!(parse_date(bad), None, "{bad}");
        }
        assert_eq!(parse_date("2024-01-05"), Some((2024, 1, 5)));
        assert_eq!(parse_date("2024-01-05T10:00:00"), Some((2024, 1, 5)));
    }

    #[test]
    fn days_from_civil_and_days_in_month_exact_beyond_chronos_range() {
        // Past `NaiveDate`'s ±262143 the 400-year cycles carry the result; values from the
        // pre-chrono formula, both directions, both sides of the epoch.
        for (y, m, d, days) in
            [(262_144i64, 1u32, 1u32, 95_026_602i64), (273_022, 10, 18, 99_000_000), (-262_145, 3, 1, -96_465_964), (-271_821, 4, 21, -99_999_999)]
        {
            assert_eq!(days_from_civil(y, m, d), days, "{y}-{m}-{d}");
            assert_eq!(super::super::civil_from_days(days), (y, m, d), "{days}");
        }
        assert_eq!(days_in_month(272_952, 2), 29, "272952 is a leap year");
        assert_eq!(days_in_month(-269_153, 2), 28);
        // The extremes must not overflow or panic (i128 intermediates / `rem_euclid`).
        let _ = (days_from_civil(i64::MAX, 12, 31), days_from_civil(i64::MIN, 1, 1), days_in_month(i64::MAX, 2), days_in_month(i64::MIN, 2));
    }
}

#[cfg(test)]
mod editing_tests {
    use std::sync::Arc;

    use serde_json::json;
    use wordcraft_doc::StoryRef;
    use wordcraft_doc::control::ControlKind;

    use crate::Session;

    fn run(s: &mut Session, id: &str, v: serde_json::Value) -> serde_json::Value {
        s.run(id, &v).unwrap_or_else(|e| panic!("{id}: {e}"))
    }
    fn text(s: &Session) -> String {
        s.doc.plain_text(StoryRef::Body)
    }
    fn info(s: &mut Session) -> serde_json::Value {
        run(s, "developer.control", json!({}))
    }

    /// A form from German Word names its placeholder style `Platzhaltertext` ("Placeholder Text"),
    /// and each control's `w:rPr` gives the style entered text takes (`Eingabe`, bold black).
    /// Text typed over the placeholder takes the control's run properties, as in Word (17 of 17
    /// values filled in Word did); without them it drops the placeholder style whatever its id.
    /// WordCraft kept the German placeholder style, so entries stayed grey.
    #[test]
    fn typing_over_a_localized_placeholder_takes_the_controls_formatting() {
        use wordcraft_doc::props::{CharProps, Rgb, TextColor};
        use wordcraft_doc::styles::{Style, StyleKind};
        let style =
            |id: &str, name: &str, chr: CharProps| Style { id: id.into(), name: name.into(), kind: StyleKind::Character, chr, ..Default::default() };
        let typed_style = |run_props: Option<CharProps>| {
            let mut s = Session::new(wordcraft_doc::Document::new());
            let grey = CharProps { color: Some(TextColor::Rgb(Rgb(0x80, 0x80, 0x80))), ..Default::default() };
            Arc::make_mut(&mut s.doc.styles).styles.push(style("Platzhaltertext", "Placeholder Text", grey));
            Arc::make_mut(&mut s.doc.styles).styles.push(style("Eingabe", "Eingabe", CharProps { bold: Some(true), ..Default::default() }));
            run(&mut s, "text.insert", json!({"text": "Name: "}));
            run(&mut s, "developer.plainText", json!({"tag": "name"}));
            let start = s.doc.control_at(&s.sel.focus).unwrap().start;
            s.doc.control_mut(&start).unwrap().run_props = run_props;
            // The placeholder uses the document's own placeholder style; none is added.
            let r = s.doc.control_at(&s.sel.focus).unwrap();
            let shown = s.doc.para_at(&r.content_start()).unwrap().props_of_char(r.content_start().off).style.clone();
            assert_eq!(shown.as_deref(), Some("Platzhaltertext"));
            assert_eq!(s.doc.styles.styles.iter().filter(|st| st.name == "Placeholder Text").count(), 1);
            run(&mut s, "caret.right", json!({}));
            run(&mut s, "text.insert", json!({"text": "Ada"}));
            let r = s.doc.control_at(&s.sel.focus).unwrap();
            s.doc.para_at(&r.content_start()).unwrap().props_of_char(r.content_start().off).style.clone()
        };
        let eingabe = CharProps { style: Some("Eingabe".into()), ..Default::default() };
        assert_eq!(typed_style(Some(eingabe)).as_deref(), Some("Eingabe"), "the control's run properties");
        assert_eq!(typed_style(None), None, "no placeholder style left");
        // A producer that localizes the style's name as well: the showing placeholder's own
        // style goes all the same.
        let mut s = Session::new(wordcraft_doc::Document::new());
        let grey = CharProps { color: Some(TextColor::Rgb(Rgb(0x80, 0x80, 0x80))), ..Default::default() };
        Arc::make_mut(&mut s.doc.styles).styles.push(style("TexteEspaceReserve", "Texte de l'espace réservé", grey));
        run(&mut s, "text.insert", json!({"text": "Nom : "}));
        run(&mut s, "developer.plainText", json!({"tag": "nom"}));
        let r = s.doc.control_at(&s.sel.focus).unwrap();
        s.doc.format_range(&r.content_start(), &r.end, &|c| c.style = Some("TexteEspaceReserve".into())).unwrap();
        run(&mut s, "caret.right", json!({}));
        run(&mut s, "text.insert", json!({"text": "Ada"}));
        let r = s.doc.control_at(&s.sel.focus).unwrap();
        let typed = s.doc.para_at(&r.content_start()).unwrap().props_of_char(r.content_start().off).style.clone();
        assert_eq!(typed, None, "a placeholder style localized in id and name");
    }

    /// Typed text with line breaks in it (`type_text` with paragraphs, a typed paste) is the
    /// Enter key pressed between the lines, so a one-line control, where Enter does nothing, stays
    /// one paragraph, and a multi-line one splits (#3973: the line breaks went in as one command
    /// and split a one-line control).
    #[test]
    fn typed_line_breaks_do_not_split_a_one_line_control() {
        for (multi_line, paragraphs) in [(false, 1), (true, 2)] {
            let mut s = Session::new(wordcraft_doc::Document::new());
            run(&mut s, "text.insert", json!({"text": "Name: "}));
            run(&mut s, "developer.plainText", json!({"title": "Name", "tag": "name"}));
            run(&mut s, "developer.properties", json!({"control": "name", "multiLine": multi_line}));
            run(&mut s, "caret.right", json!({}));
            run(&mut s, "text.insert", json!({"text": "Ada\rLovelace", "raw": true}));
            assert_eq!(s.doc.body.len(), paragraphs, "multi-line control: {multi_line}");
            let r = s.doc.control_at(&s.sel.focus).unwrap();
            assert_eq!(r.control.tag, "name", "still inside the control");
        }
    }

    #[test]
    fn typing_replaces_the_placeholder_and_emptying_brings_it_back() {
        let mut s = Session::new(wordcraft_doc::Document::new());
        run(&mut s, "text.insert", json!({"text": "Name: "}));
        run(&mut s, "developer.plainText", json!({"title": "Name", "tag": "name"}));
        let i = info(&mut s);
        assert_eq!((i["type"].as_str(), i["showingPlaceholder"].as_bool()), (Some("plainText"), Some(true)));
        let placeholder = i["placeholder"].as_str().unwrap().to_string();
        assert_eq!(text(&s), format!("Name: {placeholder}"));
        // Typing anywhere in the placeholder replaces all of it, in ordinary formatting.
        run(&mut s, "caret.right", json!({}));
        run(&mut s, "text.insert", json!({"text": "A"}));
        run(&mut s, "text.insert", json!({"text": "da"}));
        assert_eq!(text(&s), "Name: Ada");
        let i = info(&mut s);
        assert_eq!((i["showingPlaceholder"].as_bool(), i["text"].as_str()), (Some(false), Some("Ada")));
        let r = s.doc.control_at(&s.sel.focus).unwrap();
        let para = s.doc.para_at(&r.content_start()).unwrap();
        assert_eq!(para.props_of_char(r.content_start().off).style, None, "not placeholder-styled");
        // Enter in a one-line control does nothing.
        run(&mut s, "text.newParagraph", json!({}));
        assert_eq!(s.doc.body.len(), 1);
        // Deleting all of it shows the placeholder again; Backspace at the content's start keeps the control.
        for _ in 0..3 {
            run(&mut s, "text.backspace", json!({}));
        }
        assert_eq!(text(&s), format!("Name: {placeholder}"));
        assert_eq!(info(&mut s)["showingPlaceholder"], true);
        run(&mut s, "text.backspace", json!({}));
        assert_eq!(text(&s), format!("Name: {placeholder}"));
        assert_eq!(s.doc.control_ranges(StoryRef::Body).len(), 1);
        // Undo goes back through the edits.
        run(&mut s, "edit.undo", json!({}));
        run(&mut s, "edit.undo", json!({}));
        assert_eq!(text(&s), "Name: A");
        // And the file keeps it.
        let back = wordcraft_docx::read(&wordcraft_docx::write(&s.doc).unwrap()).unwrap();
        assert_eq!(back.control_ranges(StoryRef::Body).first().map(|r| r.control.tag.clone()), Some("name".into()));
    }

    #[test]
    fn check_boxes_toggle_and_locks_refuse() {
        let mut s = Session::new(wordcraft_doc::Document::new());
        run(&mut s, "developer.checkBox", json!({"tag": "ok"}));
        assert_eq!(text(&s), "\u{2610}");
        // A check box takes no typing: at its edge the text goes beside it, over it it's refused.
        run(&mut s, "text.insert", json!({"text": "x"}));
        assert_eq!(text(&s), "x\u{2610}");
        run(&mut s, "edit.undo", json!({}));
        let r = s.doc.control_ranges(StoryRef::Body)[0].clone();
        s.sel = crate::Selection { anchor: r.content_start(), focus: r.end.clone() };
        assert!(s.run("text.insert", &json!({"text": "x"})).is_err());
        assert_eq!(run(&mut s, "developer.toggleCheckBox", json!({}))["checked"], true);
        assert_eq!(text(&s), "\u{2612}");
        assert!(matches!(s.doc.control_ranges(StoryRef::Body)[0].control.kind, ControlKind::CheckBox { checked: true, .. }));
        // Content locked: no toggling.
        run(&mut s, "developer.properties", json!({"control": "ok", "lockEdit": true}));
        assert!(s.run("developer.toggleCheckBox", &json!({"control": "ok"})).is_err());
        assert_eq!(text(&s), "\u{2612}");
        // Control locked: selecting it whole and deleting is refused, the document unchanged.
        run(&mut s, "developer.properties", json!({"control": "ok", "lockEdit": false, "lockDelete": true}));
        run(&mut s, "select.all", json!({}));
        let before = s.doc.clone();
        assert!(s.run("text.delete", &json!({})).is_err());
        assert_eq!(s.doc, before);
        assert!(s.status.contains("can't be deleted"));
        // Unlocked, it goes.
        run(&mut s, "developer.properties", json!({"control": "ok", "lockDelete": false}));
        run(&mut s, "select.all", json!({}));
        run(&mut s, "text.delete", json!({}));
        assert!(s.doc.control_ranges(StoryRef::Body).is_empty());
    }

    fn replacement_control() -> Session {
        let mut s = Session::new(wordcraft_doc::Document::from_text("cat cat"));
        run(&mut s, "edit.find", json!({"text": "cat"}));
        run(&mut s, "developer.richText", json!({"tag": "protected"}));
        assert_eq!(text(&s), "cat cat");
        s
    }

    #[test]
    fn replace_all_refuses_locked_content_without_partial_edits() {
        for track_changes in [false, true] {
            let mut s = replacement_control();
            run(&mut s, "developer.properties", json!({"control": "protected", "lockEdit": true}));
            s.doc.settings.track_changes = track_changes;
            let before = s.doc.clone();
            let selection = s.sel.clone();
            let undo_depth = s.undo_depth();
            // The later, unlocked match would be replaced before the locked one.
            assert!(s.run("edit.replaceAll", &json!({"text": "cat", "with": "fox"})).is_err());
            assert!(s.status.contains("can't be edited"));
            assert_eq!(s.doc, before);
            assert_eq!(s.sel, selection);
            assert_eq!(s.undo_depth(), undo_depth);
        }
    }

    #[test]
    fn replace_all_respects_control_delete_locks_but_allows_content_edits() {
        let mut s = replacement_control();
        run(&mut s, "developer.properties", json!({"control": "protected", "lockDelete": true}));
        let before = s.doc.clone();
        // Matching both control markers would delete the control itself.
        assert!(s.run("edit.replaceAll", &json!({"text": "^gcat^g", "with": "fox"})).is_err());
        assert!(s.status.contains("can't be deleted"));
        assert_eq!(s.doc, before);
        // A deletion lock still permits editing the text inside the control.
        assert_eq!(run(&mut s, "edit.replaceAll", json!({"text": "cat", "with": "fox"}))["replaced"], 2);
        assert_eq!(text(&s), "fox fox");
        assert_eq!(s.doc.control_ranges(StoryRef::Body).len(), 1);
        run(&mut s, "edit.undo", json!({}));
        assert_eq!(s.doc, before);
    }

    #[test]
    fn lists_dates_and_blocks() {
        let mut s = Session::new(wordcraft_doc::Document::new());
        run(&mut s, "developer.dropDown", json!({"tag": "size", "items": ["Small", {"display": "Large", "value": "L"}]}));
        run(&mut s, "developer.chooseItem", json!({"control": "size", "value": "L"}));
        run(&mut s, "text.insert", json!({"text": " on "}));
        run(&mut s, "developer.datePicker", json!({"tag": "when", "format": "yyyy-MM-dd"}));
        run(&mut s, "developer.setDate", json!({"control": "when", "date": "2026-10-11"}));
        assert_eq!(text(&s), "Large on 2026-10-11");
        assert!(s.run("developer.setDate", &json!({"control": "when", "date": "2026-02-30"})).is_err());
        // A block control around two paragraphs, and a repeating section.
        run(&mut s, "text.newParagraph", json!({}));
        run(&mut s, "text.insert", json!({"text": "Item"}));
        run(&mut s, "select.all", json!({}));
        run(&mut s, "developer.richText", json!({"title": "All"}));
        run(&mut s, "developer.repeatingSection", json!({}));
        let doc = wordcraft_docx::read(&wordcraft_docx::write(&s.doc).unwrap()).unwrap();
        let kinds: Vec<_> = doc.control_ranges(StoryRef::Body).iter().map(|r| (r.control.kind.name(), r.control.block)).collect();
        for k in [("dropDown", false), ("date", false), ("richText", true), ("repeatingSection", true), ("repeatingSectionItem", true)] {
            assert!(kinds.contains(&k), "{k:?} in {kinds:?}");
        }
        assert_eq!(doc.plain_text(StoryRef::Body), s.doc.plain_text(StoryRef::Body));
    }
}

#[cfg(test)]
mod nesting_tests {
    use std::sync::Arc;

    use serde_json::json;
    use wordcraft_doc::control::{ContentControl, ControlKind, ControlLock, MAX_CONTROL_NESTING};
    use wordcraft_doc::para::{OBJ, ShapeKind};
    use wordcraft_doc::props::CharProps;
    use wordcraft_doc::{Block, Blocks, Document, InlineObject, Paragraph, PartKind, Path, Pos, StoryRef, Table};

    use crate::{Selection, Session, io};

    fn control(id: usize) -> ContentControl {
        ContentControl {
            id: Some(id as i64 + 1),
            tag: format!("field-{id}"),
            title: format!("Field {id}"),
            lock: ControlLock::SdtLocked,
            placeholder: "NamedPlaceholder".into(),
            kind: ControlKind::RichText,
            extra: vec![format!(r#"<w:dataBinding w:xpath="/root/item{id}" w:storeItemID="{{11111111-1111-1111-1111-111111111111}}"/>"#)],
            ..Default::default()
        }
    }

    fn nested(depth: usize, text: &str) -> Blocks {
        let mut p = Paragraph::new();
        for i in 0..depth {
            p.insert_object(p.len(), InlineObject::ControlStart { control: Box::new(control(i)) }, &CharProps::default()).unwrap();
        }
        let bold = CharProps { bold: Some(true), ..Default::default() };
        p.insert_text(p.len(), text, &bold).unwrap();
        for _ in 0..depth {
            p.insert_object(p.len(), InlineObject::ControlEnd, &CharProps::default()).unwrap();
        }
        vec![Arc::new(Block::Para(p))]
    }

    fn shape(story: u32) -> InlineObject {
        InlineObject::Shape {
            kind: ShapeKind::TextBox,
            w: 144.0,
            h: 72.0,
            fill: None,
            stroke: None,
            stroke_width: 0.75,
            float: Default::default(),
            story: Some(story),
            freeform: None,
            effects: Default::default(),
            extra: Default::default(),
        }
    }

    fn add_shape(blocks: &mut Blocks, story: u32, off: usize) {
        let Block::Para(p) = Arc::make_mut(&mut blocks[0]) else { panic!("paragraph") };
        p.insert_object(off, shape(story), &CharProps::default()).unwrap();
    }

    fn run(s: &mut Session, command: &str, value: serde_json::Value) {
        s.run(command, &value).unwrap_or_else(|e| panic!("{command}: {e}"));
    }

    #[test]
    fn nesting_commands_allow_64_then_refuse_65_in_both_selection_directions() {
        for reversed in [false, true] {
            let mut s = Session::new(Document::from_text("seed"));
            for n in 0..MAX_CONTROL_NESTING {
                let a = Pos::body(0, n * OBJ.len_utf8());
                let b = Pos::body(0, a.off + 4);
                s.sel = if reversed { Selection { anchor: b, focus: a } } else { Selection { anchor: a, focus: b } };
                run(&mut s, "developer.plainText", json!({"tag": format!("inserted-{n}")}));
            }
            assert_eq!(s.doc.control_ranges(StoryRef::Body).len(), MAX_CONTROL_NESTING);
            let before = s.doc.clone();
            let selection = s.sel.clone();
            let depth = s.undo_depth();
            assert!(s.run("developer.richText", &json!({})).unwrap_err().to_string().contains("64"));
            assert_eq!(s.doc, before);
            assert_eq!(s.sel, selection);
            assert_eq!(s.undo_depth(), depth);
            assert_eq!(s.doc.plain_text(StoryRef::Body), "seed");
            // A neighboring insertion outside the inline controls still has room.
            s.sel = Selection::caret(Pos::body(0, 0));
            run(&mut s, "developer.checkBox", json!({"tag": "neighbor"}));
            assert_eq!(s.doc.control_ranges(StoryRef::Body).len(), MAX_CONTROL_NESTING + 1);
            assert_eq!(s.doc.control_nesting(&s.sel.focus, &s.sel.focus), 1);
        }
    }

    #[test]
    fn nesting_repeating_section_reserves_two_levels_with_table_wraps() {
        for (inline, accepts) in [(59, true), (60, false)] {
            let mut table = Table::new(1, 1, 144.0);
            table.controls.open = vec![control(101)];
            table.controls.close = 1;
            table.rows[0].controls.open = vec![control(102)];
            table.rows[0].controls.close = 1;
            table.rows[0].cells[0].controls.open = vec![control(103)];
            table.rows[0].cells[0].controls.close = 1;
            table.rows[0].cells[0].blocks = nested(inline, "item");
            let mut doc = Document::new();
            doc.body = vec![Arc::new(Block::Table(table))];
            let at = Pos::new(StoryRef::Body, Path(vec![0, 0, 0, 0]), inline * OBJ.len_utf8());
            let mut s = Session::new(doc);
            s.sel = Selection::caret(at);
            let before = s.doc.clone();
            let result = s.run("developer.repeatingSection", &json!({"tag": "items"}));
            assert_eq!(result.is_ok(), accepts, "inline depth {inline}: {result:?}");
            if accepts {
                assert_eq!(wordcraft_doc::control::nesting_peak(&s.doc.body, &s.doc.parts), MAX_CONTROL_NESTING);
                let kinds: Vec<_> = s.doc.control_ranges(StoryRef::Body).into_iter().map(|r| r.control.kind).collect();
                assert!(kinds.iter().any(|kind| matches!(kind, ControlKind::RepeatingSection { .. })));
                assert!(kinds.contains(&ControlKind::RepeatingSectionItem));
            } else {
                assert_eq!(s.doc, before);
                assert_eq!(s.undo_depth(), 0);
            }
        }
    }

    #[test]
    fn nesting_refusal_after_nonempty_deletion_preserves_model_selection_and_both_history_stacks() {
        for reversed in [false, true] {
            for (depth, command, args, message) in
                [(64, "developer.checkBox", json!({}), "64"), (1, "developer.datePicker", json!({"date": "2026-02-30"}), "YYYY-MM-DD")]
            {
                let mut doc = Document::new();
                doc.body = nested(depth, "seed");
                let original = doc.clone();
                let mut s = Session::new(doc);
                s.sel = Selection::caret(Pos::body(0, depth * OBJ.len_utf8() + 4));
                run(&mut s, "text.insert", json!({"text": "!"}));
                s.close_typing();
                let first = s.doc.clone();
                run(&mut s, "text.insert", json!({"text": "?"}));
                s.close_typing();
                let second = s.doc.clone();
                assert!(s.undo());
                assert_eq!(s.doc, first);
                let a = Pos::body(0, depth * OBJ.len_utf8() + 1);
                let b = Pos::body(0, a.off + 3);
                s.sel = if reversed { Selection { anchor: b, focus: a } } else { Selection { anchor: a, focus: b } };
                s.pending = Some(CharProps { italic: Some(true), ..Default::default() });
                let before = s.doc.clone();
                let selection = s.sel.clone();
                let pending = s.pending.clone();
                let state = (s.dirty, s.rev(), s.undo_depth(), s.undo_mark(), s.undo_labels(), s.redo_label().map(str::to_string), s.typing_open());
                assert!(s.can_undo() && s.can_redo());
                let error = s.run(command, &args).unwrap_err();
                assert!(error.to_string().contains(message), "{error}");
                assert_eq!(s.doc, before, "selection content and every model field restored");
                assert_eq!(s.sel, selection);
                assert_eq!(s.pending, pending);
                assert_eq!(
                    (s.dirty, s.rev(), s.undo_depth(), s.undo_mark(), s.undo_labels(), s.redo_label().map(str::to_string), s.typing_open()),
                    state
                );
                assert!(s.can_undo() && s.can_redo());
                assert!(s.redo(), "refusal must preserve the actual redo snapshot");
                assert_eq!(s.doc, second);
                assert!(s.undo());
                assert_eq!(s.doc, first);
                assert!(s.undo());
                assert_eq!(s.doc, original, "refusal must preserve every earlier undo snapshot");
            }
        }
    }

    #[test]
    fn nesting_edit_uses_deepest_shared_textbox_anchor_and_refuses_missing_targets() {
        let mut doc = Document::new();
        let shared = doc.add_part(PartKind::TextBox, nested(14, "seed"));
        let mut outer_blocks = nested(50, "");
        add_shape(&mut outer_blocks, shared, 50 * OBJ.len_utf8());
        let outer = doc.add_part(PartKind::TextBox, outer_blocks);
        doc.body = nested(0, "");
        add_shape(&mut doc.body, shared, 0); // The first navigation anchor is the shallow one.
        add_shape(&mut doc.body, outer, OBJ.len_utf8());
        let at = Pos::new(StoryRef::Part(shared), Path::top(0), 14 * OBJ.len_utf8() + 1);
        let mut s = Session::new(doc);
        for target in [at, Pos::new(StoryRef::Part(999_999), Path::top(0), 0)] {
            s.sel = Selection::caret(target);
            let before = s.doc.clone();
            let selection = s.sel.clone();
            assert!(s.run("developer.checkBox", &json!({})).is_err());
            assert_eq!(s.doc, before);
            assert_eq!(s.sel, selection);
            assert_eq!(s.undo_depth(), 0);
        }
        // Removing one local level in a distinct valid input makes the same command succeed.
        let mut positive = Document::new();
        let inner = positive.add_part(PartKind::TextBox, nested(13, "seed"));
        positive.body = nested(50, "");
        add_shape(&mut positive.body, inner, 50 * OBJ.len_utf8());
        let mut s = Session::new(positive);
        s.sel = Selection::caret(Pos::new(StoryRef::Part(inner), Path::top(0), 13 * OBJ.len_utf8() + 1));
        run(&mut s, "developer.checkBox", json!({}));
        assert_eq!(wordcraft_doc::control::nesting_peak(&s.doc.body, &s.doc.parts), MAX_CONTROL_NESTING);
    }

    fn controls(doc: &Document) -> Vec<ContentControl> {
        let mut out = Vec::new();
        wordcraft_doc::control::walk_controls(&doc.body, &mut |c| out.push(c.clone()));
        for part in doc.parts.values() {
            wordcraft_doc::control::walk_controls(&part.blocks, &mut |c| out.push(c.clone()));
        }
        out
    }

    #[test]
    fn nesting_imported_deep_controls_keep_exact_semantics_through_engine_save_and_reopen() {
        let mut imported = Document::new();
        imported.body = nested(80, "café 🚀");
        let expected_controls = controls(&imported);
        let mut current = io::open_bytes("deep.json", &io::save_bytes("deep.json", &imported).unwrap()).unwrap();
        assert_eq!(current.body, imported.body, "native open does not trim existing controls");
        for _ in 0..2 {
            current = io::open_bytes("deep.docx", &io::save_bytes("deep.docx", &current).unwrap()).unwrap();
            assert_eq!(controls(&current), expected_controls, "ids, locks, kinds, tags, aliases, placeholders and binding XML");
            assert_eq!(current.plain_text(StoryRef::Body), "café 🚀");
            let p = current.para_at(&Pos::body(0, 80 * OBJ.len_utf8())).unwrap();
            assert_eq!(p.props_of_char(80 * OBJ.len_utf8()).bold, Some(true));
        }
        let mut s = Session::new(current);
        s.sel = Selection::caret(Pos::body(0, 80 * OBJ.len_utf8() + "café 🚀".len()));
        run(&mut s, "text.insert", json!({"text": "!"}));
        assert_eq!(controls(&s.doc), expected_controls, "ordinary text editing preserves imported depth");
        assert_eq!(s.doc.plain_text(StoryRef::Body), "café 🚀!");
        let back = io::open_bytes("deep.json", &io::save_bytes("deep.json", &s.doc).unwrap()).unwrap();
        assert_eq!(back.body, s.doc.body);
    }

    #[test]
    fn nesting_trimmed_textbox_paste_retains_bindings_and_formatted_content_on_repeated_reopen() {
        let mut doc = Document::new();
        doc.body = nested(50, "root");
        let mut source = Document::new();
        let story = source.add_part(PartKind::TextBox, nested(30, "café 🚀"));
        let mut fragment_body = nested(0, "");
        add_shape(&mut fragment_body, story, 0);
        let fragment =
            wordcraft_doc::edit::Fragment { blocks: fragment_body.into_iter().map(Arc::unwrap_or_clone).collect(), parts: source.parts.clone() };
        let untouched = fragment.clone();
        let mut s = Session::new(doc);
        s.sel = Selection::caret(Pos::body(0, 50 * OBJ.len_utf8()));
        s.clipboard = Some(fragment);
        run(&mut s, "edit.paste", json!({}));
        assert_eq!(s.clipboard.as_ref(), Some(&untouched));
        let expected_controls = controls(&s.doc);
        assert_eq!(expected_controls.len(), MAX_CONTROL_NESTING);
        assert_eq!(expected_controls[..50], (0..50).map(control).collect::<Vec<_>>());
        assert_eq!(expected_controls[50..], (0..14).map(control).collect::<Vec<_>>());
        let mut current = s.doc.clone();
        for _ in 0..2 {
            current = io::open_bytes("trimmed.docx", &io::save_bytes("trimmed.docx", &current).unwrap()).unwrap();
            assert_eq!(controls(&current), expected_controls);
            assert_eq!(current.plain_text(StoryRef::Body), "root");
            let (&id, part) = current.parts.iter().find(|(_, part)| part.kind == PartKind::TextBox).unwrap();
            assert_eq!(current.plain_text(StoryRef::Part(id)), "café 🚀");
            let Block::Para(p) = &*part.blocks[0] else { panic!("textbox paragraph") };
            assert_eq!(p.props_of_char(14 * OBJ.len_utf8()).bold, Some(true));
        }
        let native = io::open_bytes("trimmed.json", &io::save_bytes("trimmed.json", &current).unwrap()).unwrap();
        assert_eq!(native, current);
    }

    fn malformed_table(depth: usize, kind: usize) -> (Document, Pos) {
        let mut table = Table::new(
            if kind == 4 {
                3
            } else if kind == 0 {
                2
            } else {
                1
            },
            if kind == 1 { 2 } else { 1 },
            144.0,
        );
        table.controls.open = (0..depth)
            .map(|id| {
                let mut c = control(id);
                c.block = true;
                c
            })
            .collect();
        table.controls.close = depth as u32;
        for row in &mut table.rows {
            for cell in &mut row.cells {
                cell.blocks = nested(0, "seed");
            }
        }
        let mut at = Pos::new(StoryRef::Body, Path(vec![0, 0, 0, 0]), 1);
        match kind {
            4 => {
                table.rows[0].controls.open = std::mem::take(&mut table.controls.open);
                table.controls.close = 0;
                table.rows[1].cells.clear();
                table.rows[1].controls.close = u32::MAX;
                table.rows[2].controls.close = depth as u32;
                at.path = Path(vec![0, 2, 0, 0]);
            }
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
        let mut doc = Document::new();
        doc.body = vec![Arc::new(Block::Table(table))];
        // Session edits require a final story paragraph. Normalize the synthetic import
        // before taking its text baseline, retaining the ignored row and its raw wrappers.
        doc.ensure_nonempty();
        (doc, at)
    }

    fn assert_reopen_depth(doc: &Document, depth: usize) {
        let expected_controls = controls(doc);
        // The existing writer omits empty-cells rows, which have no paragraph text.
        // Account only for that documented structural omission when checking text.
        let mut visible = doc.clone();
        for block in &mut visible.body {
            if let Block::Table(table) = Arc::make_mut(block) {
                table.rows.retain(|row| !row.cells.is_empty());
            }
        }
        let expected_text = visible.plain_text(StoryRef::Body);
        let mut current = doc.clone();
        for _ in 0..2 {
            current = io::open_bytes("nesting.docx", &io::save_bytes("nesting.docx", &current).unwrap()).unwrap();
            assert_eq!(controls(&current), expected_controls);
            assert_eq!(current.plain_text(StoryRef::Body), expected_text);
            assert_eq!(wordcraft_doc::control::nesting_peak(&current.body, &current.parts), depth);
        }
        let native = io::open_bytes("nesting.json", &io::save_bytes("nesting.json", &current).unwrap()).unwrap();
        assert_eq!(native, current);
    }

    #[test]
    fn nesting_orphan_closes_refuse_65_and_keep_neighboring_insertions() {
        for kind in [4, 0, 1, 2, 3] {
            for depth in [64, 63] {
                let (doc, at) = malformed_table(depth, kind);
                let native = io::open_bytes("imported.json", &io::save_bytes("imported.json", &doc).unwrap()).unwrap();
                let mut s = Session::new(native);
                run(&mut s, "caret.set", json!({"pos": at}));
                let before = s.doc.clone();
                let selection = s.sel.clone();
                let history = (s.undo_depth(), s.can_redo(), s.rev(), s.dirty);
                let result = s.run("developer.checkBox", &json!({}));
                if depth == 64 {
                    assert!(result.is_err(), "orphan close kind {kind} must not grant a 65th level");
                    assert_eq!(s.doc, before);
                    assert_eq!(s.sel, selection);
                    assert_eq!((s.undo_depth(), s.can_redo(), s.rev(), s.dirty), history);
                } else {
                    assert!(result.is_ok(), "valid neighboring insertion: {result:?}");
                    assert_reopen_depth(&s.doc, 64);
                }
            }
        }
    }

    #[test]
    fn nesting_empty_json_carets_refuse_65_and_allow_64() {
        for command in ["caret.set", "select.range"] {
            for depth in [64, 63] {
                let mut doc = Document::new();
                doc.body = nested(depth, "seed");
                let mut s = Session::new(doc);
                let at = json!({"story": "body", "path": [], "off": depth * OBJ.len_utf8() + 1});
                let args = if command == "caret.set" { json!({"pos": at}) } else { json!({"anchor": at.clone(), "focus": at}) };
                run(&mut s, command, args);
                assert!(s.sel.focus.path.0.is_empty(), "exercise the accepted alias");
                let (before, selection, history) = (s.doc.clone(), s.sel.clone(), s.undo_depth());
                let result = s.run("developer.checkBox", &json!({}));
                if depth == 64 {
                    assert!(result.is_err(), "{command} alias must not bypass the limit");
                    assert_eq!(s.doc, before);
                    assert_eq!(s.sel, selection);
                    assert_eq!(s.undo_depth(), history);
                } else {
                    assert!(result.is_ok(), "{result:?}");
                    assert_reopen_depth(&s.doc, 64);
                }
            }
        }
    }

    #[test]
    fn nesting_empty_json_ranges_refuse_wrapping_without_partial_mutation() {
        for (left_alias, right_alias) in [(true, true), (true, false), (false, true), (false, false)] {
            for reversed in [false, true] {
                for depth in [64, 63] {
                    let mut doc = Document::new();
                    doc.body = nested(depth, "seed");
                    let mut s = Session::new(doc);
                    let a = json!({"story": "body", "path": if left_alias { vec![] } else { vec![0] }, "off": depth * OBJ.len_utf8() + 1});
                    let b = json!({"story": "body", "path": if right_alias { vec![] } else { vec![0] }, "off": depth * OBJ.len_utf8() + 3});
                    run(&mut s, "select.range", if reversed { json!({"anchor": b, "focus": a}) } else { json!({"anchor": a, "focus": b}) });
                    let (before, selection, history) = (s.doc.clone(), s.sel.clone(), s.undo_depth());
                    let result = s.run("developer.richText", &json!({"tag": "alias-range"}));
                    if depth == 64 {
                        assert!(result.is_err());
                        assert_eq!(s.doc, before);
                        assert_eq!(s.sel, selection);
                        assert_eq!(s.undo_depth(), history);
                    } else {
                        assert!(result.is_ok(), "{result:?}");
                        let range = s.doc.control_ranges(StoryRef::Body).into_iter().find(|r| r.control.tag == "alias-range").unwrap();
                        assert!(range.control.placeholder_text.is_empty(), "unused constructor default has a canonical representation");
                        assert_eq!(
                            range.control.placeholder_shown(),
                            ControlKind::RichText.default_placeholder(),
                            "the visible fallback is preserved"
                        );
                        assert_eq!(range.start.path, Path::top(0));
                        assert_eq!(range.end.path, range.start.path);
                        let p = s.doc.para_at(&range.start).unwrap();
                        assert_eq!(p.text.get(range.content_start().off..range.end.off), Some("ee"), "aliases wrap only the selected characters");
                        assert_reopen_depth(&s.doc, 64);
                    }
                }
            }
        }
    }

    #[test]
    fn nesting_orphan_and_alias_paste_never_adds_a_65th_saved_level() {
        for kind in [4, 0, 1, 2, 3, 5] {
            for depth in [64, 63] {
                let (doc, at) = if kind < 5 {
                    malformed_table(depth, kind)
                } else {
                    let mut doc = Document::new();
                    doc.body = nested(depth, "seed");
                    (doc, Pos::new(StoryRef::Body, Path(Vec::new()), depth * OBJ.len_utf8() + 1))
                };
                let mut s = Session::new(doc);
                run(&mut s, "caret.set", json!({"pos": at}));
                let before_controls = controls(&s.doc);
                let before_text = s.doc.plain_text(StoryRef::Body);
                let fragment = wordcraft_doc::edit::Fragment {
                    blocks: nested(1, "pasted").into_iter().map(Arc::unwrap_or_clone).collect(),
                    parts: Default::default(),
                };
                s.clipboard = Some(fragment.clone());
                run(&mut s, "edit.paste", json!({}));
                assert_eq!(s.clipboard.as_ref(), Some(&fragment));
                let after_controls = controls(&s.doc);
                assert_eq!(after_controls.len(), 64, "kind {kind}, inherited depth {depth}");
                assert_eq!(after_controls[..depth], before_controls);
                let text = s.doc.plain_text(StoryRef::Body);
                assert_eq!(text.matches("pasted").count(), 1);
                assert_eq!(text.replacen("pasted", "", 1), before_text);
                assert_reopen_depth(&s.doc, 64);
            }
        }
    }

    fn two_paragraph_fragment() -> wordcraft_doc::edit::Fragment {
        let mut first = Paragraph::new();
        let mut wrapper = control(900);
        wrapper.block = true;
        first.insert_object(0, InlineObject::ControlStart { control: Box::new(wrapper) }, &CharProps::default()).unwrap();
        first.insert_text(first.len(), "left", &CharProps { italic: Some(true), ..Default::default() }).unwrap();
        let mut last = Paragraph::new();
        last.insert_text(0, "right", &CharProps { italic: Some(true), ..Default::default() }).unwrap();
        last.insert_object(last.len(), InlineObject::ControlEnd, &CharProps::default()).unwrap();
        wordcraft_doc::edit::Fragment { blocks: vec![Block::Para(first), Block::Para(last)], parts: Default::default() }
    }

    // This observes the actual saved XML stream, independently of the model planner/counter.
    fn saved_xml_peak(bytes: &[u8]) -> usize {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut xml = String::new();
        std::io::Read::read_to_string(&mut archive.by_name("word/document.xml").unwrap(), &mut xml).unwrap();
        let (mut depth, mut peak) = (0usize, 0usize);
        for token in xml.split('>') {
            let tag = token.rsplit('<').next().unwrap_or_default();
            if tag == "w:sdt" || tag.starts_with("w:sdt ") {
                depth += 1;
                peak = peak.max(depth);
            } else if tag == "/w:sdt" {
                assert!(depth > 0, "serialized controls must balance");
                depth -= 1;
            }
        }
        assert_eq!(depth, 0);
        peak
    }

    fn assert_saved_reopens(doc: &Document, peak: usize) {
        let mut current = doc.clone();
        let expected_text = doc.plain_text(StoryRef::Body);
        for _ in 0..2 {
            let bytes = io::save_bytes("promoted.docx", &current).unwrap();
            assert_eq!(saved_xml_peak(&bytes), peak, "actual serialized w:sdt peak");
            current = io::open_bytes("promoted.docx", &bytes).unwrap();
            assert_eq!(current.plain_text(StoryRef::Body), expected_text, "merged text survives every saved reopen");
        }
        let native = io::open_bytes("promoted.json", &io::save_bytes("promoted.json", &current).unwrap()).unwrap();
        assert_eq!(native, current);
    }

    fn promoted_controls_by_id(doc: &Document) -> std::collections::BTreeMap<i64, ContentControl> {
        let values = controls(doc);
        let count = values.len();
        let mut by_id = std::collections::BTreeMap::new();
        for control in values {
            let id = control.id.expect("the promoted synthetic controls have explicit ids");
            assert!(by_id.insert(id, control).is_none(), "the promoted synthetic ids must be unique");
        }
        assert_eq!(by_id.len(), count);
        by_id
    }

    fn promoted_fixture(depth: usize) -> Document {
        let mut doc = Document::new();
        doc.body = nested(depth, "old");
        let Block::Para(first) = Arc::make_mut(&mut doc.body[0]) else { panic!("paragraph") };
        first.insert_text(first.len(), " boundary ", &CharProps::default()).unwrap();
        let mut wrapper = control(900);
        wrapper.block = true;
        first.insert_object(first.len(), InlineObject::ControlStart { control: Box::new(wrapper) }, &CharProps::default()).unwrap();
        first.insert_text(first.len(), "first", &CharProps::default()).unwrap();
        let mut last = Paragraph::new();
        last.insert_text(0, "last", &CharProps::default()).unwrap();
        last.insert_object(last.len(), InlineObject::ControlEnd, &CharProps::default()).unwrap();
        doc.body.push(Arc::new(Block::Para(last)));
        doc
    }

    #[test]
    fn nesting_effective_promoted_destination_refuses_65th_command_with_rollback() {
        for depth in [63, 62] {
            let doc = promoted_fixture(depth);
            assert_saved_reopens(&doc, depth + 1);
            let mut s = Session::new(doc);
            s.sel = Selection::caret(Pos::body(0, depth * OBJ.len_utf8() + 1));
            let (before, selection, history) = (s.doc.clone(), s.sel.clone(), s.undo_depth());
            let result = s.run("developer.checkBox", &json!({"tag": "candidate"}));
            if depth == 63 {
                assert!(result.is_err(), "the promoted enclosing block already consumes the 64th level");
                assert_eq!(s.doc, before);
                assert_eq!(s.sel, selection);
                assert_eq!(s.undo_depth(), history);
            } else {
                assert!(result.is_ok(), "one actual serialized level remains: {result:?}");
                assert_saved_reopens(&s.doc, 64);
            }
        }
    }

    #[test]
    fn nesting_effective_paste_protects_prefix_and_suffix_on_repeated_saved_reopen() {
        for before in [true, false] {
            for depth in [64, 63] {
                let mut doc = Document::new();
                doc.body = nested(depth, "old");
                let original = controls(&doc);
                let offset = if before { 0 } else { doc.para_at(&Pos::body(0, 0)).unwrap().len() };
                let mut s = Session::new(doc);
                s.sel = Selection::caret(Pos::body(0, offset));
                let fragment = two_paragraph_fragment();
                s.clipboard = Some(fragment.clone());
                let unchanged = s.doc.clone();
                run(&mut s, "edit.paste", json!({}));
                let after = s.doc.clone();
                let actual = controls(&after);
                assert_eq!(
                    saved_xml_peak(&io::save_bytes("endpoint.docx", &after).unwrap()),
                    64,
                    "actual saved endpoint promotion must not create a 65th level"
                );
                assert_eq!(actual.len(), 64, "both endpoint promotions reserve old controls: before={before}, depth={depth}");
                for control in &original {
                    assert!(actual.contains(control), "destination metadata is unchanged");
                }
                assert_eq!(actual.iter().any(|control| control.tag == "field-900"), depth == 63);
                assert_eq!(s.clipboard.as_ref(), Some(&fragment));
                assert_eq!(after.plain_text(StoryRef::Body), if before { "left\nrightold" } else { "oldleft\nright" });
                assert_saved_reopens(&after, 64);
                let expected_metadata = promoted_controls_by_id(&after);
                let mut current = after.clone();
                for _ in 0..2 {
                    current = io::open_bytes("metadata.docx", &io::save_bytes("metadata.docx", &current).unwrap()).unwrap();
                    assert_eq!(promoted_controls_by_id(&current), expected_metadata, "every full control field survives effective writer preorder");
                    for (word, italic) in [("old", false), ("left", true), ("right", true)] {
                        let (para, off) = current
                            .para_paths(StoryRef::Body)
                            .into_iter()
                            .filter_map(|path| current.para(StoryRef::Body, &path))
                            .find_map(|para| para.text.find(word).map(|off| (para, off)))
                            .unwrap();
                        let props = para.props_of_char(off);
                        assert_eq!(if italic { props.italic } else { props.bold }, Some(true), "retained formatting for {word}");
                    }
                }
                assert!(s.undo());
                assert_eq!(s.doc, unchanged);
                assert!(s.redo());
                assert_eq!(s.doc, after);
            }
        }
    }

    #[test]
    fn nesting_effective_paste_protects_merged_destination_textbox_content() {
        for before in [true, false] {
            for depth in [64, 63] {
                let mut doc = Document::new();
                let story = doc.add_part(PartKind::TextBox, nested(depth, "café 🚀"));
                let original = doc.parts[&story].clone();
                doc.body = nested(0, "");
                add_shape(&mut doc.body, story, 0);
                let mut s = Session::new(doc);
                s.sel = Selection::caret(Pos::body(0, if before { 0 } else { OBJ.len_utf8() }));
                let fragment = two_paragraph_fragment();
                s.clipboard = Some(fragment.clone());
                run(&mut s, "edit.paste", json!({}));
                assert_eq!(s.doc.parts[&story], original, "destination story is protected");
                assert_eq!(s.clipboard.as_ref(), Some(&fragment));
                assert_eq!(saved_xml_peak(&io::save_bytes("shape-endpoint.docx", &s.doc).unwrap()), 64, "actual saved destination shape nesting");
                assert_eq!(s.doc.control_ranges(StoryRef::Body).len(), usize::from(depth == 63), "promoted control must reserve the shape story");
                assert_saved_reopens(&s.doc, 64);
                let expected_metadata = promoted_controls_by_id(&s.doc);
                let mut current = s.doc.clone();
                for _ in 0..2 {
                    current = io::open_bytes("shape.docx", &io::save_bytes("shape.docx", &current).unwrap()).unwrap();
                    assert_eq!(promoted_controls_by_id(&current), expected_metadata);
                    let (&id, part) = current.parts.iter().find(|(_, part)| part.kind == PartKind::TextBox).unwrap();
                    assert_eq!(current.plain_text(StoryRef::Part(id)), "café 🚀");
                    let Block::Para(p) = &*part.blocks[0] else { panic!("paragraph") };
                    assert_eq!(p.props_of_char(depth * OBJ.len_utf8()).bold, Some(true));
                }
            }
        }
    }

    #[test]
    fn nesting_effective_paste_keeps_valid_inline_and_shifted_block_controls() {
        for at_start in [true, false] {
            for block_hint in [true, false] {
                let mut doc = Document::new();
                doc.body = nested(64, "old");
                let len = doc.para_at(&Pos::body(0, 0)).unwrap().len();
                let mut s = Session::new(doc);
                let mut fragment = wordcraft_doc::edit::Fragment {
                    blocks: nested(1, "new").into_iter().map(Arc::unwrap_or_clone).collect(),
                    parts: Default::default(),
                };
                let Block::Para(p) = &mut fragment.blocks[0] else { panic!("paragraph") };
                let InlineObject::ControlStart { control } = &mut p.objects[0] else { panic!("start") };
                control.block = block_hint;
                s.sel = Selection::caret(Pos::body(0, if at_start { 0 } else { len }));
                s.clipboard = Some(fragment.clone());
                run(&mut s, "edit.paste", json!({}));
                assert_eq!(controls(&s.doc).len(), 65, "a separate inline sibling keeps its own control");
                assert_eq!(s.clipboard.as_ref(), Some(&fragment));
                assert_saved_reopens(&s.doc, 64);
            }
        }
        let mut doc = Document::new();
        doc.body = nested(64, "old");
        let Block::Para(p) = Arc::make_mut(&mut doc.body[0]) else { panic!("paragraph") };
        for object in &mut p.objects {
            if let InlineObject::ControlStart { control } = object {
                control.block = true;
            }
        }
        // Keep the previous controls genuinely block-level after the endpoint merge: they
        // span two paragraphs. A single-paragraph block hint would demote after appended text.
        let mut last = p.split_off(64 * OBJ.len_utf8() + "old".len()).unwrap();
        last.insert_text(0, "tail", &CharProps::default()).unwrap();
        let len = last.len();
        doc.body.push(Arc::new(Block::Para(last)));
        let boundary = Pos::body(1, len);
        let normalized = doc.typing_pos(&boundary);
        assert_eq!(normalized.off, "tail".len(), "typing after trailing block ends enters those controls");
        let before_controls = controls(&doc);
        let mut inside = Session::new(doc.clone());
        inside.sel = Selection::caret(boundary);
        inside.clipboard = Some(two_paragraph_fragment());
        run(&mut inside, "edit.paste", json!({}));
        assert_eq!(controls(&inside.doc), before_controls, "normalized inside64 paste trims only the new wrapper");
        assert_saved_reopens(&inside.doc, 64);
        let Block::Para(last) = Arc::make_mut(&mut doc.body[1]) else { panic!("paragraph") };
        last.insert_text(last.len(), " outside", &CharProps::default()).unwrap();
        let len = last.len();
        let outside = Pos::body(1, len);
        assert_eq!(doc.typing_pos(&outside), outside, "visible trailing text makes the caret truly outside");
        let mut s = Session::new(doc);
        s.sel = Selection::caret(Pos::body(1, len));
        s.clipboard = Some(two_paragraph_fragment());
        run(&mut s, "edit.paste", json!({}));
        assert_eq!(controls(&s.doc).len(), 65, "the retained previous block close shifts the new wrapper to the next paragraph");
        assert_saved_reopens(&s.doc, 64);
    }

    fn implicit_scope_fixture(depth: usize, cells: bool) -> (Document, Pos) {
        let mut table = Table::new(if cells { 1 } else { 2 }, if cells { 2 } else { 1 }, 144.0);
        for row in &mut table.rows {
            for cell in &mut row.cells {
                cell.blocks = nested(0, "seed");
            }
        }
        let wrap = if cells { &mut table.rows[0].cells[0].controls } else { &mut table.rows[0].controls };
        wrap.open = (0..depth)
            .map(|id| {
                let mut ctl = control(id);
                ctl.block = true;
                ctl
            })
            .collect();
        assert_eq!(wrap.close, 0);
        let mut doc = Document::new();
        doc.body = vec![Arc::new(Block::Table(table))];
        doc.ensure_nonempty();
        let at = Pos::new(StoryRef::Body, Path(if cells { vec![0, 0, 1, 0] } else { vec![0, 1, 0, 0] }), 1);
        (doc, at)
    }

    fn assert_implicit_scope_reopens(doc: &Document, peak: usize) {
        let expected_text = doc.plain_text(StoryRef::Body);
        let expected_metadata = promoted_controls_by_id(doc);
        let mut current = doc.clone();
        for _ in 0..2 {
            let bytes = io::save_bytes("implicit.docx", &current).unwrap();
            assert_eq!(saved_xml_peak(&bytes), peak, "actual saved implicit scope depth");
            current = io::open_bytes("implicit.docx", &bytes).unwrap();
            assert_eq!(current.plain_text(StoryRef::Body), expected_text);
            assert_eq!(promoted_controls_by_id(&current), expected_metadata, "all full control fields survive normalized explicit ends");
            for path in current.para_paths(StoryRef::Body) {
                let para = current.para(StoryRef::Body, &path).unwrap();
                let pasted = para.text.find("pasted").map(|off| off..off + "pasted".len());
                for (off, ch) in para.text.char_indices().filter(|(_, ch)| *ch != OBJ) {
                    let props = para.props_of_char(off);
                    if pasted.as_ref().is_some_and(|range| range.contains(&off)) {
                        assert_eq!(props.italic, Some(true), "pasted text formatting survives at {off}:{ch}");
                    } else {
                        assert_eq!(props.bold, Some(true), "destination text formatting survives at {off}:{ch}");
                    }
                }
            }
        }
        let native = io::open_bytes("implicit.json", &io::save_bytes("implicit.json", doc).unwrap()).unwrap();
        assert_eq!(native, *doc, "native serialization retains the original implicit-end representation");
    }

    fn implicit_scope_paste(cells: bool) {
        for depth in [64, 63] {
            let (doc, at) = implicit_scope_fixture(depth, cells);
            assert_eq!(doc.control_nesting(&at, &at), depth, "scope spans the second sibling");
            let original = controls(&doc);
            let original_text = doc.plain_text(StoryRef::Body);
            let mut paragraph = Paragraph::new();
            paragraph.insert_object(0, InlineObject::ControlStart { control: Box::new(control(900)) }, &CharProps::default()).unwrap();
            paragraph.insert_text(paragraph.len(), "pasted", &CharProps { italic: Some(true), ..Default::default() }).unwrap();
            paragraph.insert_object(paragraph.len(), InlineObject::ControlEnd, &CharProps::default()).unwrap();
            let fragment = wordcraft_doc::edit::Fragment { blocks: vec![Block::Para(paragraph)], parts: Default::default() };
            let mut session = Session::new(doc.clone());
            session.sel = Selection::caret(at);
            session.clipboard = Some(fragment.clone());
            run(&mut session, "edit.paste", json!({}));
            let after = session.doc.clone();
            assert_eq!(
                saved_xml_peak(&io::save_bytes("implicit-paste.docx", &after).unwrap()),
                64,
                "the writer implicitly closes all destination wraps: no saved65th level"
            );
            let actual = controls(&after);
            assert_eq!(actual.len(), 64);
            for control in &original {
                assert!(actual.contains(control), "every destination control is protected");
            }
            assert_eq!(actual.iter().any(|ctl| ctl.tag == "field-900"), depth == 63, "nearby valid control is retained");
            assert_eq!(after.plain_text(StoryRef::Body).replace("pasted", ""), original_text);
            let Block::Table(table) = &*after.body[0] else { panic!("table retained") };
            let wrap = if cells { &table.rows[0].cells[0].controls } else { &table.rows[0].controls };
            assert_eq!(wrap.close, 0, "destination implicit representation is not rewritten");
            assert_eq!(session.clipboard.as_ref(), Some(&fragment));
            assert_implicit_scope_reopens(&after, 64);
            assert!(session.undo());
            assert_eq!(session.doc, doc);
            assert!(session.redo());
            assert_eq!(session.doc, after);
        }
    }

    #[test]
    fn nesting_implicit_row_end_protects_second_sibling_on_saved_paste() {
        implicit_scope_paste(false);
    }

    #[test]
    fn nesting_implicit_cell_end_protects_second_sibling_on_saved_paste() {
        implicit_scope_paste(true);
    }

    #[test]
    fn nesting_implicit_fresh_70_wraps_save_only_64_with_bounded_work() {
        for cells in [false, true] {
            let exercise = |count: usize| {
                let (original, _) = implicit_scope_fixture(count, cells);
                let mut after = original.clone();
                let mut removed = 0;
                let steps = wordcraft_doc::work::count(|| removed = wordcraft_doc::control::trim_nesting(&mut after.body, &mut after.parts, 64));
                assert_eq!(saved_xml_peak(&io::save_bytes("fresh-implicit.docx", &after).unwrap()), 64, "fresh implicit70 must not save70 levels");
                assert_eq!(removed, count - 64);
                let mut expected = controls(&original);
                expected.truncate(64);
                assert_eq!(controls(&after), expected, "retained controls keep every property");
                assert_eq!(after.plain_text(StoryRef::Body), original.plain_text(StoryRef::Body));
                let Block::Table(table) = &*after.body[0] else { panic!("table retained") };
                let wrap = if cells { &table.rows[0].cells[0].controls } else { &table.rows[0].controls };
                assert_eq!(wrap.close, 0);
                assert_eq!(controls(&original).len(), count, "shared source was not changed");
                assert_implicit_scope_reopens(&after, 64);
                assert!(steps >= (2 * count) as u64, "count actual scope elements/open/pair visits");
                steps
            };
            exercise(70);
            let small = exercise(128);
            let large = exercise(1024);
            assert!(large <= small * 12, "8x implicit scopes retain bounded actual plan work: {small} -> {large}");
        }
    }
}
