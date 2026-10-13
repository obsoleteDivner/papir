//! Excel number formats, shown the way Ukrainian Excel shows them: decimal
//! comma, a non-breaking space between thousands, Ukrainian month names.

const MONTHS: [&str; 12] = ["січень", "лютий", "березень", "квітень", "травень", "червень", "липень", "серпень", "вересень", "жовтень", "листопад", "грудень"];
const MON: [&str; 12] = ["січ", "лют", "бер", "кві", "тра", "чер", "лип", "сер", "вер", "жов", "лис", "гру"];
const DAYS: [&str; 7] = ["понеділок", "вівторок", "середа", "четвер", "пʼятниця", "субота", "неділя"];
const DAY: [&str; 7] = ["Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Нд"];
const THOUSANDS: char = '\u{a0}';
const DECIMAL: char = ',';

/// Code of a built-in format.
pub fn builtin(id: u32) -> Option<&'static str> {
    Some(match id {
        0 => "General",
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        5 => "#,##0 \"₴\";-#,##0 \"₴\"",
        6 => "#,##0 \"₴\";[Red]-#,##0 \"₴\"",
        7 => "#,##0.00 \"₴\";-#,##0.00 \"₴\"",
        8 => "#,##0.00 \"₴\";[Red]-#,##0.00 \"₴\"",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        12 => "# ?/?",
        13 => "# ??/??",
        14 => "dd.mm.yyyy",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        18 => "h:mm AM/PM",
        19 => "h:mm:ss AM/PM",
        20 => "h:mm",
        21 => "h:mm:ss",
        22 => "dd.mm.yyyy h:mm",
        37 => "#,##0 ;(#,##0)",
        38 => "#,##0 ;[Red](#,##0)",
        39 => "#,##0.00;(#,##0.00)",
        40 => "#,##0.00;[Red](#,##0.00)",
        45 => "mm:ss",
        46 => "[h]:mm:ss",
        47 => "mm:ss.0",
        48 => "##0.0E+0",
        49 => "@",
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Lit(String),
    /// digit placeholders and separators of the number part
    Num(String),
    Date(String),
    AmPm,
    Text,
    Percent,
    Exp(String),
}

struct Section {
    toks: Vec<Tok>,
    color: Option<u32>,
    cond: Option<(String, f64)>,
    date: bool,
}

fn color_name(n: &str) -> Option<u32> {
    Some(match n.to_ascii_lowercase().as_str() {
        "black" => 0x000000,
        "red" => 0xFF0000,
        "green" => 0x00FF00,
        "blue" => 0x0000FF,
        "yellow" => 0xFFFF00,
        "magenta" => 0xFF00FF,
        "cyan" => 0x00FFFF,
        "white" => 0xFFFFFF,
        _ => return None,
    })
}

fn parse_section(src: &str) -> Section {
    let mut toks = Vec::new();
    let mut color = None;
    let mut cond = None;
    let ch: Vec<char> = src.chars().collect();
    let mut i = 0;
    let lit = |toks: &mut Vec<Tok>, s: &str| match toks.last_mut() {
        Some(Tok::Lit(l)) => l.push_str(s),
        _ => toks.push(Tok::Lit(s.to_string())),
    };
    while i < ch.len() {
        let c = ch[i];
        match c {
            '"' => {
                let end = ch[i + 1..].iter().position(|&x| x == '"').map(|p| i + 1 + p).unwrap_or(ch.len());
                lit(&mut toks, &ch[i + 1..end].iter().collect::<String>());
                i = end + 1;
            }
            '\\' => {
                if let Some(n) = ch.get(i + 1) {
                    lit(&mut toks, &n.to_string());
                }
                i += 2;
            }
            '_' => {
                lit(&mut toks, " ");
                i += 2;
            }
            '*' => i += 2,
            '[' => {
                let end = ch[i..].iter().position(|&x| x == ']').map(|p| i + p).unwrap_or(ch.len());
                let inner: String = ch[i + 1..end.min(ch.len())].iter().collect();
                if let Some(cur) = inner.strip_prefix('$') {
                    // [$₴-422]: the symbol before the locale
                    lit(&mut toks, cur.split('-').next().unwrap_or(""));
                } else if let Some(c) = color_name(&inner) {
                    color = Some(c);
                } else if inner.to_ascii_lowercase().starts_with("color") {
                    color = Some(0x000000);
                } else if inner.starts_with(['<', '>', '=']) {
                    let op: String = inner.chars().take_while(|c| "<>=".contains(*c)).collect();
                    cond = inner[op.len()..].trim().parse().ok().map(|v| (op, v));
                } else if inner.chars().all(|c| "hHmMsS".contains(c)) {
                    toks.push(Tok::Date(format!("[{}]", inner.to_ascii_lowercase())));
                }
                i = end + 1;
            }
            '0' | '#' | '?' => {
                let mut s = String::new();
                while i < ch.len() && "0#?.,".contains(ch[i]) {
                    s.push(ch[i]);
                    i += 1;
                }
                // "0 ?/?" fractions and "# ##0": a space between placeholders
                while i + 1 < ch.len() && ch[i] == ' ' && "0#?".contains(ch[i + 1]) && s.contains(['0', '#']) {
                    s.push(',');
                    i += 1;
                    while i < ch.len() && "0#?.,".contains(ch[i]) {
                        s.push(ch[i]);
                        i += 1;
                    }
                }
                match toks.last_mut() {
                    Some(Tok::Num(n)) => n.push_str(&s),
                    _ => toks.push(Tok::Num(s)),
                }
            }
            'E' | 'e' if matches!(ch.get(i + 1), Some('+') | Some('-')) => {
                let mut s = format!("E{}", ch[i + 1]);
                i += 2;
                while i < ch.len() && "0#".contains(ch[i]) {
                    s.push(ch[i]);
                    i += 1;
                }
                toks.push(Tok::Exp(s));
            }
            // a separator right before digit placeholders: ".00"
            '.' if matches!(ch.get(i + 1), Some('0') | Some('#') | Some('?')) && !matches!(toks.last(), Some(Tok::Date(_))) => {
                let mut s = String::new();
                while i < ch.len() && "0#?.,".contains(ch[i]) {
                    s.push(ch[i]);
                    i += 1;
                }
                toks.push(Tok::Num(s));
            }
            '%' => {
                toks.push(Tok::Percent);
                i += 1;
            }
            '@' => {
                toks.push(Tok::Text);
                i += 1;
            }
            'A' | 'a' if src[src.char_indices().nth(i).map(|x| x.0).unwrap_or(0)..].to_ascii_uppercase().starts_with("AM/PM") => {
                toks.push(Tok::AmPm);
                i += 5;
            }
            'y' | 'Y' | 'm' | 'M' | 'd' | 'D' | 'h' | 'H' | 's' | 'S' => {
                let l = c.to_ascii_lowercase();
                let mut s = String::new();
                while i < ch.len() && ch[i].to_ascii_lowercase() == l {
                    s.push(l);
                    i += 1;
                }
                // fractions of seconds: ss.0
                if l == 's' && i < ch.len() && ch[i] == '.' {
                    s.push('.');
                    i += 1;
                    while i < ch.len() && ch[i] == '0' {
                        s.push('0');
                        i += 1;
                    }
                }
                toks.push(Tok::Date(s));
            }
            'B' | 'b' | 'G' | 'g' if src[src.char_indices().nth(i).map(|x| x.0).unwrap_or(0)..].eq_ignore_ascii_case("general") => {
                toks.push(Tok::Num("General".into()));
                i = ch.len();
            }
            _ => {
                lit(&mut toks, &c.to_string());
                i += 1;
            }
        }
    }
    let date = toks.iter().any(|t| matches!(t, Tok::Date(_)));
    // "m" next to hours or seconds means minutes
    if date {
        let n = toks.len();
        for k in 0..n {
            if let Tok::Date(d) = &toks[k] {
                if d == "m" || d == "mm" {
                    let prev_h = toks[..k].iter().rev().find_map(|t| if let Tok::Date(x) = t { Some(x.starts_with('h') || x.starts_with("[h")) } else { None }).unwrap_or(false);
                    let next_s = toks[k + 1..].iter().find_map(|t| if let Tok::Date(x) = t { Some(x.starts_with('s')) } else { None }).unwrap_or(false);
                    if prev_h || next_s {
                        toks[k] = Tok::Date(if d == "m" { "M".into() } else { "MM".into() });
                    }
                }
            }
        }
    }
    Section { toks, color, cond, date }
}

fn split_sections(code: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut q = false;
    let mut br = false;
    let mut esc = false;
    for c in code.chars() {
        if esc {
            out.last_mut().unwrap().push(c);
            esc = false;
            continue;
        }
        match c {
            '\\' => esc = true,
            '"' => q = !q,
            '[' if !q => br = true,
            ']' if !q => br = false,
            ';' if !q && !br => {
                out.push(String::new());
                continue;
            }
            _ => {}
        }
        out.last_mut().unwrap().push(c);
    }
    out
}

/// Whether a format shows dates or times.
pub fn is_date(code: &str) -> bool {
    split_sections(code).first().map(|s| parse_section(s).date).unwrap_or(false)
}

/// Shortest decimal for "General": up to 11 significant digits.
pub fn general(v: f64) -> String {
    if v == 0.0 {
        return "0".into();
    }
    if !v.is_finite() {
        return "#ЧИСЛО!".into();
    }
    let a = v.abs();
    if !(1e-9..1e11).contains(&a) {
        let s = format!("{:.5E}", v);
        let (m, e) = s.split_once('E').unwrap();
        let m = m.trim_end_matches('0').trim_end_matches('.').replace('.', &DECIMAL.to_string());
        let e: i32 = e.parse().unwrap_or(0);
        return format!("{m}E{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
    }
    let int_digits = (a.log10().floor() as i32 + 1).max(1);
    let dec = (10 - int_digits).clamp(0, 10) as usize;
    let s = format!("{:.*}", dec, v);
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    s.replace('.', &DECIMAL.to_string())
}

fn group(int: &str) -> String {
    let mut out = String::new();
    let n = int.len();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (n - i) % 3 == 0 {
            out.push(THOUSANDS);
        }
        out.push(c);
    }
    out
}

/// `v` by a number pattern like "#,##0.00" (sign handled by the caller).
fn number(v: f64, pat: &str) -> String {
    if pat == "General" {
        return general(v);
    }
    // trailing commas scale by 1000
    let mut pat = pat.to_string();
    let mut v = v;
    while pat.ends_with(',') {
        pat.pop();
        v /= 1000.0;
    }
    let (ip, fp) = pat.split_once('.').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or((pat.clone(), String::new()));
    let thousands = ip.contains(',');
    let ip: String = ip.chars().filter(|c| *c != ',').collect();
    let dec_min = fp.chars().filter(|c| *c == '0').count();
    let dec_max = fp.chars().filter(|c| "0#?".contains(*c)).count();
    let s = format!("{:.*}", dec_max, v);
    let (mut i, mut f) = s.split_once('.').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or((s.clone(), String::new()));
    while f.len() > dec_min && f.ends_with('0') {
        f.pop();
    }
    let int_min = ip.chars().filter(|c| *c == '0').count();
    if i == "0" && int_min == 0 {
        i.clear();
    }
    while i.len() < int_min {
        i.insert(0, '0');
    }
    let mut out = if thousands { group(&i) } else { i };
    if !f.is_empty() || (dec_max > 0 && dec_min > 0) {
        out.push(DECIMAL);
        out.push_str(&f);
    }
    out
}

fn civil(days: i64) -> (i64, u32, u32) {
    // days since 1970-01-01 → (y, m, d)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Excel serial date → (y, m, d, weekday 0=Monday, seconds of the day).
pub fn date_parts(serial: f64, date1904: bool) -> Option<(i64, u32, u32, usize, f64)> {
    if !(0.0..2_958_466.0).contains(&serial) {
        return None;
    }
    let mut days = serial.floor() as i64;
    let secs = ((serial - serial.floor()) * 86_400.0 * 1000.0).round() / 1000.0;
    let (y, m, d) = if date1904 {
        civil(days - 24_107)
    } else if days == 60 {
        // the 29th of February 1900 that never was
        (1900, 2, 29)
    } else {
        if days < 60 {
            days += 1;
        }
        civil(days - 25_569)
    };
    let wd = ((serial.floor() as i64 + if date1904 { 1462 } else { 0 } + 5).rem_euclid(7)) as usize;
    Some((y, m, d, wd, secs))
}

fn date(v: f64, sec: &Section, date1904: bool) -> String {
    let Some((y, mo, d, wd, secs)) = date_parts(v, date1904) else { return "#".repeat(8) };
    let ampm = sec.toks.contains(&Tok::AmPm);
    // round to the precision shown
    let frac = sec.toks.iter().find_map(|t| if let Tok::Date(s) = t { s.split_once('.').map(|(_, f)| f.len()) } else { None }).unwrap_or(0);
    let secs = if frac == 0 { secs.round() } else { secs };
    let (hh, mm, ss) = ((secs / 3600.0).floor() as i64, ((secs % 3600.0) / 60.0).floor() as i64, secs % 60.0);
    let total_h = v.floor() as i64 * 24 + hh;
    let mut out = String::new();
    for t in &sec.toks {
        match t {
            Tok::Lit(l) => out.push_str(l),
            Tok::Date(s) => match s.as_str() {
                "yy" => out.push_str(&format!("{:02}", y.rem_euclid(100))),
                s if s.starts_with('y') => out.push_str(&y.to_string()),
                "m" => out.push_str(&mo.to_string()),
                "mm" => out.push_str(&format!("{mo:02}")),
                "mmm" => out.push_str(MON[mo as usize - 1]),
                "mmmmm" => out.push_str(&MONTHS[mo as usize - 1][..MONTHS[mo as usize - 1].char_indices().nth(1).map(|x| x.0).unwrap_or(1)]),
                s if s.starts_with("mmmm") => out.push_str(MONTHS[mo as usize - 1]),
                "d" => out.push_str(&d.to_string()),
                "dd" => out.push_str(&format!("{d:02}")),
                "ddd" => out.push_str(DAY[wd]),
                s if s.starts_with("dddd") => out.push_str(DAYS[wd]),
                "h" | "hh" => {
                    let h = if ampm { (hh + 11) % 12 + 1 } else { hh };
                    out.push_str(&if s == "hh" { format!("{h:02}") } else { h.to_string() });
                }
                "[h]" | "[hh]" => out.push_str(&total_h.to_string()),
                "[m]" | "[mm]" => out.push_str(&(total_h * 60 + mm).to_string()),
                "M" => out.push_str(&mm.to_string()),
                "MM" => out.push_str(&format!("{mm:02}")),
                s if s.starts_with('s') => {
                    let w = if s.starts_with("ss") { 2 } else { 1 };
                    if frac > 0 {
                        out.push_str(&format!("{:0w$.frac$}", ss, w = w + 1 + frac, frac = frac).replace('.', &DECIMAL.to_string()));
                    } else {
                        out.push_str(&format!("{:0w$}", ss as i64, w = w));
                    }
                }
                _ => {}
            },
            Tok::AmPm => out.push_str(if hh < 12 { "AM" } else { "PM" }),
            _ => {}
        }
    }
    out
}

/// Text of a number under a format code, and the colour the format asks for.
pub fn format(v: f64, code: &str, date1904: bool) -> (String, Option<u32>) {
    let secs = split_sections(code);
    let parsed: Vec<Section> = secs.iter().map(|s| parse_section(s)).collect();
    // which section: conditions, else positive;negative;zero
    let has_cond = parsed.iter().any(|s| s.cond.is_some());
    let (idx, neg_in_section) = if has_cond {
        let hit = parsed.iter().position(|s| s.cond.as_ref().is_some_and(|(op, x)| match op.as_str() {
            "<" => v < *x,
            "<=" => v <= *x,
            ">" => v > *x,
            ">=" => v >= *x,
            "=" => v == *x,
            "<>" => v != *x,
            _ => false,
        }));
        (hit.unwrap_or(parsed.len().saturating_sub(1).min(2)), false)
    } else if v < 0.0 && parsed.len() >= 2 {
        (1, true)
    } else if v == 0.0 && parsed.len() >= 3 {
        (2, false)
    } else {
        (0, false)
    };
    let Some(sec) = parsed.get(idx) else { return (general(v), None) };
    if sec.date {
        return (date(v, sec, date1904), sec.color);
    }
    let mut a = if neg_in_section || has_cond { v.abs() } else { v };
    let percent = sec.toks.contains(&Tok::Percent);
    if percent {
        a *= 100.0;
    }
    let neg = a < 0.0;
    let a = a.abs();
    let mut out = String::new();
    let mut number_done = false;
    let exp = sec.toks.iter().find_map(|t| if let Tok::Exp(e) = t { Some(e.clone()) } else { None });
    for t in &sec.toks {
        match t {
            Tok::Lit(l) => out.push_str(l),
            Tok::Num(p) if !number_done => {
                number_done = true;
                if p.contains('/') || p.contains('?') && !p.contains('.') && p.chars().filter(|c| *c == '?').count() >= 2 && false {
                    out.push_str(&general(a));
                } else if let Some(e) = &exp {
                    let mant_dec = p.split_once('.').map(|(_, f)| f.len()).unwrap_or(0);
                    let s = format!("{:.*E}", mant_dec, a);
                    let (m, ex) = s.split_once('E').unwrap();
                    let ex: i32 = ex.parse().unwrap_or(0);
                    let digits = e.len().saturating_sub(2).max(1);
                    out.push_str(&m.replace('.', &DECIMAL.to_string()));
                    out.push('E');
                    out.push(if ex < 0 { '-' } else { '+' });
                    out.push_str(&format!("{:0digits$}", ex.abs(), digits = digits));
                } else {
                    out.push_str(&number(a, p));
                }
            }
            Tok::Percent => out.push('%'),
            Tok::Text => out.push_str(&general(v)),
            _ => {}
        }
    }
    if !number_done && !sec.toks.contains(&Tok::Text) && sec.toks.iter().all(|t| matches!(t, Tok::Lit(_))) && parsed.len() == 1 {
        // a format that is only text shows that text
    }
    if neg && !out.is_empty() && out.chars().any(|c| c.is_ascii_digit()) {
        out.insert(0, '-');
    }
    (out, sec.color)
}

/// Text under the text section of a format (`@`), if it has one.
pub fn format_text(s: &str, code: &str) -> String {
    let secs = split_sections(code);
    let sec = if secs.len() >= 4 { parse_section(&secs[3]) } else if secs.len() == 1 && secs[0].contains('@') { parse_section(&secs[0]) } else { return s.to_string() };
    let mut out = String::new();
    for t in &sec.toks {
        match t {
            Tok::Lit(l) => out.push_str(l),
            Tok::Text => out.push_str(s),
            _ => {}
        }
    }
    out
}
