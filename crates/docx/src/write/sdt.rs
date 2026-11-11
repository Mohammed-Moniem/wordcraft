//! Content controls → `w:sdt` (ECMA-376 §17.5.2; check boxes and repeating sections as the
//! [MS-DOCX] `w14`/`w15` extensions).
//!
//! Controls are markers in the model ([`wordcraft_doc::control`]); the file wants them as
//! elements around runs (inline) or around whole paragraphs and tables (block level). [`plan`]
//! decides, per block list, which controls are written where so that the XML always nests.

pub use wordcraft_doc::control::control_plan as plan;
use wordcraft_doc::control::{ContentControl, ControlKind};

use crate::xml::W;

fn on(w: &mut W, name: &str, v: bool) {
    if v {
        w.empty(name, &[]);
    }
}

/// `w:sdtPr` (and `w:sdtEndPr`), then open `w:sdtContent`.
pub fn open(w: &mut W, c: &ContentControl) {
    w.open("w:sdt", &[]);
    w.open("w:sdtPr", &[]);
    w.raw(&c.rpr_xml);
    if !c.title.is_empty() {
        w.val("w:alias", &c.title);
    }
    if !c.tag.is_empty() {
        w.val("w:tag", &c.tag);
    }
    if let Some(id) = c.id {
        w.val("w:id", &id.to_string());
    }
    if let Some(l) = c.lock.ooxml() {
        w.val("w:lock", l);
    }
    if !c.placeholder.is_empty() {
        w.open("w:placeholder", &[]);
        w.val("w:docPart", &c.placeholder);
        w.close("w:placeholder");
    }
    on(w, "w:temporary", c.temporary);
    on(w, "w:showingPlcHdr", c.showing_placeholder);
    // Kept elements of the schema's sequence before the type, then the type, then the rest.
    let early = |x: &&String| {
        ["<w:dataBinding", "<w:label", "<w:tabIndex"].iter().any(|n| x.strip_prefix(n).is_some_and(|rest| rest.starts_with([' ', '/', '>'])))
    };
    for x in c.extra.iter().filter(early) {
        w.raw(x);
    }
    kind(w, &c.kind);
    for x in c.extra.iter().filter(|x| !early(x)) {
        w.raw(x);
    }
    w.close("w:sdtPr");
    w.raw(&c.end_pr_xml);
    w.open("w:sdtContent", &[]);
}

/// Close `w:sdtContent` and `w:sdt`.
pub fn close(w: &mut W) {
    w.raw("</w:sdtContent></w:sdt>");
}

fn kind(w: &mut W, k: &ControlKind) {
    match k {
        ControlKind::RichText => {}
        ControlKind::Text { multi_line } => {
            if *multi_line {
                w.empty("w:text", &[("w:multiLine", "1")]);
            } else {
                w.empty("w:text", &[]);
            }
        }
        ControlKind::CheckBox { checked, checked_char, checked_font, unchecked_char, unchecked_font } => {
            w.open("w14:checkbox", &[]);
            w.empty("w14:checked", &[("w14:val", if *checked { "1" } else { "0" })]);
            for (tag, ch, font) in [("w14:checkedState", checked_char, checked_font), ("w14:uncheckedState", unchecked_char, unchecked_font)] {
                let hex = format!("{:04X}", u32::from(*ch));
                if font.is_empty() {
                    w.empty(tag, &[("w14:val", &hex)]);
                } else {
                    w.empty(tag, &[("w14:val", &hex), ("w14:font", font)]);
                }
            }
            w.close("w14:checkbox");
        }
        ControlKind::ComboBox { items, last_value } | ControlKind::DropDown { items, last_value } => {
            let tag = if matches!(k, ControlKind::ComboBox { .. }) { "w:comboBox" } else { "w:dropDownList" };
            if last_value.is_empty() {
                w.open(tag, &[]);
            } else {
                w.open(tag, &[("w:lastValue", last_value)]);
            }
            for i in items {
                w.empty("w:listItem", &[("w:displayText", &i.display), ("w:value", i.value())]);
            }
            w.close(tag);
        }
        ControlKind::Date { full_date, format, lid, calendar, store_as } => {
            if full_date.is_empty() {
                w.open("w:date", &[]);
            } else {
                w.open("w:date", &[("w:fullDate", full_date)]);
            }
            for (tag, v) in [("w:dateFormat", format), ("w:lid", lid), ("w:storeMappedDataAs", store_as), ("w:calendar", calendar)] {
                if !v.is_empty() {
                    w.val(tag, v);
                }
            }
            w.close("w:date");
        }
        ControlKind::Picture => w.empty("w:picture", &[]),
        ControlKind::Gallery { list, gallery, category, unique } => {
            let tag = if *list { "w:docPartList" } else { "w:docPartObj" };
            w.open(tag, &[]);
            if !gallery.is_empty() {
                w.val("w:docPartGallery", gallery);
            }
            if !category.is_empty() {
                w.val("w:docPartCategory", category);
            }
            on(w, "w:docPartUnique", *unique);
            w.close(tag);
        }
        ControlKind::RepeatingSection { title, no_insert_delete } => {
            w.open("w15:repeatingSection", &[]);
            if !title.is_empty() {
                w.val("w15:sectionTitle", title);
            }
            on(w, "w15:doNotAllowInsertDeleteSection", *no_insert_delete);
            w.close("w15:repeatingSection");
        }
        ControlKind::RepeatingSectionItem => w.empty("w15:repeatingSectionItem", &[]),
    }
}
