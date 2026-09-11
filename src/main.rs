mod desktop;
mod power;

use adw::prelude::*;
use gtk::{gdk, glib};
use power::{BatteryInfo, Settings};
use std::{cell::RefCell, rc::Rc};

const APP_ID: &str = "org.raven.Power";

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
    let app = adw::Application::builder().application_id(APP_ID).build();
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
    let mut css = format!(
        "@define-color accent_bg_color {accent};\n@define-color accent_color {accent};\n"
    );
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
    let brand_icon = gtk::Image::from_icon_name("battery-good-symbolic");
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
        ("utilities-system-monitor-symbolic", "Energy usage", "orange"),
        ("power-profile-balanced-symbolic", "Power profiles", "blue"),
        ("application-x-executable-symbolic", "Applications", "purple"),
        ("battery-good-symbolic", "Battery health", "red"),
    ];
    for (icon, label, tint) in pages {
        navigation.append(&nav_row(icon, label, tint));
    }
    sidebar.append(&navigation);

    let battery = BatteryInfo::read();
    let sidebar_status = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    sidebar_status.add_css_class("raven-card");
    sidebar_status.add_css_class("status-card");
    sidebar_status.append(&gtk::Image::from_icon_name("battery-good-symbolic"));
    let sidebar_status_text = gtk::Box::new(gtk::Orientation::Vertical, 1);
    let status_title = gtk::Label::new(Some(&format!("{}% remaining", battery.percent)));
    status_title.set_xalign(0.0);
    status_title.add_css_class("card-title");
    sidebar_status_text.append(&status_title);
    let status_subtitle = gtk::Label::new(Some(&format!(
        "{} · {}",
        battery.status,
        battery.remaining_text()
    )));
    status_subtitle.set_xalign(0.0);
    status_subtitle.add_css_class("dim");
    sidebar_status_text.append(&status_subtitle);
    sidebar_status.append(&sidebar_status_text);
    sidebar.append(&sidebar_status);

    let stack = gtk::Stack::builder()
        .hexpand(true)
        .vexpand(true)
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    stack.add_named(
        &overview_page(settings.clone(), &toast_overlay),
        Some("overview"),
    );
    stack.add_named(&usage_page(), Some("usage"));
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
) -> gtk::ScrolledWindow {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 18);
    page.add_css_class("page");
    let battery = BatteryInfo::read();
    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 28);
    hero.add_css_class("card");
    hero.add_css_class("hero");
    let gauge = gtk::Box::new(gtk::Orientation::Vertical, 0);
    gauge.add_css_class("battery-gauge");
    let percent = gtk::Label::new(Some(&format!("{}%", battery.percent)));
    percent.add_css_class("battery-percent");
    gauge.append(&percent);
    gauge.append(&gtk::Label::new(Some("remaining")));
    hero.append(&gauge);
    let summary = gtk::Box::new(gtk::Orientation::Vertical, 6);
    summary.set_hexpand(true);
    let live = gtk::Label::new(Some(if battery.is_real {
        "●  LIVE SYSTEM ESTIMATE"
    } else {
        "●  DEMO DATA — NO BATTERY FOUND"
    }));
    live.set_xalign(0.0);
    live.add_css_class("eyebrow");
    summary.append(&live);
    let estimate = gtk::Label::new(Some(&format!("{} left", battery.remaining_text())));
    estimate.set_xalign(0.0);
    estimate.add_css_class("hero-title");
    summary.append(&estimate);
    let status = gtk::Label::new(Some(&format!(
        "{} · {:.1} W current draw · {} battery health",
        battery.status,
        battery.power_watts,
        battery.health_percent()
    )));
    status.set_xalign(0.0);
    status.add_css_class("dim-label");
    summary.append(&status);
    let stats = gtk::Box::new(gtk::Orientation::Horizontal, 36);
    stats.set_margin_top(18);
    stats.append(&stat(
        "Current draw",
        &format!("{:.1} W", battery.power_watts),
    ));
    stats.append(&stat(
        "Battery health",
        &format!("{}% · Excellent", battery.health_percent()),
    ));
    stats.append(&stat(
        "Temperature",
        &battery
            .temperature_c
            .map(|v| format!("{v:.0}°C · Normal"))
            .unwrap_or_else(|| "Unavailable".into()),
    ));
    summary.append(&stats);
    hero.append(&summary);
    page.append(&hero);

    if battery.power_watts > 12.0 || !battery.is_real {
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
        page.append(&alert);
    }
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

fn usage_page() -> gtk::ScrolledWindow {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 16);
    page.add_css_class("page");
    let battery = BatteryInfo::read();
    let metrics = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    metrics.set_homogeneous(true);
    metrics.append(&metric(
        "Discharge rate",
        &format!("{:.1} W", battery.power_watts),
        "Live from the battery controller",
    ));
    metrics.append(&metric(
        "Battery remaining",
        &format!("{}%", battery.percent),
        &battery.remaining_text(),
    ));
    metrics.append(&metric(
        "Full capacity",
        &format!("{:.1} Wh", battery.energy_full_wh),
        "Measured maximum charge",
    ));
    metrics.append(&metric(
        "Battery status",
        &battery.status,
        &battery.battery_name,
    ));
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
    let b = BatteryInfo::read();
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
fn stat(label: &str, value: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let l = gtk::Label::new(Some(label));
    l.set_xalign(0.0);
    l.add_css_class("dim-label");
    b.append(&l);
    let v = gtk::Label::new(Some(value));
    v.set_xalign(0.0);
    v.add_css_class("stat-value");
    b.append(&v);
    b
}
fn metric(label: &str, value: &str, detail: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 5);
    b.add_css_class("metric-card");
    b.append(&gtk::Label::new(Some(label)));
    let v = gtk::Label::new(Some(value));
    v.add_css_class("metric-value");
    b.append(&v);
    let d = gtk::Label::new(Some(detail));
    d.add_css_class("dim-label");
    d.set_wrap(true);
    b.append(&d);
    b
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
