mod chart;
mod desktop;
mod history;
mod monitor;
mod power;

use adw::prelude::*;
use gtk::{gdk, glib};
use history::duration_text;
use monitor::Monitor;
use power::{PowerState, Settings};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

const APP_ID: &str = "org.raven.Power";

/// How often the window re-reads the battery. Samples are only kept once a
/// minute, so this is the latency of the labels, not the size of the history.
const REFRESH: Duration = Duration::from_secs(20);

/// The chart windows the history page offers, in seconds.
const RANGES: [(&str, u64); 4] = [
    ("Last 3 hours", 3 * 3600),
    ("Last 12 hours", 12 * 3600),
    ("Last 24 hours", 24 * 3600),
    ("Last 7 days", 7 * 24 * 3600),
];

/// One widget's reaction to a fresh reading.
type Updater = Box<dyn Fn(&Monitor)>;

/// Everything that follows the monitor: a closure per widget, run after
/// every refresh with the fresh state.
#[derive(Default)]
struct Live {
    updaters: RefCell<Vec<Updater>>,
}

impl Live {
    fn bind(&self, update: impl Fn(&Monitor) + 'static) {
        self.updaters.borrow_mut().push(Box::new(update));
    }

    fn label(&self, label: &gtk::Label, text: impl Fn(&Monitor) -> String + 'static) {
        let label = label.clone();
        self.bind(move |m| label.set_text(&text(m)));
    }

    fn visible(&self, widget: &impl IsA<gtk::Widget>, shown: impl Fn(&Monitor) -> bool + 'static) {
        let widget = widget.clone();
        self.bind(move |m| widget.set_visible(shown(m)));
    }

    fn redraw(&self, area: &gtk::DrawingArea) {
        let area = area.clone();
        self.bind(move |_| area.queue_draw());
    }

    fn run(&self, monitor: &Monitor) {
        for update in self.updaters.borrow().iter() {
            update(monitor);
        }
    }
}

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--apply-profile") {
        let profile = args.get(2).map(String::as_str).unwrap_or_default();
        return match power::apply_profile_sysfs(profile) {
            Ok(()) => glib::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("raven-power: {error}");
                glib::ExitCode::FAILURE
            }
        };
    }
    // `NON_UNIQUE`: a second launch is a second window, as for every Raven
    // app, rather than a raise of the first.
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_startup(|_| load_css());
    app.connect_activate(build_ui);
    app.run()
}

/// The shared Raven Glass sheet, then this app's own classes, in one
/// provider; the accent and light-mode overrides go in a second one above
/// it, exactly as Settings and Store layer theirs.
fn load_css() {
    let display = gdk::Display::default().expect("A graphical display is required");
    let provider = gtk::CssProvider::new();
    provider.load_from_string(concat!(
        include_str!("raven-glass.css"),
        include_str!("style.css")
    ));
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let desktop = desktop::Desktop::load();
    let look = &desktop.appearance;
    adw::StyleManager::default().set_color_scheme(match look.theme_mode {
        desktop::ThemeMode::Dark => adw::ColorScheme::ForceDark,
        desktop::ThemeMode::Light => adw::ColorScheme::ForceLight,
        desktop::ThemeMode::Auto => adw::ColorScheme::PreferDark,
    });
    let accent = desktop.accent();
    let mut css =
        format!("@define-color accent_bg_color {accent};\n@define-color accent_color {accent};\n");
    if look.theme_mode == desktop::ThemeMode::Light {
        css.push_str(include_str!("raven-glass-light.css"));
    }
    let overrides = gtk::CssProvider::new();
    overrides.load_from_string(&css);
    gtk::style_context_add_provider_for_display(
        &display,
        &overrides,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
    );
}

fn build_ui(app: &adw::Application) {
    let mut stored_settings = Settings::load();
    let system_profile = power::active_power_profile();
    if matches!(
        system_profile.as_str(),
        "performance" | "balanced" | "power-saver"
    ) {
        stored_settings.profile = system_profile;
    }
    let settings = Rc::new(RefCell::new(stored_settings));
    let monitor = Rc::new(RefCell::new(Monitor::start()));
    let live = Rc::new(Live::default());
    let accent = chart::Rgb::from_hex(desktop::Desktop::load().accent());
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Raven Power")
        .default_width(1180)
        .default_height(760)
        .build();
    window.add_css_class("raven");
    // Alpha only; the blur behind a glass window is the compositor's.
    if desktop::Desktop::load().appearance.transparency {
        window.add_css_class("glass");
    }
    let toast_overlay = adw::ToastOverlay::new();
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 10);
    sidebar.add_css_class("sidebar");
    let brand = gtk::Box::new(gtk::Orientation::Horizontal, 11);
    brand.add_css_class("brand");
    // The Raven mark, as /etc/os-release names it; the battery glyph only on
    // a system that has not installed the logo.
    let brand_icon = gtk::Image::from_icon_name("battery-good-symbolic");
    if let Some(display) = gtk::gdk::Display::default()
        && gtk::IconTheme::for_display(&display).has_icon("raven-logo")
    {
        brand_icon.set_icon_name(Some("raven-logo"));
    }
    brand.append(&brand_icon);
    let brand_text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let app_title = gtk::Label::new(Some("Raven Power"));
    app_title.set_xalign(0.0);
    app_title.add_css_class("app-title");
    brand_text.append(&app_title);
    let app_subtitle = gtk::Label::new(Some("Battery management"));
    app_subtitle.set_xalign(0.0);
    app_subtitle.add_css_class("app-subtitle");
    brand_text.append(&app_subtitle);
    brand.append(&brand_text);
    sidebar.append(&brand);

    let navigation = gtk::ListBox::new();
    navigation.add_css_class("navigation-sidebar");
    navigation.set_selection_mode(gtk::SelectionMode::Single);
    navigation.set_vexpand(true);
    // Each section's icon sits in a tinted tile; the tint names the domain,
    // never the accent, so the sidebar reads the same under any accent.
    let pages = [
        ("view-grid-symbolic", "Overview", "green"),
        (
            "utilities-system-monitor-symbolic",
            "Energy usage",
            "orange",
        ),
        ("power-profile-balanced-symbolic", "Power profiles", "blue"),
        (
            "application-x-executable-symbolic",
            "Applications",
            "purple",
        ),
        ("battery-good-symbolic", "Battery health", "red"),
        ("document-open-recent-symbolic", "Battery history", "teal"),
    ];
    for (icon, label, tint) in pages {
        navigation.append(&nav_row(icon, label, tint));
    }
    sidebar.append(&navigation);

    let sidebar_status = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    sidebar_status.add_css_class("raven-card");
    sidebar_status.add_css_class("status-card");
    sidebar_status.append(&gtk::Image::from_icon_name("battery-good-symbolic"));
    let sidebar_status_text = gtk::Box::new(gtk::Orientation::Vertical, 1);
    let status_title = gtk::Label::new(None);
    status_title.set_xalign(0.0);
    status_title.add_css_class("card-title");
    live.label(&status_title, |m| format!("{}% charged", m.battery.percent));
    sidebar_status_text.append(&status_title);
    let status_subtitle = gtk::Label::new(None);
    status_subtitle.set_xalign(0.0);
    status_subtitle.add_css_class("dim");
    live.label(&status_subtitle, |m| {
        format!(
            "{} · {}",
            m.estimate.state.label(),
            m.estimate.short(&m.battery)
        )
    });
    sidebar_status_text.append(&status_subtitle);
    sidebar_status.append(&sidebar_status_text);
    sidebar.append(&sidebar_status);

    let stack = gtk::Stack::builder()
        .hexpand(true)
        .vexpand(true)
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    stack.add_named(
        &overview_page(settings.clone(), &toast_overlay, &live),
        Some("overview"),
    );
    stack.add_named(&usage_page(&live), Some("usage"));
    stack.add_named(
        &profiles_page(settings.clone(), &toast_overlay),
        Some("profiles"),
    );
    stack.add_named(
        &applications_page(settings.clone(), &toast_overlay),
        Some("applications"),
    );
    stack.add_named(
        &health_page(settings.clone(), &toast_overlay),
        Some("health"),
    );
    stack.add_named(
        &history_page(monitor.clone(), &live, accent),
        Some("history"),
    );
    let title = adw::WindowTitle::new("Power overview", "Live battery status and power controls");
    navigation.connect_row_selected(glib::clone!(
        #[weak]
        stack,
        #[weak]
        title,
        move |_, row| {
            let Some(row) = row else {
                return;
            };
            let data = [
                (
                    "overview",
                    "Power overview",
                    "Live battery status and power controls",
                ),
                (
                    "usage",
                    "Energy usage",
                    "See exactly where your battery power goes",
                ),
                (
                    "profiles",
                    "Power profiles",
                    "Tune performance and battery life",
                ),
                (
                    "applications",
                    "Applications",
                    "Control power use for each application",
                ),
                (
                    "health",
                    "Battery health",
                    "Capacity, charging, and long-term battery care",
                ),
                (
                    "history",
                    "Battery history",
                    "Charge over time, drain by level, and how the estimates held up",
                ),
            ];
            if let Some((page, heading, subtitle)) = data.get(row.index() as usize) {
                stack.set_visible_child_name(page);
                title.set_title(heading);
                title.set_subtitle(subtitle);
            }
        }
    ));
    navigation.select_row(navigation.row_at_index(0).as_ref());

    let header = adw::HeaderBar::builder().show_title(true).build();
    header.set_title_widget(Some(&title));
    let show_sidebar = gtk::ToggleButton::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text("Sections")
        .visible(false)
        .build();
    header.pack_start(&show_sidebar);
    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.set_tooltip_text(Some("Refresh battery information"));
    header.pack_end(&refresh);

    let toolbar = adw::ToolbarView::new();
    toolbar.set_top_bar_style(adw::ToolbarStyle::Raised);
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&stack));
    let sidebar_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(false)
        .child(&sidebar)
        .build();
    let split = adw::OverlaySplitView::builder()
        .sidebar(&sidebar_scroller)
        .content(&toolbar)
        .sidebar_width_fraction(0.22)
        .min_sidebar_width(220.0)
        .max_sidebar_width(270.0)
        .build();
    split
        .bind_property("show-sidebar", &show_sidebar, "active")
        .bidirectional()
        .sync_create()
        .build();
    let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        880.0,
        adw::LengthUnit::Px,
    ));
    narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
    narrow.add_setter(&show_sidebar, "visible", Some(&true.to_value()));
    window.add_breakpoint(narrow);
    navigation.connect_row_activated(glib::clone!(
        #[weak]
        split,
        move |_, _| {
            if split.is_collapsed() {
                split.set_show_sidebar(false);
            }
        }
    ));
    refresh.connect_clicked(glib::clone!(
        #[weak]
        window,
        move |_| {
            window.close();
            if let Some(app) = window.application() {
                app.activate();
            }
        }
    ));

    toast_overlay.set_child(Some(&split));
    window.set_content(Some(&toast_overlay));
    window.set_size_request(480, 360);
    window.set_default_size(1080, 700);
    window.present();

    // First paint from the reading taken at startup, then a fresh reading
    // every REFRESH for as long as this window lives.
    live.run(&monitor.borrow());
    let weak_window = window.downgrade();
    glib::timeout_add_local(REFRESH, move || {
        if weak_window.upgrade().is_none() {
            return glib::ControlFlow::Break;
        }
        monitor.borrow_mut().refresh();
        live.run(&monitor.borrow());
        glib::ControlFlow::Continue
    });
}

fn nav_row(icon: &str, label: &str, tint: &str) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    let box_ = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let tile = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tile.add_css_class("nav-icon");
    tile.add_css_class(tint);
    tile.set_valign(gtk::Align::Center);
    let image = gtk::Image::from_icon_name(icon);
    image.set_halign(gtk::Align::Center);
    image.set_hexpand(true);
    tile.append(&image);
    box_.append(&tile);
    let text = gtk::Label::new(Some(label));
    text.set_xalign(0.0);
    box_.append(&text);
    row.set_child(Some(&box_));
    row
}

fn page_scroll(content: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(content)
        .build();
    scroll.add_css_class("page-scroll");
    scroll
}

fn overview_page(
    settings: Rc<RefCell<Settings>>,
    toasts: &adw::ToastOverlay,
    live: &Live,
) -> gtk::ScrolledWindow {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 18);
    page.add_css_class("page");
    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 28);
    hero.add_css_class("card");
    hero.add_css_class("hero");
    let gauge = gtk::Box::new(gtk::Orientation::Vertical, 0);
    gauge.add_css_class("battery-gauge");
    let percent = gtk::Label::new(None);
    percent.add_css_class("battery-percent");
    live.label(&percent, |m| format!("{}%", m.battery.percent));
    gauge.append(&percent);
    let gauge_caption = gtk::Label::new(None);
    live.label(&gauge_caption, |m| {
        match m.estimate.state {
            PowerState::OnBattery => "remaining",
            PowerState::Charging => "charging",
            PowerState::PluggedIn => "plugged in",
        }
        .into()
    });
    gauge.append(&gauge_caption);
    hero.append(&gauge);
    let summary = gtk::Box::new(gtk::Orientation::Vertical, 6);
    summary.set_hexpand(true);
    let eyebrow = gtk::Label::new(None);
    eyebrow.set_xalign(0.0);
    eyebrow.add_css_class("eyebrow");
    live.label(&eyebrow, |m| {
        if m.battery.is_real {
            "●  LIVE SYSTEM ESTIMATE"
        } else {
            "●  DEMO DATA — NO BATTERY FOUND"
        }
        .into()
    });
    summary.append(&eyebrow);
    let estimate = gtk::Label::new(None);
    estimate.set_xalign(0.0);
    estimate.add_css_class("hero-title");
    live.label(&estimate, |m| m.estimate.headline(&m.battery));
    summary.append(&estimate);
    let status = gtk::Label::new(None);
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    live.label(&status, |m| {
        format!(
            "{} · {} · {}% battery health",
            m.estimate.state.label(),
            power_text(m),
            m.battery.health_percent()
        )
    });
    summary.append(&status);
    let basis = gtk::Label::new(None);
    basis.set_xalign(0.0);
    basis.set_wrap(true);
    basis.add_css_class("dim-label");
    live.label(&basis, basis_text);
    summary.append(&basis);
    let stats = gtk::Box::new(gtk::Orientation::Horizontal, 36);
    stats.set_margin_top(18);
    let (draw, draw_value) = stat("Power");
    live.label(&draw_value, power_text);
    stats.append(&draw);
    let (health, health_value) = stat("Battery health");
    live.label(&health_value, |m| {
        let health = m.battery.health_percent();
        format!(
            "{health}% · {}",
            if health > 85 { "Excellent" } else { "Worn" }
        )
    });
    stats.append(&health);
    let (temperature, temperature_value) = stat("Temperature");
    live.label(&temperature_value, |m| {
        m.battery
            .temperature_c
            .map(|v| format!("{v:.0}°C · Normal"))
            .unwrap_or_else(|| "Unavailable".into())
    });
    stats.append(&temperature);
    summary.append(&stats);
    hero.append(&summary);
    page.append(&hero);

    let alert = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    alert.add_css_class("alert-card");
    let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
    icon.add_css_class("alert-icon");
    alert.append(&icon);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let heading = gtk::Label::new(Some("Battery drain is higher than expected"));
    heading.set_xalign(0.0);
    heading.add_css_class("card-title");
    text.append(&heading);
    let body = gtk::Label::new(Some(
        "Review active applications and display brightness to recover runtime.",
    ));
    body.set_xalign(0.0);
    body.add_css_class("dim-label");
    text.append(&body);
    alert.append(&text);
    // Only a drain can be too high; the adapter carrying 30 W is not one.
    live.visible(&alert, |m| {
        m.estimate.state == PowerState::OnBattery
            && (m.battery.power_watts > 12.0 || !m.battery.is_real)
    });
    page.append(&alert);
    let heading = section_title(
        "Choose your power mode",
        "Raven applies profiles through raven-powerd or the Linux power-profile service.",
    );
    page.append(&heading);
    let profiles = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    profiles.set_homogeneous(true);
    for (id, name, icon, desc, estimate) in [
        (
            "performance",
            "Performance",
            "power-profile-performance-symbolic",
            "Maximum speed for demanding work.",
            "Fastest",
        ),
        (
            "balanced",
            "Balanced",
            "power-profile-balanced-symbolic",
            "A smart mix of speed and battery life.",
            "Recommended",
        ),
        (
            "power-saver",
            "Eco",
            "power-profile-power-saver-symbolic",
            "Quieter background work and longer runtime.",
            "+1 hr 19 min",
        ),
    ] {
        let card = profile_card(name, icon, desc, estimate, settings.borrow().profile == id);
        let gesture = gtk::GestureClick::new();
        let id = id.to_string();
        let s = settings.clone();
        let toast = toasts.clone();
        gesture.connect_released(move |_, _, _, _| match power::set_power_profile(&id) {
            Ok(_) => {
                s.borrow_mut().profile = id.clone();
                s.borrow().save();
                toast.add_toast(adw::Toast::new(&format!("{name} profile activated")));
            }
            Err(e) => toast.add_toast(adw::Toast::new(&e)),
        });
        card.add_controller(gesture);
        profiles.append(&card);
    }
    page.append(&profiles);
    let tips = gtk::Box::new(gtk::Orientation::Vertical, 0);
    tips.add_css_class("card");
    tips.append(&section_title(
        "Quick savings",
        "Small changes that make the battery last longer.",
    ));
    let bg = switch_row(
        "Limit background activity",
        "Reduce work from applications you are not using",
        settings.borrow().background_saving,
    );
    let s = settings.clone();
    bg.1.connect_active_notify(move |v| {
        s.borrow_mut().background_saving = v.is_active();
        s.borrow().save();
    });
    tips.append(&bg.0);
    let wifi = switch_row(
        "Wi-Fi power saving",
        "Reduce wireless power between transfers",
        settings.borrow().wifi_saving,
    );
    let s = settings.clone();
    wifi.1.connect_active_notify(move |v| {
        s.borrow_mut().wifi_saving = v.is_active();
        s.borrow().save();
    });
    tips.append(&wifi.0);
    page.append(&tips);
    page_scroll(&page)
}

fn usage_page(live: &Live) -> gtk::ScrolledWindow {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 16);
    page.add_css_class("page");
    let metrics = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    metrics.set_homogeneous(true);
    let (rate, rate_value, rate_detail) = metric("Power");
    live.label(&rate_value, power_text);
    live.label(&rate_detail, |m| {
        match m.estimate.state {
            PowerState::OnBattery => "Drawn from the battery right now",
            PowerState::Charging => "Flowing into the battery right now",
            PowerState::PluggedIn => "The adapter is carrying the load",
        }
        .into()
    });
    metrics.append(&rate);
    let (remaining, remaining_value, remaining_detail) = metric("Battery remaining");
    live.label(&remaining_value, |m| format!("{}%", m.battery.percent));
    live.label(&remaining_detail, |m| m.estimate.short(&m.battery));
    metrics.append(&remaining);
    let (capacity, capacity_value, capacity_detail) = metric("Full capacity");
    live.label(&capacity_value, |m| {
        format!("{:.1} Wh", m.battery.energy_full_wh)
    });
    capacity_detail.set_text("Measured maximum charge");
    metrics.append(&capacity);
    let (status, status_value, status_detail) = metric("Battery status");
    live.label(&status_value, |m| m.estimate.state.label().into());
    live.label(&status_detail, |m| {
        format!("{} · {}", m.battery.status, m.battery.battery_name)
    });
    metrics.append(&status);
    page.append(&metrics);
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("card");
    card.append(&section_title(
        "What’s using resources",
        "Processes ranked by total CPU time. Power figures require kernel energy counters.",
    ));
    for process in power::active_processes().into_iter().take(8) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("data-row");
        let icon = gtk::Image::from_icon_name("application-x-executable-symbolic");
        row.append(&icon);
        let name = gtk::Label::new(Some(&process.name));
        name.set_xalign(0.0);
        name.set_hexpand(true);
        row.append(&name);
        let info = gtk::Label::new(Some(&format!(
            "{} processes  ·  {:.0} MB",
            process.pids.len(),
            process.memory_mb
        )));
        info.add_css_class("dim-label");
        row.append(&info);
        card.append(&row);
    }
    page.append(&card);
    page_scroll(&page)
}

fn profiles_page(
    settings: Rc<RefCell<Settings>>,
    toasts: &adw::ToastOverlay,
) -> gtk::ScrolledWindow {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 16);
    page.add_css_class("page");
    page.append(&section_title(
        "Granular profile controls",
        "These preferences are saved locally for Raven's power service.",
    ));
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("card");
    let cpu = scale_row(
        "CPU performance limit",
        "Maximum processor capacity while on battery",
        20.0,
        100.0,
        settings.borrow().cpu_limit as f64,
        "%",
    );
    let s = settings.clone();
    let t = toasts.clone();
    cpu.1.connect_value_changed(move |v| {
        s.borrow_mut().cpu_limit = v.value() as u8;
        s.borrow().save();
        t.add_toast(adw::Toast::new("CPU limit saved"));
    });
    card.append(&cpu.0);
    let bright = scale_row(
        "Screen brightness cap",
        "Maximum brightness while on battery",
        20.0,
        100.0,
        settings.borrow().brightness_limit as f64,
        "%",
    );
    let s = settings.clone();
    bright.1.connect_value_changed(move |v| {
        s.borrow_mut().brightness_limit = v.value() as u8;
        s.borrow().save();
    });
    card.append(&bright.0);
    page.append(&card);
    let note = gtk::Label::new(Some(
        "Hardware enforcement for CPU and brightness limits is designed to be applied by the Raven privileged power service. Profile switching already uses power-profiles-daemon.",
    ));
    note.set_wrap(true);
    note.set_xalign(0.0);
    note.add_css_class("info-note");
    page.append(&note);
    page_scroll(&page)
}

fn applications_page(
    settings: Rc<RefCell<Settings>>,
    toasts: &adw::ToastOverlay,
) -> gtk::ScrolledWindow {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 16);
    page.add_css_class("page");
    page.append(&section_title(
        "Application eco mode",
        "Lower the CPU scheduling priority of selected applications.",
    ));
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("card");
    for process in power::active_processes() {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("data-row");
        row.append(&gtk::Image::from_icon_name(
            "application-x-executable-symbolic",
        ));
        let labels = gtk::Box::new(gtk::Orientation::Vertical, 1);
        labels.set_hexpand(true);
        let name = gtk::Label::new(Some(&process.name));
        name.set_xalign(0.0);
        name.add_css_class("row-title");
        labels.append(&name);
        let detail = gtk::Label::new(Some(&format!(
            "{} running process{} · {:.0} MB",
            process.pids.len(),
            if process.pids.len() == 1 { "" } else { "es" },
            process.memory_mb
        )));
        detail.set_xalign(0.0);
        detail.add_css_class("dim-label");
        labels.append(&detail);
        row.append(&labels);
        let toggle = gtk::Switch::new();
        toggle.set_valign(gtk::Align::Center);
        toggle.set_active(settings.borrow().eco_apps.contains(&process.name));
        let process = process.clone();
        let s = settings.clone();
        let toast = toasts.clone();
        toggle.connect_state_set(move |_, eco| {
            match power::set_process_eco(&process.pids, eco) {
                Ok(_) => {
                    let mut config = s.borrow_mut();
                    if eco && !config.eco_apps.contains(&process.name) {
                        config.eco_apps.push(process.name.clone());
                    } else if !eco {
                        config.eco_apps.retain(|n| n != &process.name);
                    }
                    config.save();
                    toast.add_toast(adw::Toast::new(if eco {
                        "Eco mode enabled"
                    } else {
                        "Normal priority requested"
                    }));
                }
                Err(e) => toast.add_toast(adw::Toast::new(&e)),
            }
            glib::Propagation::Proceed
        });
        row.append(&gtk::Label::new(Some("Eco")));
        row.append(&toggle);
        card.append(&row);
    }
    page.append(&card);
    page_scroll(&page)
}

fn health_page(settings: Rc<RefCell<Settings>>, toasts: &adw::ToastOverlay) -> gtk::ScrolledWindow {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 16);
    page.add_css_class("page");
    let b = power::BatteryInfo::read();
    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 24);
    hero.add_css_class("card");
    hero.add_css_class("hero");
    let ring = gtk::Label::new(Some(&format!("{}%\ncapacity", b.health_percent())));
    ring.add_css_class("health-ring");
    hero.append(&ring);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 5);
    text.append(&section_title(
        if b.health_percent() > 85 {
            "Your battery is in excellent condition"
        } else {
            "Your battery has experienced wear"
        },
        &format!(
            "Maximum capacity is {:.1} Wh of the original {:.1} Wh.",
            b.energy_full_wh, b.energy_design_wh
        ),
    ));
    let facts = format!(
        "Cycle count: {}   ·   Temperature: {}",
        b.cycle_count
            .map(|v| v.to_string())
            .unwrap_or_else(|| "Unavailable".into()),
        b.temperature_c
            .map(|v| format!("{v:.0}°C"))
            .unwrap_or_else(|| "Unavailable".into())
    );
    text.append(&gtk::Label::new(Some(&facts)));
    hero.append(&text);
    page.append(&hero);
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("card");
    card.append(&section_title(
        "Charge limit",
        "Reduce long-term wear by avoiding a constant full charge.",
    ));
    let scale = scale_row(
        "Maximum charge",
        "Recommended: 80%",
        50.0,
        100.0,
        settings.borrow().charge_limit as f64,
        "%",
    );
    let s = settings.clone();
    let toast = toasts.clone();
    scale.1.connect_value_changed(move |v| {
        s.borrow_mut().charge_limit = v.value() as u8;
        s.borrow().save();
        toast.add_toast(adw::Toast::new("Charge limit preference saved"));
    });
    card.append(&scale.0);
    page.append(&card);
    let note = gtk::Label::new(Some(
        "The preferred limit is saved. Enforcing it requires vendor-specific battery-controller support through Raven's privileged service.",
    ));
    note.set_wrap(true);
    note.set_xalign(0.0);
    note.add_css_class("info-note");
    page.append(&note);
    page_scroll(&page)
}

fn section_title(title: &str, subtitle: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let h = gtk::Label::new(Some(title));
    h.set_xalign(0.0);
    h.add_css_class("section-title");
    b.append(&h);
    let s = gtk::Label::new(Some(subtitle));
    s.set_wrap(true);
    s.set_xalign(0.0);
    s.add_css_class("dim-label");
    b.append(&s);
    b
}
/// A caption over a value; the value label comes back for live binding.
fn stat(label: &str) -> (gtk::Box, gtk::Label) {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let l = gtk::Label::new(Some(label));
    l.set_xalign(0.0);
    l.add_css_class("dim-label");
    b.append(&l);
    let v = gtk::Label::new(None);
    v.set_xalign(0.0);
    v.add_css_class("stat-value");
    b.append(&v);
    (b, v)
}
/// A metric card; the value and detail labels come back for live binding.
fn metric(label: &str) -> (gtk::Box, gtk::Label, gtk::Label) {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 5);
    b.add_css_class("metric-card");
    b.append(&gtk::Label::new(Some(label)));
    let v = gtk::Label::new(None);
    v.add_css_class("metric-value");
    b.append(&v);
    let d = gtk::Label::new(None);
    d.add_css_class("dim-label");
    d.set_wrap(true);
    d.set_justify(gtk::Justification::Center);
    b.append(&d);
    (b, v, d)
}

/// "10.6 W draw", "28.0 W in", or "0 W · on adapter".
fn power_text(m: &Monitor) -> String {
    match m.estimate.state {
        PowerState::OnBattery => format!("{:.1} W draw", m.battery.power_watts),
        PowerState::Charging => format!("{:.1} W in", m.battery.power_watts),
        PowerState::PluggedIn => "0 W · on adapter".into(),
    }
}

/// The line under the headline that says where the estimate came from.
fn basis_text(m: &Monitor) -> String {
    let e = &m.estimate;
    if e.state == PowerState::PluggedIn {
        return "The battery is not being drawn on.".into();
    }
    let naive = e
        .instant_minutes
        .map(|i| format!(" A naive meter would say {}.", duration_text(i)))
        .unwrap_or_default();
    match e.observed_pct_hr {
        Some(rate) => format!(
            "From {rate:.1}%/h observed over the last {}{}.{naive}",
            duration_text(e.observed_minutes.max(1)),
            if e.basis == history::Basis::Learned {
                ", shaped by earlier sessions"
            } else {
                ""
            }
        ),
        None => "From this instant's power reading only. It steadies after a few minutes.".into(),
    }
}

fn history_page(
    monitor: Rc<RefCell<Monitor>>,
    live: &Live,
    accent: chart::Rgb,
) -> gtk::ScrolledWindow {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 16);
    page.add_css_class("page");

    let metrics = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    metrics.set_homogeneous(true);
    let (estimate, estimate_value, estimate_detail) = metric("Estimated");
    live.label(&estimate_value, |m| m.estimate.short(&m.battery));
    live.label(&estimate_detail, |m| m.estimate.basis_text().into());
    metrics.append(&estimate);
    let (observed, observed_value, observed_detail) = metric("Observed drain");
    live.label(&observed_value, |m| match m.estimate.observed_pct_hr {
        Some(rate) => format!("{rate:.1}%/h"),
        None => "—".into(),
    });
    live.label(&observed_detail, |m| {
        match (m.estimate.state, m.estimate.observed_pct_hr) {
            (PowerState::OnBattery, Some(rate)) => format!(
                "{:.1} W over the last {}",
                rate / 100.0 * m.battery.energy_full_wh,
                duration_text(m.estimate.observed_minutes.max(1))
            ),
            (PowerState::Charging, Some(_)) => format!(
                "Charging, over the last {}",
                duration_text(m.estimate.observed_minutes.max(1))
            ),
            (PowerState::PluggedIn, _) => "Not on battery".into(),
            (_, None) => "Needs a minute of samples".into(),
        }
    });
    metrics.append(&observed);
    let (instant, instant_value, instant_detail) = metric("Instant reading");
    live.label(&instant_value, |m| {
        m.estimate
            .instant_minutes
            .map(duration_text)
            .unwrap_or_else(|| "—".into())
    });
    live.label(&instant_detail, |m| {
        format!("{} · what a naive meter shows", power_text(m))
    });
    metrics.append(&instant);
    let (accuracy, accuracy_value, accuracy_detail) = metric("Estimate accuracy");
    live.label(&accuracy_value, |m| {
        match m.history.accuracy(m.now.saturating_sub(7 * 24 * 3600)) {
            Some(a) => format!("±{:.0} min", a.model_error_min),
            None => "—".into(),
        }
    });
    live.label(&accuracy_detail, |m| {
        match m.history.accuracy(m.now.saturating_sub(7 * 24 * 3600)) {
            Some(a) => {
                let instant = a
                    .instant_error_min
                    .map(|e| format!(" · naive ±{e:.0} min"))
                    .unwrap_or_default();
                format!(
                    "{} scored this week · {:.0}% within 10%{instant}",
                    a.checkpoints,
                    a.within_ten_percent * 100.0
                )
            }
            None => "Nothing scored yet".into(),
        }
    });
    metrics.append(&accuracy);
    page.append(&metrics);

    let range = Rc::new(Cell::new(RANGES[0].1));
    let charge_card = gtk::Box::new(gtk::Orientation::Vertical, 12);
    charge_card.add_css_class("card");
    let charge_header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let charge_title = section_title(
        "Charge over time",
        "One sample a minute while Raven Power is open. The dotted line is the model's projection from now.",
    );
    charge_title.set_hexpand(true);
    charge_header.append(&charge_title);
    let names: Vec<&str> = RANGES.iter().map(|(name, _)| *name).collect();
    let picker = gtk::DropDown::from_strings(&names);
    picker.set_valign(gtk::Align::Start);
    charge_header.append(&picker);
    charge_card.append(&charge_header);
    let legend = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    for (class, name) in [
        ("legend-battery", "●  On battery"),
        ("legend-charge", "●  Charging"),
        ("legend-plugged", "●  Plugged in"),
    ] {
        let item = gtk::Label::new(Some(name));
        item.add_css_class("dim-label");
        item.add_css_class(class);
        legend.append(&item);
    }
    charge_card.append(&legend);
    let charge = chart::charge_chart(monitor.clone(), range.clone(), accent);
    live.redraw(&charge);
    charge_card.append(&charge);
    page.append(&charge_card);

    let band_card = gtk::Box::new(gtk::Orientation::Vertical, 12);
    band_card.add_css_class("card");
    band_card.append(&section_title(
        "Drain by charge level",
        "Percent per hour measured in each band across every session on battery, weighted by time. Cells drain faster in percent terms as they empty, and the model uses this curve to shape the estimate below the current level. The bright bar is the band being crossed now.",
    ));
    let bands = chart::band_chart(monitor.clone(), accent);
    live.redraw(&bands);
    band_card.append(&bands);
    page.append(&band_card);

    let accuracy_card = gtk::Box::new(gtk::Orientation::Vertical, 12);
    accuracy_card.add_css_class("card");
    accuracy_card.append(&section_title(
        "How past estimates held up",
        "Every prediction is kept and scored once the session has run on: the realized line is the time the rest of the charge really took at the drain that followed. The closer the predicted line hugs it, the better.",
    ));
    let scores = chart::accuracy_chart(monitor.clone(), range.clone(), accent);
    live.redraw(&scores);
    accuracy_card.append(&scores);
    let checkpoints = gtk::Box::new(gtk::Orientation::Vertical, 0);
    live.bind(glib::clone!(
        #[weak]
        checkpoints,
        move |m| {
            while let Some(child) = checkpoints.first_child() {
                checkpoints.remove(&child);
            }
            // The latest few, at least twenty minutes apart, newest first.
            let mut shown: Vec<history::Checkpoint> = Vec::new();
            for point in m.history.checkpoints().into_iter().rev() {
                if shown
                    .last()
                    .is_none_or(|last| last.t.saturating_sub(point.t) >= 20 * 60)
                {
                    shown.push(point);
                }
                if shown.len() == 6 {
                    break;
                }
            }
            for point in shown {
                checkpoints.append(&checkpoint_row(&point));
            }
        }
    ));
    accuracy_card.append(&checkpoints);
    page.append(&accuracy_card);

    let note = gtk::Label::new(Some(
        "History is recorded only while Raven Power is open and is kept for 30 days in ~/.local/share/raven-power/history.jsonl. The Refresh button reloads it.",
    ));
    note.set_wrap(true);
    note.set_xalign(0.0);
    note.add_css_class("info-note");
    page.append(&note);

    picker.connect_selected_notify(move |picker| {
        if let Some((_, seconds)) = RANGES.get(picker.selected() as usize) {
            range.set(*seconds);
            charge.queue_draw();
            scores.queue_draw();
        }
    });
    page_scroll(&page)
}

fn checkpoint_row(point: &history::Checkpoint) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("data-row");
    let when = glib::DateTime::from_unix_local(point.t as i64)
        .ok()
        .and_then(|d| d.format("%a %H:%M").ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let verb = match point.state {
        PowerState::Charging => "to full",
        _ => "left",
    };
    let title = gtk::Label::new(Some(&format!("{when} at {}%", point.percent)));
    title.set_xalign(0.0);
    title.add_css_class("row-title");
    title.set_size_request(130, -1);
    row.append(&title);
    let detail = gtk::Label::new(Some(&format!(
        "Predicted {} {verb} · really {}",
        duration_text(point.predicted_min),
        duration_text(point.realized_min)
    )));
    detail.set_xalign(0.0);
    detail.set_hexpand(true);
    detail.add_css_class("dim-label");
    row.append(&detail);
    let error = point.error_min();
    let off = duration_text(error.unsigned_abs() as u32);
    let verdict = gtk::Label::new(Some(&if error.unsigned_abs() < 3 {
        "spot on".to_string()
    } else if error > 0 {
        format!("{off} too optimistic")
    } else {
        format!("{off} too cautious")
    }));
    let within = error.unsigned_abs() as f64 <= 0.10 * point.realized_min.max(1) as f64;
    verdict.add_css_class(if within { "green-label" } else { "dim-label" });
    row.append(&verdict);
    row
}
fn profile_card(name: &str, icon: &str, desc: &str, detail: &str, active: bool) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 8);
    b.add_css_class("profile-card");
    if active {
        b.add_css_class("active-profile");
    }
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 9);
    top.append(&gtk::Image::from_icon_name(icon));
    let n = gtk::Label::new(Some(name));
    n.add_css_class("row-title");
    top.append(&n);
    b.append(&top);
    let d = gtk::Label::new(Some(desc));
    d.set_wrap(true);
    d.set_xalign(0.0);
    d.add_css_class("dim-label");
    b.append(&d);
    let info = gtk::Label::new(Some(detail));
    info.set_xalign(0.0);
    info.add_css_class("green-label");
    b.append(&info);
    b
}
fn switch_row(title: &str, subtitle: &str, active: bool) -> (gtk::Box, gtk::Switch) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("data-row");
    let text = section_title(title, subtitle);
    text.set_hexpand(true);
    row.append(&text);
    let toggle = gtk::Switch::new();
    toggle.set_active(active);
    toggle.set_valign(gtk::Align::Center);
    row.append(&toggle);
    (row, toggle)
}
fn scale_row(
    title: &str,
    subtitle: &str,
    min: f64,
    max: f64,
    value: f64,
    suffix: &str,
) -> (gtk::Box, gtk::Scale) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    row.add_css_class("data-row");
    let text = section_title(title, subtitle);
    text.set_hexpand(true);
    row.append(&text);
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max, 5.0);
    scale.set_size_request(260, -1);
    scale.set_value(value);
    scale.set_draw_value(true);
    scale.set_value_pos(gtk::PositionType::Right);
    let suffix = suffix.to_string();
    scale.set_format_value_func(move |_, v| format!("{v:.0}{suffix}"));
    row.append(&scale);
    (row, scale)
}
