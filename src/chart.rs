//! The three battery charts, drawn with cairo on a `gtk::DrawingArea`:
//! charge over time, drain by charge band, and predicted against realized
//! runtime. Each reads the shared monitor on every draw, so a
//! `queue_draw` after a sample is all it takes to keep them current.

use crate::history::{BANDS, Checkpoint, SESSION_GAP_SECS, duration_text};
use crate::monitor::Monitor;
use crate::power::PowerState;
use gtk::{cairo, glib, prelude::*};
use std::{cell::Cell, cell::RefCell, rc::Rc};

const MARGIN_LEFT: f64 = 44.0;
const MARGIN_RIGHT: f64 = 14.0;
const MARGIN_TOP: f64 = 12.0;
const MARGIN_BOTTOM: f64 = 24.0;
const FONT_SIZE: f64 = 11.0;

/// One line of the accuracy chart: name, colour, alpha, dash, and the
/// value it reads from a checkpoint.
type Series<'a> = (
    &'a str,
    Rgb,
    f64,
    &'a [f64],
    Box<dyn Fn(&Checkpoint) -> Option<f64>>,
);

#[derive(Clone, Copy)]
pub struct Rgb(pub f64, pub f64, pub f64);

impl Rgb {
    /// `#RRGGBB`; anything else is the Raven default blue.
    pub fn from_hex(hex: &str) -> Self {
        let channel = |i: usize| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map(|v| v as f64 / 255.0)
                .unwrap_or(0.5)
        };
        if crate::desktop::is_hex(hex) {
            Self(channel(1), channel(3), channel(5))
        } else {
            Self::from_hex(crate::desktop::DEFAULT_ACCENT)
        }
    }
}

/// The colours one chart draws with. Foreground comes from the widget's
/// style at draw time so the charts follow the light and dark sheets.
#[derive(Clone, Copy)]
pub struct Palette {
    pub fg: Rgb,
    pub accent: Rgb,
    pub charge: Rgb,
}

impl Palette {
    fn from_widget(widget: &gtk::DrawingArea, accent: Rgb) -> Self {
        let fg = widget.color();
        Self {
            fg: Rgb(fg.red() as f64, fg.green() as f64, fg.blue() as f64),
            accent,
            charge: Rgb(0.30, 0.78, 0.50),
        }
    }

    fn for_state(&self, state: PowerState) -> (Rgb, f64) {
        match state {
            PowerState::OnBattery => (self.accent, 1.0),
            PowerState::Charging => (self.charge, 1.0),
            PowerState::PluggedIn => (self.fg, 0.45),
        }
    }
}

fn set(cr: &cairo::Context, colour: Rgb, alpha: f64) {
    cr.set_source_rgba(colour.0, colour.1, colour.2, alpha);
}

fn text(cr: &cairo::Context, s: &str, x: f64, y: f64, align: f64) {
    let width = cr.text_extents(s).map(|e| e.width()).unwrap_or(0.0);
    cr.move_to(x - width * align, y);
    let _ = cr.show_text(s);
}

fn clock(t: u64, format: &str) -> String {
    glib::DateTime::from_unix_local(t as i64)
        .ok()
        .and_then(|d| d.format(format).ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// A plot rectangle with linear axes.
struct Plot {
    x0: f64,
    y0: f64,
    width: f64,
    height: f64,
    t_min: f64,
    t_max: f64,
    v_min: f64,
    v_max: f64,
}

impl Plot {
    fn new(width: i32, height: i32, t: (f64, f64), v: (f64, f64)) -> Self {
        Self {
            x0: MARGIN_LEFT,
            y0: MARGIN_TOP,
            width: (width as f64 - MARGIN_LEFT - MARGIN_RIGHT).max(1.0),
            height: (height as f64 - MARGIN_TOP - MARGIN_BOTTOM).max(1.0),
            t_min: t.0,
            t_max: t.1.max(t.0 + 1.0),
            v_min: v.0,
            v_max: v.1.max(v.0 + 1.0),
        }
    }

    fn x(&self, t: f64) -> f64 {
        self.x0 + (t - self.t_min) / (self.t_max - self.t_min) * self.width
    }

    fn y(&self, v: f64) -> f64 {
        self.y0 + self.height - (v - self.v_min) / (self.v_max - self.v_min) * self.height
    }

    fn bottom(&self) -> f64 {
        self.y0 + self.height
    }

    /// Horizontal grid lines with labels on the left.
    fn grid(
        &self,
        cr: &cairo::Context,
        palette: &Palette,
        values: &[f64],
        label: impl Fn(f64) -> String,
    ) {
        cr.set_line_width(1.0);
        for &v in values {
            let y = self.y(v).round() + 0.5;
            set(cr, palette.fg, 0.10);
            cr.move_to(self.x0, y);
            cr.line_to(self.x0 + self.width, y);
            let _ = cr.stroke();
            set(cr, palette.fg, 0.55);
            text(cr, &label(v), self.x0 - 8.0, y + 4.0, 1.0);
        }
    }

    /// Time labels along the bottom at round intervals.
    fn time_axis(&self, cr: &cairo::Context, palette: &Palette) {
        let span = self.t_max - self.t_min;
        let step = [
            900.0, 1800.0, 3600.0, 7200.0, 10800.0, 21600.0, 43200.0, 86400.0,
        ]
        .into_iter()
        .find(|s| span / s <= 8.0)
        .unwrap_or(86400.0);
        let format = if span > 2.0 * 86400.0 {
            "%a %H:%M"
        } else {
            "%H:%M"
        };
        // Ticks on local round times, not on the window's start.
        let offset = glib::DateTime::now_local()
            .ok()
            .map(|d| d.utc_offset().as_seconds() as f64)
            .unwrap_or(0.0);
        let mut t = ((self.t_min + offset) / step).ceil() * step - offset;
        set(cr, palette.fg, 0.55);
        while t <= self.t_max {
            let x = self.x(t);
            set(cr, palette.fg, 0.10);
            cr.move_to(x.round() + 0.5, self.y0);
            cr.line_to(x.round() + 0.5, self.bottom());
            let _ = cr.stroke();
            set(cr, palette.fg, 0.55);
            text(cr, &clock(t as u64, format), x, self.bottom() + 15.0, 0.5);
            t += step;
        }
    }
}

fn prepare(cr: &cairo::Context) {
    cr.select_font_face("Sans", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(FONT_SIZE);
    cr.set_line_join(cairo::LineJoin::Round);
    cr.set_line_cap(cairo::LineCap::Round);
}

fn empty_message(cr: &cairo::Context, palette: &Palette, width: i32, height: i32, lines: &[&str]) {
    set(cr, palette.fg, 0.55);
    let mut y = height as f64 / 2.0 - (lines.len() as f64 - 1.0) * 8.0;
    for line in lines {
        text(cr, line, width as f64 / 2.0, y, 0.5);
        y += 16.0;
    }
}

fn area(height: i32) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_content_height(height);
    area.set_hexpand(true);
    area.add_css_class("chart");
    area
}

/// Charge percent over the last `range` seconds, coloured by power state,
/// adapter time shaded, and the model's projection dotted ahead of now.
pub fn charge_chart(
    monitor: Rc<RefCell<Monitor>>,
    range: Rc<Cell<u64>>,
    accent: Rgb,
) -> gtk::DrawingArea {
    let area = area(240);
    area.set_draw_func(move |widget, cr, width, height| {
        prepare(cr);
        let palette = Palette::from_widget(widget, accent);
        let monitor = monitor.borrow();
        let now = monitor.now as f64;
        let range = range.get() as f64;
        let estimate = &monitor.estimate;
        // Leave a quarter of the window for the projection when there is one.
        let ahead = if estimate.minutes.is_some() {
            range * 0.25
        } else {
            0.0
        };
        let plot = Plot::new(width, height, (now - range, now + ahead), (0.0, 100.0));
        plot.grid(cr, &palette, &[0.0, 25.0, 50.0, 75.0, 100.0], |v| {
            format!("{v:.0}%")
        });
        plot.time_axis(cr, &palette);

        let samples: Vec<_> = monitor
            .history
            .samples()
            .iter()
            .filter(|s| (s.t as f64) >= now - range - SESSION_GAP_SECS as f64)
            .collect();
        if samples.len() < 2 {
            empty_message(
                cr,
                &palette,
                width,
                height,
                &[
                    "No history yet",
                    "Samples are taken every minute while Raven Power is open",
                ],
            );
            return;
        }
        // Adapter time first, as a wash behind the line.
        for pair in samples.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if a.state == PowerState::OnBattery || b.t - a.t > SESSION_GAP_SECS {
                continue;
            }
            let (colour, _) = palette.for_state(a.state);
            set(cr, colour, 0.07);
            let x = plot.x(a.t as f64).max(plot.x0);
            cr.rectangle(x, plot.y0, plot.x(b.t as f64) - x, plot.height);
            let _ = cr.fill();
        }
        // The line itself, one stroke per segment so the colour can change.
        cr.set_line_width(2.0);
        for pair in samples.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if b.t - a.t > SESSION_GAP_SECS {
                continue;
            }
            let (colour, alpha) = palette.for_state(a.state);
            let (xa, ya) = (plot.x(a.t as f64), plot.y(a.level()));
            let (xb, yb) = (plot.x(b.t as f64), plot.y(b.level()));
            set(cr, colour, 0.10 * alpha);
            cr.move_to(xa, ya);
            cr.line_to(xb, yb);
            cr.line_to(xb, plot.bottom());
            cr.line_to(xa, plot.bottom());
            cr.close_path();
            let _ = cr.fill();
            set(cr, colour, alpha);
            cr.move_to(xa, ya);
            cr.line_to(xb, yb);
            let _ = cr.stroke();
        }
        // Now.
        let x_now = plot.x(now).round() + 0.5;
        set(cr, palette.fg, 0.35);
        cr.set_line_width(1.0);
        cr.move_to(x_now, plot.y0);
        cr.line_to(x_now, plot.bottom());
        let _ = cr.stroke();
        // The projection to empty or to full.
        if let Some(minutes) = estimate.minutes {
            let level = monitor.battery.percent as f64;
            let (target, colour) = match estimate.state {
                PowerState::Charging => (100.0, palette.charge),
                _ => (0.0, palette.accent),
            };
            let t_end = now + minutes as f64 * 60.0;
            // Clip to the window, keeping the slope.
            let t_draw = t_end.min(plot.t_max);
            let level_draw = level + (target - level) * (t_draw - now) / (t_end - now).max(1.0);
            set(cr, colour, 0.9);
            cr.set_line_width(1.5);
            cr.set_dash(&[3.0, 4.0], 0.0);
            cr.move_to(x_now, plot.y(level));
            cr.line_to(plot.x(t_draw), plot.y(level_draw));
            let _ = cr.stroke();
            cr.set_dash(&[], 0.0);
            let label = match estimate.state {
                PowerState::Charging => format!("full in {}", duration_text(minutes)),
                _ => format!("{} left", duration_text(minutes)),
            };
            set(cr, colour, 1.0);
            let y = (plot.y(level) - 8.0).max(plot.y0 + FONT_SIZE);
            text(
                cr,
                &label,
                (x_now + 6.0).min(plot.x0 + plot.width - 90.0),
                y,
                0.0,
            );
        }
    });
    area
}

/// Drain, percent per hour, in each ten-percent band of charge as learnt
/// from earlier sessions, with the band being crossed now highlighted and
/// the rate observed this session ruled across.
pub fn band_chart(monitor: Rc<RefCell<Monitor>>, accent: Rgb) -> gtk::DrawingArea {
    let area = area(200);
    area.set_draw_func(move |widget, cr, width, height| {
        prepare(cr);
        let palette = Palette::from_widget(widget, accent);
        let monitor = monitor.borrow();
        let rates = monitor.history.band_rates(PowerState::OnBattery);
        let observed = monitor
            .estimate
            .rate_pct_hr
            .filter(|_| monitor.estimate.state == PowerState::OnBattery);
        let top = rates
            .iter()
            .flatten()
            .copied()
            .chain(observed)
            .fold(0.0_f64, f64::max);
        if top <= 0.0 {
            empty_message(
                cr,
                &palette,
                width,
                height,
                &[
                    "No drain measured yet",
                    "Run on battery for a few minutes to start filling this in",
                ],
            );
            return;
        }
        let v_max = (top * 1.25 / 5.0).ceil() * 5.0;
        let plot = Plot::new(width, height, (0.0, BANDS as f64), (0.0, v_max));
        let step = if v_max > 40.0 { 10.0 } else { 5.0 };
        let mut grid = Vec::new();
        let mut v = 0.0;
        while v <= v_max {
            grid.push(v);
            v += step;
        }
        plot.grid(cr, &palette, &grid, |v| format!("{v:.0}%/h"));
        let here = (monitor.battery.percent as usize / 10).min(BANDS - 1);
        let slot = plot.width / BANDS as f64;
        let bar = slot * 0.62;
        // Bands run from full on the left to empty on the right.
        for (i, band) in (0..BANDS).rev().enumerate() {
            let x = plot.x0 + slot * i as f64 + (slot - bar) / 2.0;
            let label = format!("{}–{}", band * 10, band * 10 + 10);
            set(cr, palette.fg, if band == here { 0.9 } else { 0.55 });
            text(cr, &label, x + bar / 2.0, plot.bottom() + 15.0, 0.5);
            match rates[band] {
                Some(rate) => {
                    let y = plot.y(rate);
                    set(cr, palette.accent, if band == here { 1.0 } else { 0.5 });
                    cr.rectangle(x, y, bar, plot.bottom() - y);
                    let _ = cr.fill();
                    set(cr, palette.fg, 0.85);
                    text(cr, &format!("{rate:.1}"), x + bar / 2.0, y - 5.0, 0.5);
                }
                None => {
                    set(cr, palette.fg, 0.18);
                    cr.set_line_width(1.0);
                    cr.set_dash(&[3.0, 3.0], 0.0);
                    cr.rectangle(x + 0.5, plot.y0 + 0.5, bar - 1.0, plot.height - 1.0);
                    let _ = cr.stroke();
                    cr.set_dash(&[], 0.0);
                }
            }
        }
        if let Some(rate) = observed {
            let y = plot.y(rate).round() + 0.5;
            set(cr, palette.fg, 0.7);
            cr.set_line_width(1.0);
            cr.set_dash(&[5.0, 4.0], 0.0);
            cr.move_to(plot.x0, y);
            cr.line_to(plot.x0 + plot.width, y);
            let _ = cr.stroke();
            cr.set_dash(&[], 0.0);
            text(
                cr,
                &format!("now {rate:.1}%/h"),
                plot.x0 + plot.width - 4.0,
                y - 5.0,
                1.0,
            );
        }
    });
    area
}

/// Predicted minutes against the minutes the battery really went on to
/// take, for every scored prediction inside the window.
pub fn accuracy_chart(
    monitor: Rc<RefCell<Monitor>>,
    range: Rc<Cell<u64>>,
    accent: Rgb,
) -> gtk::DrawingArea {
    let area = area(220);
    area.set_draw_func(move |widget, cr, width, height| {
        prepare(cr);
        let palette = Palette::from_widget(widget, accent);
        let monitor = monitor.borrow();
        let now = monitor.now as f64;
        let range = range.get() as f64;
        let points: Vec<Checkpoint> = monitor
            .history
            .checkpoints()
            .into_iter()
            .filter(|c| c.t as f64 >= now - range)
            .collect();
        if points.len() < 2 {
            empty_message(
                cr,
                &palette,
                width,
                height,
                &[
                    "Nothing to score in this window yet",
                    "A prediction is scored once the session has run 15 more minutes",
                ],
            );
            return;
        }
        let top = points
            .iter()
            .flat_map(|c| [c.predicted_min, c.realized_min, c.instant_min.unwrap_or(0)])
            .max()
            .unwrap_or(60) as f64;
        let v_max = ((top * 1.15 / 60.0).ceil() * 60.0).max(60.0);
        let plot = Plot::new(width, height, (now - range, now), (0.0, v_max));
        let step = if v_max > 480.0 { 120.0 } else { 60.0 };
        let mut grid = Vec::new();
        let mut v = 0.0;
        while v <= v_max {
            grid.push(v);
            v += step;
        }
        plot.grid(cr, &palette, &grid, |v| format!("{:.0} h", v / 60.0));
        plot.time_axis(cr, &palette);

        let series: [Series; 3] = [
            (
                "Instant reading",
                palette.fg,
                0.35,
                &[3.0, 3.0],
                Box::new(|c| c.instant_min.map(f64::from)),
            ),
            (
                "Predicted",
                palette.accent,
                1.0,
                &[],
                Box::new(|c| Some(c.predicted_min as f64)),
            ),
            (
                "Realized",
                palette.fg,
                0.9,
                &[],
                Box::new(|c| Some(c.realized_min as f64)),
            ),
        ];
        for (_, colour, alpha, dash, value) in &series {
            set(cr, *colour, *alpha);
            cr.set_line_width(if dash.is_empty() { 2.0 } else { 1.5 });
            cr.set_dash(dash, 0.0);
            let mut pen_down = false;
            let mut last_t = 0u64;
            for point in &points {
                let Some(v) = value(point) else {
                    pen_down = false;
                    continue;
                };
                let (x, y) = (plot.x(point.t as f64), plot.y(v));
                if pen_down && point.t - last_t <= SESSION_GAP_SECS {
                    cr.line_to(x, y);
                } else {
                    let _ = cr.stroke();
                    cr.move_to(x, y);
                }
                pen_down = true;
                last_t = point.t;
            }
            let _ = cr.stroke();
            cr.set_dash(&[], 0.0);
        }
        // Legend, top right.
        let mut x = plot.x0 + plot.width - 8.0;
        for (name, colour, alpha, dash, _) in series.iter().rev() {
            let w = cr.text_extents(name).map(|e| e.width()).unwrap_or(0.0);
            x -= w;
            set(cr, palette.fg, 0.8);
            text(cr, name, x, plot.y0 + FONT_SIZE, 0.0);
            x -= 18.0;
            set(cr, *colour, *alpha);
            cr.set_line_width(2.0);
            cr.set_dash(dash, 0.0);
            cr.move_to(x, plot.y0 + FONT_SIZE - 4.0);
            cr.line_to(x + 14.0, plot.y0 + FONT_SIZE - 4.0);
            let _ = cr.stroke();
            cr.set_dash(&[], 0.0);
            x -= 16.0;
        }
    });
    area
}
