use std::collections::BTreeMap;

use super::format::{date_text, excel_stamp, format_value, is_date_code};
use super::model::{Axis, Group, Grouping, Kind, Pos, Series, Stock};
use super::plot::label;
use super::{Chart, El, FONT, LINE_W, MAX_CELLS, MAX_POINTS, MAX_RADAR_SERIES, MAX_SERIES, MAX_SPOKES, Rgb, fill_colour, int, measure, slot};

/// Bound metadata before any repeated resolution, including groups with no series.
pub(super) const MAX_CHART_GROUPS: usize = MAX_SERIES;
pub(super) const MAX_CHART_AXES: usize = MAX_SERIES * 2;
pub(super) const MAX_AXIS_REFS: usize = 2;

#[cfg(test)]
#[derive(Clone, Copy, Default, Debug)]
pub(super) struct AxisWork {
    pub(super) groups: usize,
    pub(super) axes: usize,
    pub(super) lookups: usize,
}
#[cfg(test)]
std::thread_local! {
    static AXIS_WORK: std::cell::Cell<AxisWork> = const { std::cell::Cell::new(AxisWork { groups: 0, axes: 0, lookups: 0 }) };
}
#[cfg(test)]
fn record_axis_work(update: impl FnOnce(&mut AxisWork)) {
    AXIS_WORK.with(|cell| {
        let mut work = cell.get();
        update(&mut work);
        cell.set(work);
    });
}
#[cfg(test)]
pub(super) fn reset_axis_work() {
    AXIS_WORK.with(|cell| cell.set(AxisWork::default()));
}
#[cfg(test)]
pub(super) fn axis_work() -> AxisWork {
    AXIS_WORK.with(std::cell::Cell::get)
}

impl Chart {
    pub(super) fn parse(space: &El, theme: &[Rgb]) -> Option<Chart> {
        let chart = space.child("c:chart")?;
        let plot = chart.child("c:plotArea")?;
        let ty = plot.els().find(|e| Kind::of(&e.name).is_some())?;
        let kind = Kind::of(&ty.name)?;
        let scatter = kind == Kind::Scatter;
        let horizontal = kind == Kind::Bar && bar_across(ty);
        let date1904 = space.child("c:date1904").is_some_and(|d| d.attr("val").is_none_or(truthy));
        // Pie, doughnut and radar charts draw their first chart type alone; the others share the
        // frame with every chart type that fits it (scatters with scatters, bars across with bars
        // across, the rest together).
        let tys: Vec<(&El, Kind)> = if matches!(kind, Kind::Pie | Kind::Doughnut | Kind::Radar) {
            vec![(ty, kind)]
        } else {
            plot.els()
                .filter_map(|e| Some((e, Kind::of(&e.name)?)))
                .filter(|(e, k)| match k {
                    Kind::Scatter => scatter,
                    Kind::Bar => !scatter && bar_across(e) == horizontal,
                    Kind::Line | Kind::Area | Kind::Stock => !scatter && !horizontal,
                    Kind::Pie | Kind::Doughnut | Kind::Radar => false,
                })
                .take(MAX_CHART_GROUPS)
                .collect()
        };
        let (mut budget, mut left) = (MAX_CELLS, MAX_SERIES);
        // Keep the originating type beside its group through pruning. Rejected groups must
        // neither resolve axes nor choose an axis for the groups that remain.
        let mut admitted: Vec<(&El, Group)> = Vec::new();
        for (ty, k) in &tys {
            if left == 0 {
                break;
            }
            #[cfg(test)]
            record_axis_work(|w| w.groups = w.groups.saturating_add(1));
            let mut series = read_series_in(ty, *k == Kind::Scatter, theme, &mut budget, &mut left);
            if series.is_empty() {
                continue;
            }
            let type_labels = show_values(ty.child("c:dLbls"));
            for s in &mut series {
                s.labels = Some(s.labels.or(type_labels).unwrap_or(false));
            }
            let num = |n: &str| ty.child(n).and_then(|g| g.attr("val")).and_then(int);
            let grouping = match ty.child("c:grouping").and_then(|g| g.attr("val")) {
                Some("stacked") if matches!(k, Kind::Bar | Kind::Area | Kind::Line) => Grouping::Stacked,
                Some("percentStacked") if matches!(k, Kind::Bar | Kind::Area | Kind::Line) => Grouping::Percent,
                _ => Grouping::Clustered,
            };
            admitted.push((
                *ty,
                Group {
                    kind: *k,
                    grouping,
                    gap: num("c:gapWidth").unwrap_or(150).clamp(0, 500) as f32 / 100.0,
                    overlap: num("c:overlap").unwrap_or(0).clamp(-100, 100) as f32 / 100.0,
                    series,
                    secondary: false,
                    first: 0,
                    stock: if matches!(k, Kind::Line | Kind::Stock) { Stock::parse(ty, theme) } else { Stock::default() },
                },
            ));
        }
        if admitted.is_empty() {
            return None;
        }
        let cats = admitted
            .iter()
            .flat_map(|(ty, _)| ty.children("c:ser"))
            .find_map(|s| s.child("c:cat"))
            .map(|c| labels(Some(c), MAX_POINTS, date1904))
            .unwrap_or_default();
        let longest = admitted.iter().flat_map(|(_, g)| &g.series).map(|s| s.values.len()).max().unwrap_or(0);
        let n = if kind == Kind::Radar {
            longest.max(cats.len()).min(MAX_SPOKES)
        } else if matches!(kind, Kind::Pie | Kind::Doughnut) {
            admitted.first().and_then(|(_, g)| g.series.first()).map_or(0, |s| s.values.len()).max(cats.len())
        } else {
            longest.max(cats.len())
        };
        // Every series is padded to `n` points, so keep only as many as the cell budget holds.
        if let Some(keep) = MAX_CELLS.checked_div(n) {
            let mut keep = keep.max(1);
            for (_, g) in &mut admitted {
                g.series.truncate(keep);
                keep -= g.series.len();
            }
            admitted.retain(|(_, g)| !g.series.is_empty());
        }
        if kind == Kind::Radar {
            for (_, g) in &mut admitted {
                g.series.truncate(MAX_RADAR_SERIES);
            }
        }
        let mut cats = cats;
        cats.resize(n, String::new());
        let radar_style = ty.child("c:radarStyle").and_then(|r| r.attr("val"));
        let mut first = 0;
        for (_, g) in &mut admitted {
            for s in &mut g.series {
                s.values.resize(n, None);
                s.xs.resize(n, None);
                if s.labels == Some(true) {
                    s.raw = s.values.clone();
                }
            }
            if g.grouping == Grouping::Percent {
                to_percent(&mut g.series, n);
            }
            g.first = first;
            first += g.series.len();
        }
        let retained_types: Vec<(&El, Kind)> = admitted.iter().map(|(ty, g)| (*ty, g.kind)).collect();
        let (cat, val, sec, axes) = chart_axes(plot, scatter, &retained_types);
        let groups: Vec<Group> = admitted
            .into_iter()
            .zip(axes)
            .map(|((_, mut g), secondary)| {
                g.secondary = secondary;
                g
            })
            .collect();
        let sec = sec.filter(|_| groups.iter().any(|g| g.secondary));
        let val_fmt = axis_format(&val, groups.iter().filter(|g| !g.secondary));
        let sec_fmt = sec.as_ref().map_or_else(String::new, |a| axis_format(a, groups.iter().filter(|g| g.secondary)));
        let num = |n: &str| ty.child(n).and_then(|g| g.attr("val")).and_then(int);
        let title = match groups.as_slice() {
            [g] => title_of(chart, &g.series),
            _ => title_of(chart, &[]),
        };
        Some(Chart {
            kind,
            horizontal,
            groups,
            hole: num("c:holeSize").unwrap_or(50).clamp(10, 90) as f32 / 100.0,
            first_angle: num("c:firstSliceAng").unwrap_or(0).clamp(0, 360) as f64,
            radar_fill: radar_style == Some("filled"),
            radar_marks: radar_style == Some("marker"),
            vary: matches!(kind, Kind::Pie | Kind::Doughnut) && ty.child("c:varyColors").and_then(|v| v.attr("val")).is_none_or(truthy),
            n,
            cats,
            title,
            accents: (4..10).map(|i| slot(theme, i)).collect(),
            legend: legend_pos(chart),
            cat,
            val,
            sec,
            text_size: text_size(space),
            val_fmt,
            sec_fmt,
            date1904,
            plot_border: plot.child("c:spPr").and_then(|p| p.child("a:ln")).and_then(|l| fill_colour(l, theme)),
        })
    }

    pub(super) fn stacked(&self) -> bool {
        self.groups.first().is_some_and(Group::stacked)
    }

    fn accent(&self, i: usize) -> Rgb {
        self.accents.get(i % 6).copied().unwrap_or(Rgb::BLACK)
    }

    /// Colour of series `j` of group `g`: its own, else the accent its `c:idx` picks (its place
    /// among all the chart's series when it has none).
    pub(super) fn series_colour(&self, g: &Group, j: usize) -> Rgb {
        let s = g.series.get(j);
        s.and_then(|s| s.color).unwrap_or_else(|| self.accent(s.and_then(|s| s.idx).unwrap_or(g.first + j)))
    }

    /// Colour of point `i` of series `j` of group `g`: its `c:dPt` override, else the varied
    /// accent (pie) or the series colour.
    pub(super) fn point_colour(&self, g: &Group, j: usize, i: usize) -> Rgb {
        if let Some(Some(c)) = g.series.get(j).and_then(|s| s.points.get(i)) {
            return *c;
        }
        if self.vary { self.accent(i) } else { self.series_colour(g, j) }
    }

    pub(super) fn legend_entries(&self) -> Vec<(String, Rgb)> {
        if matches!(self.kind, Kind::Pie | Kind::Doughnut) {
            let Some(g) = self.groups.first() else { return Vec::new() };
            return self
                .cats
                .iter()
                .enumerate()
                .map(|(i, c)| (if c.is_empty() { (i + 1).to_string() } else { c.clone() }, self.point_colour(g, 0, i)))
                .collect();
        }
        let mut entries: Vec<(i64, String, Rgb)> = self
            .groups
            .iter()
            .flat_map(|g| g.series.iter().enumerate().map(move |(j, s)| (g, j, s)))
            .map(|(g, j, s)| {
                let k = g.first + j;
                let name = if s.name.is_empty() { format!("Series {}", k + 1) } else { s.name.clone() };
                (s.order.unwrap_or(k as i64), name, self.series_colour(g, j))
            })
            .collect();
        // Several chart types: one list in the series' order across them.
        if self.groups.len() > 1 {
            entries.sort_by_key(|e| e.0);
        }
        entries.into_iter().map(|(_, name, c)| (name, c)).collect()
    }
}

/// Whether a `c:barChart` draws bars across (`c:barDir` = `bar`), not columns.
fn bar_across(ty: &El) -> bool {
    ty.child("c:barDir").and_then(|d| d.attr("val")) == Some("bar")
}

/// The series of a chart type element, within the chart's cell budget.
pub(super) fn read_series(ty: &El, scatter: bool, theme: &[Rgb]) -> Vec<Series> {
    let (mut budget, mut left) = (MAX_CELLS, MAX_SERIES);
    read_series_in(ty, scatter, theme, &mut budget, &mut left)
}

/// The series of a chart type element within what is `left` of the chart's cell `budget` and
/// series count, which they use up.
fn read_series_in(ty: &El, scatter: bool, theme: &[Rgb], budget: &mut usize, left: &mut usize) -> Vec<Series> {
    let mut series = Vec::new();
    for ser in ty.children("c:ser").take(*left) {
        let s = Series::parse(ser, scatter, theme, (*budget).min(MAX_POINTS));
        *budget = budget.saturating_sub(s.values.len().max(s.xs.len()));
        series.push(s);
    }
    *left -= series.len();
    series
}

/// Whether this axis contains only percent-stacked groups. Mixed groups share a numeric axis.
pub(super) fn only_percent<'a>(mut groups: impl Iterator<Item = &'a Group>) -> bool {
    let Some(first) = groups.next() else { return false };
    first.grouping == Grouping::Percent && groups.all(|g| g.grouping == Grouping::Percent)
}

/// Explicit axis formatting wins; percentage is only a default for a wholly percent axis.
fn axis_format<'a>(axis: &Axis, mut groups: impl Iterator<Item = &'a Group> + Clone) -> String {
    if let Some(fmt) = &axis.fmt {
        return fmt.clone();
    }
    if only_percent(groups.clone()) {
        return "0%".to_string();
    }
    groups.find_map(|g| g.series.iter().find_map(|s| s.fmt.clone())).unwrap_or_else(|| "General".to_string())
}

/// Values as their share of each category's total (100% stacked charts).
fn to_percent(series: &mut [Series], n: usize) {
    for i in 0..n {
        let total: f64 = series.iter().map(|s| s.values.get(i).copied().flatten().map_or(0.0, f64::abs)).sum();
        for s in series.iter_mut() {
            if let Some(Some(v)) = s.values.get_mut(i) {
                *v = if total > 0.0 { *v / total } else { 0.0 };
            }
        }
    }
}

/// The category axis (scatter: the horizontal value axis) and the (primary) value axis of a plot
/// area, the secondary value axis (that of the first chart type not on the primary one) and for
/// each chart type whether it is on it.
fn chart_axes(plot: &El, scatter: bool, tys: &[(&El, Kind)]) -> (Axis, Axis, Option<Axis>, Vec<bool>) {
    let values = ValueAxes::new(plot);
    let cat_el = plot.els().find(|e| matches!(e.name.as_str(), "c:catAx" | "c:dateAx"));
    if scatter {
        let x = values.axes.iter().position(|a| a.horizontal).or_else(|| (!values.axes.is_empty()).then_some(0));
        let y = values.axes.iter().position(|a| !a.horizontal).or_else(|| (0..values.axes.len()).find(|i| Some(*i) != x));
        return (axis(values.element(x)), axis(values.element(y)), None, vec![false; tys.len()]);
    }
    let refs: Vec<Option<usize>> = tys.iter().map(|(ty, _)| values.value_axis_of(ty)).collect();
    // Resolve only admitted groups; prefer their first referenced axis not on the right.
    // Metadata is cached once, so a wide axis element is never rescanned for each group.
    let primary = refs
        .iter()
        .flatten()
        .copied()
        .find(|i| values.axes.get(*i).is_some_and(|a| !a.right))
        .or_else(|| refs.iter().flatten().copied().next())
        .or_else(|| (!values.axes.is_empty()).then_some(0));
    let secondary: Vec<bool> = refs.iter().map(|r| tys.len() > 1 && r.is_some() && *r != primary).collect();
    let sec = refs.iter().zip(&secondary).find(|(_, s)| **s).map(|(r, _)| axis(values.element(*r)));
    (axis(cat_el), axis(values.element(primary)), sec, secondary)
}

struct ValueAxis<'a> {
    el: &'a El,
    horizontal: bool,
    right: bool,
}

/// At most 512 axes indexed once; at most two integer-key lookups per admitted group.
struct ValueAxes<'a> {
    axes: Vec<ValueAxis<'a>>,
    by_id: BTreeMap<u32, usize>,
}

impl<'a> ValueAxes<'a> {
    fn new(plot: &'a El) -> Self {
        let mut axes = Vec::new();
        let mut by_id = BTreeMap::new();
        for el in plot.children("c:valAx").take(MAX_CHART_AXES) {
            #[cfg(test)]
            record_axis_work(|w| w.axes = w.axes.saturating_add(1));
            if let Some(id) = el.child("c:axId").and_then(axis_id) {
                // Duplicate IDs retain the first axis in XML order.
                by_id.entry(id).or_insert(axes.len());
            }
            let pos = el.child("c:axPos").and_then(|p| p.attr("val"));
            axes.push(ValueAxis { el, horizontal: matches!(pos, Some("b" | "t")), right: pos == Some("r") });
        }
        Self { axes, by_id }
    }

    fn element(&self, index: Option<usize>) -> Option<&'a El> {
        index.and_then(|i| self.axes.get(i)).map(|a| a.el)
    }

    fn value_axis_of(&self, ty: &El) -> Option<usize> {
        ty.children("c:axId")
            .take(MAX_AXIS_REFS)
            .filter_map(|id| {
                #[cfg(test)]
                record_axis_work(|w| w.lookups = w.lookups.saturating_add(1));
                axis_id(id).and_then(|id| self.by_id.get(&id).copied())
            })
            .min()
    }
}

fn axis_id(el: &El) -> Option<u32> {
    el.attr("val").and_then(|v| v.trim().parse::<u32>().ok())
}

/// The chart's title. An empty `c:title` shows the series name (Word's automatic title of a
/// one-series chart) unless the automatic title is deleted.
pub(super) fn title_of(chart: &El, series: &[Series]) -> Option<String> {
    let auto_deleted = chart.child("c:autoTitleDeleted").and_then(|a| a.attr("val")).is_some_and(truthy);
    let auto = if !auto_deleted && series.len() == 1 { series.first().map(|s| s.name.clone()).filter(|s| !s.is_empty()) } else { None };
    chart.child("c:title").and_then(|t| title_text(t).or(auto))
}

/// Whether a `c:dLbls` shows the values (`None` when there's none, or it doesn't say).
fn show_values(d: Option<&El>) -> Option<bool> {
    let d = d?;
    if d.child("c:delete").and_then(|e| e.attr("val")).is_some_and(truthy) {
        return Some(false);
    }
    d.child("c:showVal").map(|v| v.attr("val").is_none_or(truthy))
}

fn legend_pos(chart: &El) -> Option<Pos> {
    chart.child("c:legend").map(|l| match l.child("c:legendPos").and_then(|p| p.attr("val")).unwrap_or("r") {
        "l" => Pos::Left,
        "t" | "tr" => Pos::Top,
        "b" => Pos::Bottom,
        _ => Pos::Right,
    })
}

/// The chart's text size (its `c:txPr` default run size), points.
pub(super) fn text_size(space: &El) -> f32 {
    space
        .child("c:txPr")
        .and_then(|t| t.find("a:defRPr"))
        .and_then(|d| d.attr("sz"))
        .and_then(int)
        .map_or(FONT, |v| (v as f32 / 100.0).clamp(6.0, 24.0))
}

impl Series {
    fn parse(ser: &El, scatter: bool, theme: &[Rgb], cap: usize) -> Series {
        let name = ser.child("c:tx").and_then(|t| t.find("c:v")).map(|v| label(v.text().trim())).unwrap_or_default();
        let sp = ser.child("c:spPr");
        let ln = sp.and_then(|p| p.child("a:ln"));
        let marker = ser.child("c:marker").and_then(|m| m.child("c:spPr")).and_then(|p| fill_colour(p, theme));
        let color = sp.and_then(|p| fill_colour(p, theme)).or_else(|| ln.and_then(|l| fill_colour(l, theme))).or(marker);
        let line_w = ln.and_then(|l| l.attr("w")).and_then(|v| measure(v, 12_700.0)).map_or(LINE_W, |w| w.clamp(0.0, 20.0));
        let line = ln.is_none_or(|l| l.child("a:noFill").is_none());
        let (values, fmt) = numbers(ser.child(if scatter { "c:yVal" } else { "c:val" }), cap);
        let (mut xs, x_fmt) = if scatter { numbers(ser.child("c:xVal"), cap) } else { (Vec::new(), None) };
        if scatter && xs.is_empty() {
            xs = (1..=values.len()).map(|i| Some(i as f64)).collect();
        }
        let mut points = vec![None; values.len()];
        for dpt in ser.children("c:dPt") {
            let i = dpt.child("c:idx").and_then(|e| e.attr("val")).and_then(int).and_then(|i| usize::try_from(i).ok());
            let c = dpt.child("c:spPr").and_then(|p| fill_colour(p, theme));
            if let Some(slot) = i.and_then(|i| points.get_mut(i)) {
                *slot = c;
            }
        }
        let labels = show_values(ser.child("c:dLbls"));
        let idx = ser.child("c:idx").and_then(|e| e.attr("val")).and_then(|v| v.trim().parse::<u32>().ok()).and_then(|v| usize::try_from(v).ok());
        let order = ser.child("c:order").and_then(|e| e.attr("val")).and_then(|v| v.trim().parse::<u32>().ok()).map(i64::from);
        Series { name, idx, order, color, line_w, line, values, xs, points, fmt, x_fmt, labels, raw: Vec::new() }
    }
}

/// Longest number-format code honoured (the limit spreadsheet apps use); longer ones are ignored.
const MAX_FORMAT_CODE: usize = 255;

/// Category labels of a `c:cat` (string or number cache), capped. Numbers show in the cache's
/// format code (dates included); text and `General` numbers as they are stored.
pub(super) fn labels(el: Option<&El>, cap: usize, date1904: bool) -> Vec<String> {
    let Some(el) = el else { return Vec::new() };
    let Some(cache) = el.find("c:strCache").or_else(|| el.find("c:numCache")).or_else(|| el.find("c:strLit")).or_else(|| el.find("c:numLit")) else {
        return Vec::new();
    };
    let code =
        cache.child("c:formatCode").map(|f| f.text()).filter(|f| !f.is_empty() && f.len() <= MAX_FORMAT_CODE && !f.eq_ignore_ascii_case("General"));
    pts(cache, cap)
        .into_iter()
        .map(|p| {
            let text = p.unwrap_or_default();
            match (code.as_deref(), text.trim().parse::<f64>()) {
                (Some(code), Ok(v)) if v.is_finite() => label(&category_text(v, code, 1.0, date1904)),
                _ => label(&text),
            }
        })
        .collect()
}

/// A number in its format code: a date or time for date codes (serial days, from 1904 when
/// `date1904`), else as [`format_value`] with tick step `step`.
pub(super) fn category_text(v: f64, code: &str, step: f64, date1904: bool) -> String {
    match excel_stamp(v, date1904) {
        Some(t) if is_date_code(code) => date_text(t, code),
        _ => format_value(v, code, step),
    }
}

/// Values of a `c:val`-style element (number cache or literal), capped; with its format code.
pub(super) fn numbers(el: Option<&El>, cap: usize) -> (Vec<Option<f64>>, Option<String>) {
    let Some(cache) = el.and_then(|e| e.find("c:numCache").or_else(|| e.find("c:numLit"))) else {
        return (Vec::new(), None);
    };
    let fmt = cache.child("c:formatCode").map(|f| f.text()).filter(|f| !f.is_empty() && f.len() <= MAX_FORMAT_CODE);
    let vals = pts(cache, cap).into_iter().map(|p| p.and_then(|t| t.trim().parse::<f64>().ok()).filter(|v| v.is_finite())).collect();
    (vals, fmt)
}

/// The `c:pt` texts of a cache by index. Indexes and counts from the file are capped.
fn pts(cache: &El, cap: usize) -> Vec<Option<String>> {
    let declared = cache.child("c:ptCount").and_then(|c| c.attr("val")).and_then(int).unwrap_or(0).clamp(0, cap as i64) as usize;
    let mut out: Vec<Option<String>> = vec![None; declared];
    for p in cache.children("c:pt") {
        let Some(i) = p.attr("idx").and_then(int).and_then(|i| usize::try_from(i).ok()) else { continue };
        if i >= cap {
            continue;
        }
        if out.len() <= i {
            out.resize(i + 1, None);
        }
        if let Some(slot) = out.get_mut(i) {
            *slot = p.child("c:v").map(|v| v.text());
        }
    }
    out
}

pub(super) fn axis(a: Option<&El>) -> Axis {
    let Some(a) = a else { return Axis { deleted: true, ..Default::default() } };
    let scaling = a.child("c:scaling");
    let num =
        |n: &str| scaling.and_then(|s| s.child(n)).and_then(|e| e.attr("val")).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
    Axis {
        deleted: a.child("c:delete").and_then(|d| d.attr("val")).is_some_and(truthy),
        grid: a.child("c:majorGridlines").is_some(),
        line: a.child("c:spPr").and_then(|p| p.child("a:ln")).and_then(|l| l.child("a:noFill")).is_none(),
        mid: a.child("c:crossBetween").and_then(|c| c.attr("val")) == Some("midCat"),
        min: num("c:min"),
        max: num("c:max"),
        fmt: a
            .child("c:numFmt")
            .filter(|f| f.attr("sourceLinked") != Some("1"))
            .and_then(|f| f.attr("formatCode"))
            .filter(|f| f.len() <= MAX_FORMAT_CODE)
            .map(str::to_string),
    }
}

fn title_text(t: &El) -> Option<String> {
    let tx = t.child("c:tx")?;
    let s = match tx.child("c:rich") {
        Some(rich) => {
            let mut s = String::new();
            rich_into(rich, &mut s);
            s
        }
        None => tx.find("c:v").map(|v| v.text()).unwrap_or_default(),
    };
    let s = label(s.trim());
    (!s.is_empty()).then_some(s)
}

/// The `a:t` runs under an element, in order.
fn rich_into(e: &El, s: &mut String) {
    for c in e.els() {
        if c.name == "a:t" {
            s.push_str(&c.text());
        } else {
            rich_into(c, s);
        }
    }
}

fn truthy(v: &str) -> bool {
    matches!(v.trim(), "1" | "true")
}
