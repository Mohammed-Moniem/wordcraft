//! Combo charts (several chart-type groups in one plot area, a secondary value axis) and stock
//! charts (hi-low lines, up/down bars): ECMA-376 Part 1 §21.2.2.145, §21.2.2.198, §21.2.2.218.

use super::*;
use wordcraft_doc::THEME_COLORS;

const NS: &str =
    r#"xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main""#;

const W: f32 = 400.0;
const H: f32 = 300.0;

fn draw(plot: &str, legend: bool) -> Vec<GraphicItem> {
    let legend = if legend { r#"<c:legend><c:legendPos val="b"/></c:legend>"# } else { "" };
    let x = format!(r#"<c:chartSpace {NS}><c:chart><c:plotArea>{plot}</c:plotArea>{legend}</c:chart></c:chartSpace>"#);
    let space = crate::xml::parse(x.as_bytes()).unwrap();
    chart_items(&space, &THEME_COLORS, W, H)
}

/// A series with string categories, values and an optional `c:spPr` body.
fn ser(idx: usize, name: &str, cats: &[&str], vals: &[f64], sppr: &str) -> String {
    let cat_pts: String = cats.iter().enumerate().map(|(k, c)| format!(r#"<c:pt idx="{k}"><c:v>{c}</c:v></c:pt>"#)).collect();
    let val_pts: String = vals.iter().enumerate().map(|(k, v)| format!(r#"<c:pt idx="{k}"><c:v>{v}</c:v></c:pt>"#)).collect();
    let sppr = if sppr.is_empty() { String::new() } else { format!("<c:spPr>{sppr}</c:spPr>") };
    format!(
        r#"<c:ser><c:idx val="{idx}"/><c:order val="{idx}"/><c:tx><c:strRef><c:strCache><c:ptCount val="1"/><c:pt idx="0"><c:v>{name}</c:v></c:pt></c:strCache></c:strRef></c:tx>{sppr}<c:cat><c:strRef><c:strCache><c:ptCount val="{nc}"/>{cat_pts}</c:strCache></c:strRef></c:cat><c:val><c:numRef><c:numCache><c:formatCode>General</c:formatCode><c:ptCount val="{nv}"/>{val_pts}</c:numCache></c:numRef></c:val></c:ser>"#,
        nc = cats.len(),
        nv = vals.len()
    )
}

fn solid(hex: &str) -> String {
    format!(r#"<a:solidFill><a:srgbClr val="{hex}"/></a:solidFill>"#)
}

fn no_line() -> &'static str {
    "<a:ln><a:noFill/></a:ln>"
}

fn bar_group(series: &str, ax: (u32, u32)) -> String {
    format!(
        r#"<c:barChart><c:barDir val="col"/><c:grouping val="clustered"/>{series}<c:gapWidth val="150"/><c:axId val="{}"/><c:axId val="{}"/></c:barChart>"#,
        ax.0, ax.1
    )
}

fn line_group(series: &str, ax: (u32, u32)) -> String {
    format!(r#"<c:lineChart><c:grouping val="standard"/>{series}<c:marker val="1"/><c:axId val="{}"/><c:axId val="{}"/></c:lineChart>"#, ax.0, ax.1)
}

fn area_group(series: &str, ax: (u32, u32)) -> String {
    format!(r#"<c:areaChart><c:grouping val="standard"/>{series}<c:axId val="{}"/><c:axId val="{}"/></c:areaChart>"#, ax.0, ax.1)
}

fn cat_ax(id: u32, cross: u32) -> String {
    format!(
        r#"<c:catAx><c:axId val="{id}"/><c:scaling><c:orientation val="minMax"/></c:scaling><c:delete val="0"/><c:axPos val="b"/><c:crossAx val="{cross}"/><c:crossBetween val="between"/></c:catAx>"#
    )
}

/// A value axis; `right` makes it a secondary axis on the right that crosses at the maximum.
fn val_ax(id: u32, cross: u32, right: bool, max: Option<f64>) -> String {
    let scaling = match max {
        Some(m) => format!(r#"<c:scaling><c:orientation val="minMax"/><c:max val="{m}"/><c:min val="0"/></c:scaling>"#),
        None => r#"<c:scaling><c:orientation val="minMax"/></c:scaling>"#.to_string(),
    };
    let (pos, crosses) = if right { ("r", "max") } else { ("l", "autoZero") };
    format!(
        r#"<c:valAx><c:axId val="{id}"/>{scaling}<c:delete val="0"/><c:axPos val="{pos}"/><c:majorGridlines/><c:numFmt formatCode="General" sourceLinked="1"/><c:crossAx val="{cross}"/><c:crosses val="{crosses}"/><c:crossBetween val="between"/></c:valAx>"#
    )
}

/// Filled paths: (colour, min x, min y, max x, max y, index in the item list).
fn fills(items: &[GraphicItem]) -> Vec<(Rgb, f32, f32, f32, f32, usize)> {
    items
        .iter()
        .enumerate()
        .filter_map(|(k, it)| match it {
            GraphicItem::Path { segs, fill: Some(c), .. } => {
                let p = points(segs);
                let xs = p.iter().map(|q| q.0);
                let ys = p.iter().map(|q| q.1);
                Some((
                    *c,
                    xs.clone().fold(f32::INFINITY, f32::min),
                    ys.clone().fold(f32::INFINITY, f32::min),
                    xs.fold(f32::NEG_INFINITY, f32::max),
                    ys.fold(f32::NEG_INFINITY, f32::max),
                    k,
                ))
            }
            _ => None,
        })
        .collect()
}

/// Stroked, unfilled paths: (colour, their points, index in the item list).
fn strokes(items: &[GraphicItem]) -> Vec<(Rgb, Vec<(f32, f32)>, usize)> {
    items
        .iter()
        .enumerate()
        .filter_map(|(k, it)| match it {
            GraphicItem::Path { segs, fill: None, stroke: Some(c), .. } => Some((*c, points(segs), k)),
            _ => None,
        })
        .collect()
}

fn points(segs: &[PathSeg]) -> Vec<(f32, f32)> {
    segs.iter()
        .filter_map(|s| match s {
            PathSeg::Move(x, y) | PathSeg::Line(x, y) => Some((*x, *y)),
            _ => None,
        })
        .collect()
}

fn texts(items: &[GraphicItem]) -> Vec<(String, f32)> {
    items
        .iter()
        .filter_map(|it| match it {
            GraphicItem::Text { text, rect, .. } => Some((text.clone(), rect[0])),
            _ => None,
        })
        .collect()
}

const CATS: [&str; 3] = ["Q1", "Q2", "Q3"];
const BAR: &str = "4472C4";
const LINE: &str = "ED7D31";

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.05
}

#[test]
fn combo_draws_the_line_group_on_the_bar_groups_scale() {
    let plot = format!(
        "{}{}{}{}",
        bar_group(&ser(0, "Sales", &CATS, &[10.0, 20.0, 30.0], &solid(BAR)), (1, 2)),
        line_group(&ser(1, "Average", &CATS, &[15.0, 25.0, 5.0], &format!("<a:ln w=\"28575\">{}</a:ln>", solid(LINE))), (1, 2)),
        cat_ax(1, 2),
        val_ax(2, 1, false, None)
    );
    let items = draw(&plot, false);
    let bars: Vec<_> = fills(&items).into_iter().filter(|f| f.0 == Rgb(0x44, 0x72, 0xC4)).collect();
    assert_eq!(bars.len(), 3, "three columns");
    let line: Vec<_> = strokes(&items).into_iter().filter(|s| s.0 == Rgb(0xED, 0x7D, 0x31)).collect();
    assert_eq!(line.len(), 1, "the line series of the combo is drawn: {:?}", strokes(&items));
    let pts = &line[0].1;
    assert_eq!(pts.len(), 3);
    // Same category places: each line point at its column's centre.
    for (i, b) in bars.iter().enumerate() {
        assert!(near(pts[i].0, (b.1 + b.3) / 2.0), "point {i} x {} vs column centre {}", pts[i].0, (b.1 + b.3) / 2.0);
    }
    // Same value scale: the baseline is the columns' bottom, the 20 column's top gives the slope.
    let base = bars[1].4;
    let per = (base - bars[1].2) / 20.0;
    for (i, v) in [15.0f32, 25.0, 5.0].iter().enumerate() {
        assert!(near(pts[i].1, base - per * v), "point {i} y {} vs {}", pts[i].1, base - per * v);
    }
}

#[test]
fn combo_layers_areas_then_bars_then_lines_whatever_the_xml_order() {
    let plot = format!(
        "{}{}{}{}{}",
        line_group(&ser(2, "Line", &CATS, &[5.0, 6.0, 7.0], &format!("<a:ln>{}</a:ln>", solid(LINE))), (1, 2)),
        bar_group(&ser(1, "Bars", &CATS, &[3.0, 4.0, 5.0], &solid(BAR)), (1, 2)),
        area_group(&ser(0, "Area", &CATS, &[1.0, 2.0, 3.0], &solid("A5A5A5")), (1, 2)),
        cat_ax(1, 2),
        val_ax(2, 1, false, None)
    );
    let items = draw(&plot, false);
    let f = fills(&items);
    let area = f.iter().filter(|x| x.0 == Rgb(0xA5, 0xA5, 0xA5)).map(|x| x.5).max().expect("area drawn");
    let bars: Vec<usize> = f.iter().filter(|x| x.0 == Rgb(0x44, 0x72, 0xC4)).map(|x| x.5).collect();
    assert_eq!(bars.len(), 3, "bars drawn");
    let line = strokes(&items).into_iter().find(|s| s.0 == Rgb(0xED, 0x7D, 0x31)).expect("line drawn").2;
    assert!(area < *bars.iter().min().unwrap(), "area behind the bars");
    assert!(line > *bars.iter().max().unwrap(), "line in front of the bars");
}

#[test]
fn combo_legend_lists_every_group_and_colours_series_by_their_index() {
    let plot = format!(
        "{}{}{}{}",
        bar_group(&ser(0, "Sales", &CATS, &[10.0, 20.0, 30.0], ""), (1, 2)),
        line_group(&ser(1, "Average", &CATS, &[15.0, 25.0, 5.0], ""), (1, 2)),
        cat_ax(1, 2),
        val_ax(2, 1, false, None)
    );
    let items = draw(&plot, true);
    let t: Vec<String> = texts(&items).into_iter().map(|t| t.0).collect();
    assert!(t.iter().any(|s| s == "Sales") && t.iter().any(|s| s == "Average"), "legend {t:?}");
    // The default colours: what a plain two-series column chart gives its first and second series.
    let reference = draw(
        &format!(
            "{}{}{}",
            bar_group(&format!("{}{}", ser(0, "A", &CATS, &[1.0, 2.0, 3.0], ""), ser(1, "B", &CATS, &[1.0, 2.0, 3.0], "")), (1, 2)),
            cat_ax(1, 2),
            val_ax(2, 1, false, None)
        ),
        false,
    );
    let rf = fills(&reference);
    let first = rf.iter().find(|f| f.1 > 0.0 && f.3 - f.1 < W / 2.0).expect("a column").0;
    let second = rf.iter().map(|f| f.0).find(|c| *c != first).expect("a second series colour");
    let bar = fills(&items).into_iter().find(|f| f.0 == first).map(|f| f.0);
    assert_eq!(bar, Some(first), "the bar series (idx 0) keeps accent 1");
    assert!(strokes(&items).iter().any(|s| s.0 == second && s.1.len() == 3), "the line series (idx 1) gets accent 2");
}

#[test]
fn combo_line_on_a_secondary_axis_gets_its_own_scale_and_right_hand_labels() {
    let plot = format!(
        "{}{}{}{}{}{}",
        bar_group(&ser(0, "Units", &CATS, &[100.0, 200.0, 300.0], &solid(BAR)), (1, 2)),
        line_group(&ser(1, "Margin", &CATS, &[0.1, 0.2, 0.3], &format!("<a:ln>{}</a:ln>", solid(LINE))), (3, 4)),
        cat_ax(1, 2),
        val_ax(2, 1, false, Some(400.0)),
        // The secondary category axis Word writes alongside (deleted).
        r#"<c:catAx><c:axId val="3"/><c:scaling><c:orientation val="minMax"/></c:scaling><c:delete val="1"/><c:axPos val="b"/><c:crossAx val="4"/></c:catAx>"#,
        val_ax(4, 3, true, Some(0.4))
    );
    let items = draw(&plot, false);
    let bars: Vec<_> = fills(&items).into_iter().filter(|f| f.0 == Rgb(0x44, 0x72, 0xC4)).collect();
    assert_eq!(bars.len(), 3);
    let line = strokes(&items).into_iter().find(|s| s.0 == Rgb(0xED, 0x7D, 0x31)).expect("line drawn").1;
    // 0.1 / 0.4 of the secondary axis is as high as 100 / 400 of the primary one.
    for (i, (p, b)) in line.iter().zip(&bars).enumerate() {
        assert!(near(p.1, b.2), "point {i} y {} vs column top {}", p.1, b.2);
    }
    let right_edge = bars.iter().map(|b| b.3).fold(0.0f32, f32::max);
    let t = texts(&items);
    assert!(t.iter().any(|(s, x)| s == "0.4" && *x > right_edge), "secondary labels right of the plot: {t:?}");
    assert!(t.iter().any(|(s, x)| s == "400" && *x < bars[0].1), "primary labels stay on the left: {t:?}");
}

fn stock(series: &str, extra: &str) -> String {
    format!("<c:stockChart>{series}{extra}<c:axId val=\"1\"/><c:axId val=\"2\"/></c:stockChart>{}{}", cat_ax(1, 2), val_ax(2, 1, false, Some(40.0)))
}

#[test]
fn stock_chart_draws_hi_low_lines_not_a_placeholder() {
    let series = format!(
        "{}{}{}",
        ser(0, "High", &CATS, &[30.0, 35.0, 25.0], no_line()),
        ser(1, "Low", &CATS, &[10.0, 20.0, 5.0], no_line()),
        ser(2, "Close", &CATS, &[20.0, 22.0, 15.0], no_line())
    );
    let items = draw(&stock(&series, &format!("<c:hiLowLines><c:spPr><a:ln w=\"9525\">{}</a:ln></c:spPr></c:hiLowLines>", solid("000000"))), false);
    assert!(!texts(&items).iter().any(|t| t.0.contains("not supported")), "no placeholder");
    let hl: Vec<_> = strokes(&items).into_iter().filter(|s| s.0 == Rgb::BLACK).collect();
    assert_eq!(hl.len(), 3, "one hi-low line per category: {hl:?}");
    // Vertical, ordered left to right, lengths in proportion to high - low (20, 15, 20).
    let len: Vec<f32> = hl
        .iter()
        .map(|s| {
            assert_eq!(s.1.len(), 2);
            assert!(near(s.1[0].0, s.1[1].0), "vertical: {:?}", s.1);
            (s.1[0].1 - s.1[1].1).abs()
        })
        .collect();
    assert!(hl[0].1[0].0 < hl[1].1[0].0 && hl[1].1[0].0 < hl[2].1[0].0);
    let per = len[0] / 20.0;
    assert!(near(len[1], per * 15.0) && near(len[2], per * 20.0), "{len:?}");
    // The lowest end of the 3rd line is 5, the top of the 2nd is 35: 30 apart on the shared scale.
    let low3 = hl[2].1.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);
    let high2 = hl[1].1.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
    assert!(near(low3 - high2, per * 30.0), "{low3} {high2}");
    // The series lines have `a:ln/a:noFill`: nothing else stroked in the series colours.
    let accents: Vec<Rgb> = THEME_COLORS.to_vec();
    assert!(strokes(&items).iter().all(|s| s.0 == Rgb::BLACK || !accents.contains(&s.0) || s.1.len() <= 2), "no series lines");
}

#[test]
fn stock_chart_up_down_bars_span_open_to_close() {
    let series = format!(
        "{}{}{}{}",
        ser(0, "Open", &CATS[..2], &[10.0, 30.0], no_line()),
        ser(1, "High", &CATS[..2], &[25.0, 35.0], no_line()),
        ser(2, "Low", &CATS[..2], &[5.0, 12.0], no_line()),
        ser(3, "Close", &CATS[..2], &[20.0, 15.0], no_line())
    );
    let extra = format!(
        r#"<c:hiLowLines/><c:upDownBars><c:gapWidth val="150"/><c:upBars><c:spPr>{}</c:spPr></c:upBars><c:downBars><c:spPr>{}</c:spPr></c:downBars></c:upDownBars>"#,
        solid("00FF00"),
        solid("FF0000")
    );
    let items = draw(&stock(&series, &extra), false);
    let f = fills(&items);
    let up: Vec<_> = f.iter().filter(|x| x.0 == Rgb(0, 0xFF, 0)).collect();
    let down: Vec<_> = f.iter().filter(|x| x.0 == Rgb(0xFF, 0, 0)).collect();
    assert_eq!((up.len(), down.len()), (1, 1), "an up bar for Q1 (10 -> 20), a down bar for Q2 (30 -> 15)");
    let (u, d) = (up[0], down[0]);
    // Heights 10 and 15 on the same scale.
    assert!(near((u.4 - u.2) / (d.4 - d.2), 10.0 / 15.0), "heights {} {}", u.4 - u.2, d.4 - d.2);
    // gapWidth 150: a bar is band / 2.5 wide, centred on its category.
    let band = (d.1 + d.3) / 2.0 - (u.1 + u.3) / 2.0;
    assert!(band > 0.0);
    assert!(near((u.3 - u.1) / band, 0.4) && near((d.3 - d.1) / band, 0.4), "widths {} {} band {band}", u.3 - u.1, d.3 - d.1);
    // The bars are on top of the hi-low lines.
    let hl =
        strokes(&items).into_iter().filter(|s| s.1.len() == 2 && near(s.1[0].0, s.1[1].0) && near(s.1[0].0, (u.1 + u.3) / 2.0)).map(|s| s.2).max();
    assert!(hl.is_some_and(|k| k < u.5), "Q1 hi-low line drawn before (under) its up bar");
}

#[test]
fn deleted_secondary_axis_keeps_its_scale_but_draws_no_labels() {
    let deleted = val_ax(4, 3, true, Some(0.4)).replace(r#"<c:delete val="0"/>"#, r#"<c:delete val="1"/>"#);
    let plot = format!(
        "{}{}{}{}{}",
        bar_group(&ser(0, "Units", &CATS, &[100.0, 200.0, 300.0], &solid(BAR)), (1, 2)),
        line_group(&ser(1, "Margin", &CATS, &[0.1, 0.2, 0.3], &format!("<a:ln>{}</a:ln>", solid(LINE))), (3, 4)),
        cat_ax(1, 2),
        val_ax(2, 1, false, Some(400.0)),
        deleted
    );
    let items = draw(&plot, false);
    let bars: Vec<_> = fills(&items).into_iter().filter(|f| f.0 == Rgb(0x44, 0x72, 0xC4)).collect();
    let line = strokes(&items).into_iter().find(|s| s.0 == Rgb(0xED, 0x7D, 0x31)).expect("line drawn").1;
    for (p, b) in line.iter().zip(&bars) {
        assert!(near(p.1, b.2), "secondary scale kept: {} vs {}", p.1, b.2);
    }
    let t = texts(&items);
    assert!(!t.iter().any(|(s, _)| s == "0.4"), "no secondary labels: {t:?}");
}

#[test]
fn stock_chart_without_hi_low_lines_draws_its_series_lines() {
    let series = format!(
        "{}{}{}",
        ser(0, "High", &CATS, &[30.0, 35.0, 25.0], ""),
        ser(1, "Low", &CATS, &[10.0, 20.0, 5.0], ""),
        ser(2, "Close", &CATS, &[20.0, 22.0, 15.0], "")
    );
    let items = draw(&stock(&series, ""), false);
    assert!(!texts(&items).iter().any(|t| t.0.contains("not supported")));
    let three: Vec<_> = strokes(&items).into_iter().filter(|s| s.1.len() == 3).collect();
    assert_eq!(three.len(), 3, "a line per series");
    assert!(!strokes(&items).iter().any(|s| s.0 == TEXT && s.1.len() == 2 && near(s.1[0].0, s.1[1].0)), "no hi-low lines");
}

#[test]
fn a_combo_skips_an_unsupported_group_and_caps_series_over_all_groups() {
    let plot = format!(
        "{}<c:bubbleChart>{}</c:bubbleChart>{}{}{}",
        bar_group(&ser(0, "Sales", &CATS, &[1.0, 2.0, 3.0], &solid(BAR)), (1, 2)),
        ser(1, "Bubbles", &CATS, &[1.0, 2.0, 3.0], ""),
        line_group(&ser(2, "Trend", &CATS, &[2.0, 2.0, 2.0], &format!("<a:ln>{}</a:ln>", solid(LINE))), (1, 2)),
        cat_ax(1, 2),
        val_ax(2, 1, false, None)
    );
    let items = draw(&plot, true);
    let t: Vec<String> = texts(&items).into_iter().map(|t| t.0).collect();
    assert!(!t.iter().any(|s| s.contains("not supported") || s == "Bubbles"), "{t:?}");
    assert!(t.iter().any(|s| s == "Sales") && t.iter().any(|s| s == "Trend"), "{t:?}");

    // MAX_SERIES counts the series of every group together.
    let many = |k: usize| (0..k).map(|i| ser(i, "S", &CATS[..1], &[1.0], "")).collect::<String>();
    let x = format!(
        r#"<c:chartSpace {NS}><c:chart><c:plotArea>{}{}{}{}</c:plotArea></c:chart></c:chartSpace>"#,
        bar_group(&many(MAX_SERIES - 1), (1, 2)),
        line_group(&many(10), (1, 2)),
        cat_ax(1, 2),
        val_ax(2, 1, false, None)
    );
    let chart = Chart::parse(&crate::xml::parse(x.as_bytes()).unwrap(), &THEME_COLORS).expect("chart");
    assert_eq!(chart.groups.iter().map(|g| g.series.len()).sum::<usize>(), MAX_SERIES);
    assert_eq!(chart.groups.len(), 2);
}

// W5972-STOCK-SERIES-ORDER: c:order is collection order, independent of XML order and c:idx.
fn order_ser(idx: usize, order: Option<&str>, vals: &[f64]) -> String {
    let xml = ser(idx, "Price", &CATS[..vals.len()], vals, no_line());
    let order = order.map_or_else(String::new, |v| format!(r#"<c:order val="{v}"/>"#));
    xml.replace(&format!(r#"<c:order val="{idx}"/>"#), &order)
}

fn order_bars() -> String {
    format!(
        r#"<c:hiLowLines/><c:upDownBars><c:upBars><c:spPr>{}</c:spPr></c:upBars><c:downBars><c:spPr>{}</c:spPr></c:downBars></c:upDownBars>"#,
        solid("00FF00"),
        solid("FF0000")
    )
}

fn order_fills(series: &str) -> Vec<(Rgb, f32, f32, f32, f32)> {
    fills(&draw(&stock(series, &order_bars()), false))
        .into_iter()
        .filter(|f| f.0 == Rgb(0, 255, 0) || f.0 == Rgb(255, 0, 0))
        .map(|f| (f.0, f.1, f.2, f.3, f.4))
        .collect()
}

#[test]
fn stock_order_ohlc_all_xml_permutations_keep_up_and_down_endpoints() {
    // Deliberately unrelated c:idx values must not control endpoint selection.
    let s = [
        order_ser(9, Some("0"), &[10.0, 30.0]),
        order_ser(7, Some("1"), &[25.0, 35.0]),
        order_ser(5, Some("2"), &[5.0, 12.0]),
        order_ser(3, Some("3"), &[20.0, 15.0]),
    ];
    let reference = order_fills(&s.concat());
    assert_eq!(reference.len(), 2);
    assert_eq!((reference[0].0, reference[1].0), (Rgb(0, 255, 0), Rgb(255, 0, 0)));
    let mut permutations = 0;
    for a in 0..4 {
        for b in 0..4 {
            for c in 0..4 {
                for d in 0..4 {
                    if a == b || a == c || a == d || b == c || b == d || c == d {
                        continue;
                    }
                    assert_eq!(order_fills(&format!("{}{}{}{}", s[a], s[b], s[c], s[d])), reference, "XML order {a}{b}{c}{d}");
                    permutations += 1;
                }
            }
        }
    }
    assert_eq!(permutations, 24);
}

#[test]
fn stock_order_hlc_permutations_keep_high_low_geometry() {
    let s = [order_ser(8, Some("0"), &[30.0]), order_ser(6, Some("1"), &[5.0]), order_ser(4, Some("2"), &[20.0])];
    let lines =
        |xml: &str| strokes(&draw(&stock(xml, "<c:hiLowLines/>"), false)).into_iter().filter(|s| s.0 == TEXT).map(|s| s.1).collect::<Vec<_>>();
    let reference = lines(&s.concat());
    assert_eq!(reference.len(), 1);
    assert_eq!(reference[0].len(), 2);
    assert!(near(reference[0][0].0, reference[0][1].0));
    assert!(reference[0][0].1 < reference[0][1].1);
    for p in [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
        assert_eq!(lines(&format!("{}{}{}", s[p[0]], s[p[1]], s[p[2]])), reference);
    }
}

#[test]
fn stock_order_missing_invalid_duplicate_and_sparse_orders_have_stable_fallbacks() {
    // Local recovery uses retained compatible-series position, with XML position breaking ties.
    for invalid in [None, Some(""), Some("-1"), Some("1.5"), Some("4294967296"), Some("NaN")] {
        let s =
            [order_ser(90, invalid, &[10.0]), order_ser(70, Some("1"), &[25.0]), order_ser(50, Some("2"), &[5.0]), order_ser(30, Some("3"), &[20.0])];
        let bars = order_fills(&s.concat());
        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].0, Rgb(0, 255, 0), "fallback {invalid:?}");
    }
    for orders in [[None, None, None, None], [Some("7"), Some("7"), Some("7"), Some("7")]] {
        let s =
            [order_ser(9, orders[0], &[10.0]), order_ser(7, orders[1], &[25.0]), order_ser(5, orders[2], &[5.0]), order_ser(3, orders[3], &[20.0])];
        assert_eq!(order_fills(&s.concat())[0].0, Rgb(0, 255, 0));
    }
    let s = [
        order_ser(9, Some("4294967295"), &[20.0]),
        order_ser(7, Some("30"), &[25.0]),
        order_ser(5, Some("20"), &[5.0]),
        order_ser(3, Some("10"), &[10.0]),
    ];
    assert_eq!(order_fills(&s.concat())[0].0, Rgb(0, 255, 0));
}

#[test]
fn stock_order_missing_endpoint_value_does_not_substitute_another_series() {
    let s = [order_ser(3, Some("3"), &[20.0]), order_ser(1, Some("1"), &[25.0]), order_ser(2, Some("2"), &[5.0]), order_ser(0, Some("0"), &[10.0])];
    let high_low = |xml: &str| {
        strokes(&draw(&stock(xml, "<c:hiLowLines/>"), false))
            .into_iter()
            .filter(|s| s.0 == TEXT && s.1.len() == 2 && near(s.1[0].0, s.1[1].0))
            .map(|s| s.1)
            .collect::<Vec<_>>()
    };
    // TEXT is the default hi-low colour; axes and gridlines have distinct colours.
    let reference = high_low(&s.concat());
    assert_eq!(reference.len(), 1, "the actual high-low path");
    assert!(reference[0][0].1 < reference[0][1].1);
    for (endpoint, value) in [(3, 10), (0, 20)] {
        let point = format!(r#"<c:pt idx="0"><c:v>{value}</c:v></c:pt>"#);
        let mut missing = s.clone();
        missing[endpoint] = missing[endpoint].replace(&point, "");
        assert_ne!(missing[endpoint], s[endpoint], "fixture removes the chosen endpoint");
        let series = missing.concat();
        assert!(order_fills(&series).is_empty(), "no substitute endpoint for {endpoint}");
        assert_eq!(high_low(&series), reference, "high/low remains 25/5 with endpoint {endpoint} missing");
    }
}

#[test]
fn stock_order_fallback_counts_retained_compatible_series_before_the_group() {
    let prefix = format!(
        "{}<c:bubbleChart>{}</c:bubbleChart>",
        bar_group(&format!("{}{}", ser(0, "A", &CATS[..1], &[1.0], &solid(BAR)), ser(1, "B", &CATS[..1], &[2.0], &solid(BAR))), (1, 2)),
        ser(2, "Ignored", &CATS[..1], &[99.0], "")
    );
    let series = |fallback| {
        format!(
            "{}{}{}{}",
            order_ser(90, fallback, &[10.0]),
            order_ser(70, Some("1"), &[25.0]),
            order_ser(50, Some("3"), &[5.0]),
            order_ser(30, Some("4"), &[20.0])
        )
    };
    let plot = |fallback| format!("{prefix}{}", stock(&series(fallback), &order_bars()));
    let price_bars = |plot: &str| {
        fills(&draw(plot, false))
            .into_iter()
            .filter(|f| f.0 == Rgb(0, 255, 0) || f.0 == Rgb(255, 0, 0))
            .map(|f| (f.0, f.1, f.2, f.3, f.4))
            .collect::<Vec<_>>()
    };
    let reference = price_bars(&plot(Some("2")));
    assert_eq!(reference.len(), 1);
    assert_eq!(reference[0].0, Rgb(255, 0, 0), "high 25 to close 20 is down");
    assert_eq!(price_bars(&plot(None)), reference, "fallback starts at retained position 2, excluding bubbles");
    let xml = format!(r#"<c:chartSpace {NS}><c:chart><c:plotArea>{}</c:plotArea></c:chart></c:chartSpace>"#, plot(None));
    let chart = Chart::parse(&crate::xml::parse(xml.as_bytes()).unwrap(), &THEME_COLORS).unwrap();
    assert_eq!(chart.groups.len(), 2);
    assert_eq!(chart.groups[1].first, 2);
}

#[test]
fn combo_point_and_padded_cell_limits_are_shared_across_groups() {
    let many = |start: usize| {
        (start..start + 6)
            .map(|i| format!(
                r#"<c:ser><c:idx val="{i}"/><c:val><c:numRef><c:numCache><c:ptCount val="10001"/><c:pt idx="0"><c:v>1</c:v></c:pt><c:pt idx="10000"><c:v>99</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser>"#
            ))
            .collect::<String>()
    };
    let plot = format!("{}{}{}{}", bar_group(&many(0), (1, 2)), line_group(&many(6), (1, 2)), cat_ax(1, 2), val_ax(2, 1, false, None));
    let xml = format!(r#"<c:chartSpace {NS}><c:chart><c:plotArea>{plot}</c:plotArea></c:chart></c:chartSpace>"#);
    let chart = Chart::parse(&crate::xml::parse(xml.as_bytes()).unwrap(), &THEME_COLORS).unwrap();
    assert_eq!(chart.n, MAX_POINTS);
    assert_eq!(chart.groups.len(), 2);
    assert_eq!(chart.groups[0].series.len(), 6);
    assert_eq!(chart.groups[1].series.len(), 4);
    assert_eq!(chart.groups[1].first, 6);
    assert_eq!(chart.groups.iter().flat_map(|g| &g.series).map(|s| s.values.len()).sum::<usize>(), MAX_CELLS);
    assert!(chart.groups.iter().flat_map(|g| &g.series).all(|s| s.values.len() == MAX_POINTS && !s.values.contains(&Some(99.0))));
}

fn repair_parse(plot: &str) -> Chart {
    let xml = format!(r#"<c:chartSpace {NS}><c:chart><c:plotArea>{plot}</c:plotArea></c:chart></c:chartSpace>"#);
    Chart::parse(&crate::xml::parse(xml.as_bytes()).unwrap(), &THEME_COLORS).expect("admitted chart")
}

#[test]
fn chart_repair_axis_work_is_bounded_before_resolution() {
    let axes: String = (1000..2000).map(|i| val_ax(i, 1, false, Some(40.0))).collect();
    let empty = r#"<c:lineChart><c:axId val="0"/></c:lineChart>"#.repeat(1000);
    let xml = format!(r#"<c:chartSpace {NS}><c:chart><c:plotArea>{empty}{axes}</c:plotArea></c:chart></c:chartSpace>"#);
    parse::reset_axis_work();
    assert!(Chart::parse(&crate::xml::parse(xml.as_bytes()).unwrap(), &THEME_COLORS).is_none());
    let empty_work = parse::axis_work();
    assert_eq!(empty_work.groups, parse::MAX_CHART_GROUPS);
    assert_eq!((empty_work.axes, empty_work.lookups), (0, 0), "empty groups resolve no axes");

    let refs = r#"<c:axId val="0"/>"#.repeat(5);
    let group = format!("<c:lineChart>{}{refs}</c:lineChart>", ser(0, "S", &CATS[..1], &[1.0], no_line()));
    parse::reset_axis_work();
    let chart = repair_parse(&format!("{}{axes}", group.repeat(1000)));
    let work = parse::axis_work();
    assert_eq!(chart.groups.len(), MAX_SERIES);
    assert_eq!(work.groups, parse::MAX_CHART_GROUPS);
    assert_eq!(work.axes, parse::MAX_CHART_AXES);
    assert_eq!(work.lookups, MAX_SERIES * parse::MAX_AXIS_REFS);
    eprintln!(
        "STRUCTURAL_AXIS_WORK empty_groups={} empty_axes={} empty_lookups={} retained_groups={} indexed_axes={} integer_lookups={}",
        empty_work.groups, empty_work.axes, empty_work.lookups, work.groups, work.axes, work.lookups
    );
}

#[test]
fn chart_repair_pruned_groups_do_not_choose_axes() {
    let many: String = (0..10).map(|i| format!(
        r#"<c:ser><c:idx val="{i}"/><c:val><c:numRef><c:numCache><c:ptCount val="10000"/><c:pt idx="0"><c:v>1</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser>"#
    )).collect();
    let plot = format!(
        "{}{}{}{}{}",
        line_group(&many, (1, 2)),
        bar_group(&ser(10, "Dropped", &CATS[..1], &[2.0], ""), (3, 4)),
        val_ax(4, 3, true, Some(1.0)),
        cat_ax(1, 2),
        val_ax(2, 1, false, Some(40.0))
    );
    parse::reset_axis_work();
    let chart = repair_parse(&plot);
    assert_eq!(chart.groups.len(), 1, "cell padding removes the second group");
    assert!(chart.sec.is_none());
    assert_eq!(chart.val.max, Some(40.0), "retained group selects referenced axis 2, not first axis 4");
    assert_eq!(parse::axis_work().lookups, 2, "only the retained group resolves references");
}

fn repair_ordinary() -> String {
    line_group(&ser(0, "Ordinary", &CATS[..2], &[10.0, 20.0], &format!("<a:ln>{}</a:ln>", solid(LINE))), (1, 2))
}

fn repair_percent(ax: (u32, u32)) -> String {
    bar_group(
        &format!("{}{}", ser(1, "Share A", &CATS[..2], &[50.0, 50.0], &solid(BAR)), ser(2, "Share B", &CATS[..2], &[50.0, 50.0], &solid("A5A5A5"))),
        ax,
    )
    .replace("clustered", "percentStacked")
}

fn repair_primary(max: Option<f64>) -> String {
    val_ax(2, 1, false, max).replace(r#"sourceLinked="1""#, r#"sourceLinked="0""#)
}

fn repair_line(plot: &str) -> Vec<(f32, f32)> {
    let lines: Vec<_> = strokes(&draw(plot, false)).into_iter().filter(|s| s.0 == Rgb(0xED, 0x7D, 0x31) && s.1.len() == 2).collect();
    assert_eq!(lines.len(), 1, "actual ordinary-series path");
    lines[0].1.clone()
}

#[test]
fn chart_repair_mixed_axis_keeps_explicit_scale_format_and_geometry() {
    let axes = format!("{}{}", cat_ax(1, 2), repair_primary(Some(40.0)));
    let reference = repair_line(&format!("{}{axes}", repair_ordinary()));
    let plot = format!("{}{}{axes}", repair_ordinary(), repair_percent((1, 2)));
    let line = repair_line(&plot);
    assert_eq!(line, reference, "10 and 20 retain their 1/4 and 1/2 positions on 0..40");
    assert!(line[0].1 > line[1].1, "distinct values must not flatten against the top");
    let chart = repair_parse(&plot);
    let scale = plot::Axes::of(&chart).vs;
    assert_eq!((scale.lo, scale.hi), (0.0, 40.0));
    assert_eq!(chart.val_fmt, "General");
    assert!(!texts(&draw(&plot, false)).iter().any(|(s, _)| s.contains('%')));
}

#[test]
fn chart_repair_mixed_automatic_axis_includes_ordinary_values() {
    let plot = format!("{}{}{}{}", repair_ordinary(), repair_percent((1, 2)), cat_ax(1, 2), val_ax(2, 1, false, None));
    let chart = repair_parse(&plot);
    let scale = plot::Axes::of(&chart).vs;
    assert!(scale.lo <= 0.0 && scale.hi >= 20.0);
    assert_eq!(chart.val_fmt, "General");
    let line = repair_line(&plot);
    assert!(line[0].1 > line[1].1, "the actual path retains ordinary data separation");
}

#[test]
fn chart_repair_separate_percent_axis_keeps_independent_policy() {
    let primary = format!("{}{}", cat_ax(1, 2), repair_primary(Some(40.0)));
    let reference = repair_line(&format!("{}{primary}", repair_ordinary()));
    let plot = format!("{}{}{primary}{}", repair_ordinary(), repair_percent((3, 4)), val_ax(4, 3, true, None));
    let chart = repair_parse(&plot);
    assert_eq!(chart.val_fmt, "General");
    assert_eq!(chart.sec_fmt, "0%");
    assert!(chart.sec.is_some());
    let line = repair_line(&plot);
    assert!(near(line[0].1, reference[0].1) && near(line[1].1, reference[1].1));
    assert!(texts(&draw(&plot, false)).iter().any(|(s, x)| s == "100%" && *x > line[1].0));
}

#[test]
fn chart_repair_pure_percent_respects_explicit_axis_authority() {
    let axis = repair_primary(Some(2.0)).replace(r#"formatCode="General""#, r#"formatCode="0.00""#);
    let chart = repair_parse(&format!("{}{}{axis}", repair_percent((1, 2)), cat_ax(1, 2)));
    assert_eq!(chart.val_fmt, "0.00");
    let scale = plot::Axes::of(&chart).vs;
    assert_eq!((scale.lo, scale.hi), (0.0, 2.0));
}
