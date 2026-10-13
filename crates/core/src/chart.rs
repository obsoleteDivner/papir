//! Charts (DrawingML `c:chartSpace`): drawn from the cached values Word
//! stores with the chart. Bar/column (clustered, stacked), line, area, pie.

use crate::fonts::Fonts;
use crate::layout::Placed;
use crate::xml::Element;

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Column { stacked: bool },
    Bar { stacked: bool },
    Line,
    Area,
    Pie,
}

#[derive(Clone, Debug)]
pub struct Series {
    pub name: String,
    pub vals: Vec<f64>,
    pub color: Option<u32>,
    /// per-point colours (pie slices)
    pub points: Vec<(usize, u32)>,
}

#[derive(Clone, Debug)]
pub struct Chart {
    pub kind: Kind,
    pub title: Option<String>,
    pub cats: Vec<String>,
    pub series: Vec<Series>,
    /// legend position: r, l, t, b; None without a legend
    pub legend: Option<String>,
}

const PALETTE: [u32; 6] = [0x4472C4, 0xED7D31, 0xA5A5A5, 0xFFC000, 0x5B9BD5, 0x70AD47];

fn local<'a>(e: &'a Element, name: &str) -> Option<&'a Element> {
    e.elems().find(|c| c.local == name)
}

fn find<'a>(e: &'a Element, name: &str) -> Option<&'a Element> {
    for c in e.elems() {
        if c.local == name {
            return Some(c);
        }
        if let Some(f) = find(c, name) {
            return Some(f);
        }
    }
    None
}

fn val(e: &Element) -> Option<String> {
    e.attrs.iter().find(|a| a.local == "val").map(|a| a.value.clone())
}

/// Points of a string or number cache, by index.
fn cache(e: &Element) -> Vec<String> {
    let Some(c) = find(e, "strCache").or_else(|| find(e, "numCache")) else {
        // literal values
        return find(e, "v").map(|v| vec![v.text()]).unwrap_or_default();
    };
    let n = local(c, "ptCount").and_then(val).and_then(|v| v.parse().ok()).unwrap_or(0usize);
    let mut out = vec![String::new(); n];
    for pt in c.elems().filter(|x| x.local == "pt") {
        let i: usize = pt.attrs.iter().find(|a| a.local == "idx").and_then(|a| a.value.parse().ok()).unwrap_or(0);
        if i >= out.len() {
            out.resize(i + 1, String::new());
        }
        out[i] = local(pt, "v").map(|v| v.text()).unwrap_or_default();
    }
    out
}

fn fill_of(e: &Element) -> Option<u32> {
    let sp = local(e, "spPr")?;
    let f = local(sp, "solidFill")?;
    let c = local(f, "srgbClr")?;
    val(c).and_then(|v| crate::layout::color(&v.to_uppercase()))
}

pub fn parse(root: &Element) -> Option<Chart> {
    let chart = local(root, "chart")?;
    let plot = local(chart, "plotArea")?;
    let (node, kind) = plot.elems().find_map(|c| {
        let stacked = local(c, "grouping").and_then(val).is_some_and(|g| g.contains("tacked"));
        let k = match c.local.as_str() {
            "barChart" | "bar3DChart" => {
                if local(c, "barDir").and_then(val).as_deref() == Some("bar") {
                    Kind::Bar { stacked }
                } else {
                    Kind::Column { stacked }
                }
            }
            "lineChart" | "line3DChart" | "scatterChart" | "radarChart" => Kind::Line,
            "areaChart" | "area3DChart" => Kind::Area,
            "pieChart" | "pie3DChart" | "doughnutChart" => Kind::Pie,
            _ => return None,
        };
        Some((c, k))
    })?;
    let mut cats: Vec<String> = Vec::new();
    let mut series = Vec::new();
    for s in node.elems().filter(|x| x.local == "ser") {
        let name = local(s, "tx").map(cache).and_then(|v| v.into_iter().next()).unwrap_or_default();
        if cats.is_empty() {
            if let Some(c) = local(s, "cat").or_else(|| local(s, "xVal")) {
                cats = cache(c);
            }
        }
        let vals = local(s, "val")
            .or_else(|| local(s, "yVal"))
            .map(cache)
            .unwrap_or_default()
            .into_iter()
            .map(|v| v.parse::<f64>().unwrap_or(0.0))
            .collect();
        let points = s
            .elems()
            .filter(|x| x.local == "dPt")
            .filter_map(|p| Some((local(p, "idx").and_then(val)?.parse().ok()?, fill_of(p)?)))
            .collect();
        series.push(Series { name, vals, color: fill_of(s), points });
    }
    let title = local(chart, "title").map(|t| {
        let mut s = String::new();
        fn texts(e: &Element, s: &mut String) {
            for c in e.elems() {
                if c.local == "t" {
                    s.push_str(&c.text());
                } else {
                    texts(c, s);
                }
            }
        }
        texts(t, &mut s);
        s
    });
    // single-series charts without a title show the series name, like Word
    let title = match title {
        Some(t) if t.is_empty() && series.len() == 1 => Some(series[0].name.clone()),
        t => t,
    };
    let legend = local(chart, "legend").map(|l| local(l, "legendPos").and_then(val).unwrap_or_else(|| "r".into()));
    if cats.is_empty() {
        let n = series.iter().map(|s| s.vals.len()).max().unwrap_or(0);
        cats = (1..=n).map(|i| i.to_string()).collect();
    }
    Some(Chart { kind, title, cats, series, legend })
}

fn fmt(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        format!("{:.1}", v).replace('.', ",")
    }
}

/// A round step for about `n` ticks up to `max`.
fn nice_step(range: f64, n: f64) -> f64 {
    if range <= 0.0 {
        return 1.0;
    }
    let raw = range / n;
    let p = 10f64.powf(raw.log10().floor());
    let m = raw / p;
    let k = if m <= 1.0 { 1.0 } else if m <= 2.0 { 2.0 } else if m <= 2.5 { 2.5 } else if m <= 5.0 { 5.0 } else { 10.0 };
    k * p
}

impl Chart {
    fn color(&self, i: usize) -> u32 {
        self.series.get(i).and_then(|s| s.color).unwrap_or(PALETTE[i % PALETTE.len()])
    }

    /// Drawing items for the chart in the box (x, y, w, h), points.
    pub fn render(&self, fonts: &Fonts, x: f32, y: f32, w: f32, h: f32) -> Vec<Placed> {
        let mut out = Vec::new();
        let face = fonts.select("Calibri", false, false);
        let bold = fonts.select("Calibri", true, false);
        let tw = |s: &str, size: f32| s.chars().map(|c| fonts.width(face, c, size)).sum::<f32>();
        let text = |out: &mut Vec<Placed>, tx: f32, ty: f32, s: &str, size: f32, f: u16, color: u32| {
            out.push(Placed::Text { x: tx, y: ty, text: s.to_string(), face: f, size, color });
        };
        out.push(Placed::Rect { x, y, w, h, color: 0xFFFFFF });
        let mut top = y + 6.0;
        if let Some(t) = self.title.as_ref().filter(|t| !t.is_empty()) {
            let size = 13.0;
            text(&mut out, x + (w - tw(t, size)) / 2.0, top + size, t, size, bold, 0x404040);
            top += size + 8.0;
        }
        // legend entries: series names, or categories for a pie
        let names: Vec<(String, u32)> = if self.kind == Kind::Pie {
            let s = self.series.first();
            self.cats.iter().enumerate().map(|(i, c)| (c.clone(), s.and_then(|s| s.points.iter().find(|p| p.0 == i).map(|p| p.1)).unwrap_or(PALETTE[i % PALETTE.len()]))).collect()
        } else {
            self.series.iter().enumerate().map(|(i, s)| (s.name.clone(), self.color(i))).collect()
        };
        let lsize = 8.0;
        let (mut right, mut bottom) = (x + w - 6.0, y + h - 6.0);
        match self.legend.as_deref() {
            Some("b") | Some("t") => {
                let total: f32 = names.iter().map(|(n, _)| tw(n, lsize) + 18.0).sum();
                let ly = if self.legend.as_deref() == Some("b") { y + h - 8.0 } else { top + lsize };
                let mut lx = x + (w - total).max(0.0) / 2.0;
                for (n, c) in &names {
                    out.push(Placed::Rect { x: lx, y: ly - 6.0, w: 6.0, h: 6.0, color: *c });
                    text(&mut out, lx + 9.0, ly, n, lsize, face, 0x595959);
                    lx += tw(n, lsize) + 18.0;
                }
                if self.legend.as_deref() == Some("b") {
                    bottom -= 16.0;
                } else {
                    top += 16.0;
                }
            }
            Some(_) => {
                let lw = names.iter().map(|(n, _)| tw(n, lsize)).fold(0.0f32, f32::max).min(w * 0.35) + 16.0;
                right -= lw;
                let mut ly = top + (bottom - top - names.len() as f32 * 13.0) / 2.0 + 9.0;
                for (n, c) in &names {
                    out.push(Placed::Rect { x: right + 8.0, y: ly - 6.0, w: 6.0, h: 6.0, color: *c });
                    text(&mut out, right + 17.0, ly, n, lsize, face, 0x595959);
                    ly += 13.0;
                }
            }
            None => {}
        }
        if self.kind == Kind::Pie {
            self.pie(&mut out, x + 6.0, top, right, bottom, &names);
            return out;
        }

        // value range
        let n = self.cats.len().max(1);
        let stacked = matches!(self.kind, Kind::Column { stacked: true } | Kind::Bar { stacked: true });
        let (mut lo, mut hi) = (0f64, 0f64);
        for i in 0..n {
            if stacked {
                let (p, m): (f64, f64) = self.series.iter().map(|s| s.vals.get(i).copied().unwrap_or(0.0)).fold((0.0, 0.0), |(p, m), v| if v >= 0.0 { (p + v, m) } else { (p, m + v) });
                hi = hi.max(p);
                lo = lo.min(m);
            } else {
                for s in &self.series {
                    let v = s.vals.get(i).copied().unwrap_or(0.0);
                    hi = hi.max(v);
                    lo = lo.min(v);
                }
            }
        }
        let step = nice_step(hi - lo, 5.0);
        let hi = (hi / step).ceil() * step;
        let lo = (lo / step).floor() * step;
        let span = (hi - lo).max(step);
        let vsize = 8.0;
        let ticks: Vec<f64> = (0..=((span / step).round() as i64)).map(|k| lo + k as f64 * step).collect();
        let horizontal = matches!(self.kind, Kind::Bar { .. });

        let label_w = if horizontal {
            self.cats.iter().map(|c| tw(c, vsize)).fold(0.0f32, f32::max).min(w * 0.3)
        } else {
            ticks.iter().map(|t| tw(&fmt(*t), vsize)).fold(0.0f32, f32::max)
        };
        let (pl, pt, pr, pb) = (x + 6.0 + label_w + 4.0, top + 4.0, right - 4.0, bottom - vsize - 6.0);
        let (pw, ph) = ((pr - pl).max(10.0), (pb - pt).max(10.0));
        let grid = 0xD9D9D9;
        // gridlines and value labels
        for t in &ticks {
            let f = ((*t - lo) / span) as f32;
            if horizontal {
                let gx = pl + f * pw;
                out.push(Placed::Stroke { x1: gx, y1: pt, x2: gx, y2: pb, w: 0.5, color: grid, style: 0 });
                let s = fmt(*t);
                text(&mut out, gx - tw(&s, vsize) / 2.0, pb + vsize + 3.0, &s, vsize, face, 0x595959);
            } else {
                let gy = pb - f * ph;
                out.push(Placed::Stroke { x1: pl, y1: gy, x2: pr, y2: gy, w: 0.5, color: grid, style: 0 });
                let s = fmt(*t);
                text(&mut out, pl - 4.0 - tw(&s, vsize), gy + vsize * 0.35, &s, vsize, face, 0x595959);
            }
        }
        let zero = ((0.0 - lo) / span) as f32;
        let group = if horizontal { ph / n as f32 } else { pw / n as f32 };
        // category labels
        for (i, c) in self.cats.iter().enumerate() {
            if horizontal {
                let cy = pt + group * (i as f32 + 0.5);
                text(&mut out, pl - 4.0 - tw(c, vsize), cy + vsize * 0.35, c, vsize, face, 0x595959);
            } else {
                let cx = pl + group * (i as f32 + 0.5);
                text(&mut out, cx - tw(c, vsize) / 2.0, pb + vsize + 3.0, c, vsize, face, 0x595959);
            }
        }
        match self.kind {
            Kind::Column { .. } | Kind::Bar { .. } => {
                let ns = self.series.len().max(1) as f32;
                let bw = if stacked { group * 0.6 } else { group * 0.62 / ns };
                for i in 0..n {
                    let (mut up, mut down) = (0f64, 0f64);
                    for (si, s) in self.series.iter().enumerate() {
                        let v = s.vals.get(i).copied().unwrap_or(0.0);
                        let (a, b) = if stacked {
                            if v >= 0.0 {
                                up += v;
                                (up - v, up)
                            } else {
                                down += v;
                                (down, down - v)
                            }
                        } else if v >= 0.0 {
                            (0.0, v)
                        } else {
                            (v, 0.0)
                        };
                        let fa = ((a - lo) / span) as f32;
                        let fb = ((b - lo) / span) as f32;
                        let off = if stacked { (group - bw) / 2.0 } else { group * 0.19 + bw * si as f32 };
                        let color = self.color(si);
                        if horizontal {
                            let by = pt + group * i as f32 + off;
                            out.push(Placed::Rect { x: pl + fa * pw, y: by, w: (fb - fa) * pw, h: bw, color });
                        } else {
                            let bx = pl + group * i as f32 + off;
                            out.push(Placed::Rect { x: bx, y: pb - fb * ph, w: bw, h: (fb - fa) * ph, color });
                        }
                    }
                }
            }
            Kind::Line | Kind::Area => {
                for (si, s) in self.series.iter().enumerate().rev() {
                    let color = self.color(si);
                    let pts: Vec<(f32, f32)> = (0..n)
                        .map(|i| {
                            let v = s.vals.get(i).copied().unwrap_or(0.0);
                            (pl + group * (i as f32 + 0.5), pb - ((v - lo) / span) as f32 * ph)
                        })
                        .collect();
                    if self.kind == Kind::Area {
                        let mut poly = pts.clone();
                        poly.push((pts.last().unwrap().0, pb - zero * ph));
                        poly.push((pts[0].0, pb - zero * ph));
                        out.push(Placed::Poly { pts: poly, color });
                    } else {
                        for wpt in pts.windows(2) {
                            out.push(Placed::Stroke { x1: wpt[0].0, y1: wpt[0].1, x2: wpt[1].0, y2: wpt[1].1, w: 2.0, color, style: 0 });
                        }
                        for (px, py) in &pts {
                            out.push(Placed::Rect { x: px - 2.5, y: py - 2.5, w: 5.0, h: 5.0, color });
                        }
                    }
                }
            }
            Kind::Pie => {}
        }
        // the axis line
        if horizontal {
            let ax = pl + zero * pw;
            out.push(Placed::Stroke { x1: ax, y1: pt, x2: ax, y2: pb, w: 0.75, color: 0xBFBFBF, style: 0 });
        } else {
            let ay = pb - zero * ph;
            out.push(Placed::Stroke { x1: pl, y1: ay, x2: pr, y2: ay, w: 0.75, color: 0xBFBFBF, style: 0 });
        }
        out
    }

    fn pie(&self, out: &mut Vec<Placed>, x1: f32, y1: f32, x2: f32, y2: f32, colors: &[(String, u32)]) {
        let Some(s) = self.series.first() else { return };
        let total: f64 = s.vals.iter().filter(|v| **v > 0.0).sum();
        if total <= 0.0 {
            return;
        }
        let r = ((x2 - x1).min(y2 - y1) / 2.0 - 4.0).max(4.0);
        let (cx, cy) = ((x1 + x2) / 2.0, (y1 + y2) / 2.0);
        // clockwise from twelve o'clock, like Word
        let mut a = -std::f32::consts::FRAC_PI_2;
        for (i, v) in s.vals.iter().enumerate() {
            if *v <= 0.0 {
                continue;
            }
            let sweep = (*v / total) as f32 * std::f32::consts::TAU;
            let steps = ((sweep / 0.08).ceil() as usize).max(2);
            let mut pts = vec![(cx, cy)];
            for k in 0..=steps {
                let t = a + sweep * k as f32 / steps as f32;
                pts.push((cx + r * t.cos(), cy + r * t.sin()));
            }
            out.push(Placed::Poly { pts, color: colors.get(i).map(|c| c.1).unwrap_or(PALETTE[i % PALETTE.len()]) });
            a += sweep;
        }
    }
}
