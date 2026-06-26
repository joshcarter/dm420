//! The radio + audio settings form shown in the unlocked Digital panel body.

use eframe::egui;

use app_core::{LineProfile, Protocol, SerialConfig};
use types::{Band, ContestProfile, HF_BANDS};

use crate::bus_view::BusView;
use crate::settings::{DEFAULT_BAUD, HardwareConfig, KENWOOD_BAUDS};
use crate::theme::Palette;

/// Editable radio + audio settings shown in the unlocked FT8 panel body. Seeded
/// from the currently-applied config when the panel is unlocked; the edits are
/// committed when the GUI is re-locked (see the locked branch in `Panel::ui`),
/// which pushes them to the live producers via [`BusView::apply_config`].
pub(super) struct ConfigForm {
    /// Whether the fields have been seeded from the applied config yet. Cleared by
    /// the panel on unlock so the form re-syncs to the applied config.
    pub(super) loaded: bool,
    audio_input: Option<String>,
    /// TX audio output device (the rig's data-in); `None` = system default.
    audio_output: Option<String>,
    port: Option<String>,
    baud: u32,
    profile: LineProfile,
    autodetect: bool,
    protocol: Protocol,
    /// The operator's active bands, edited via the BANDS checkbox grid. Seeded from
    /// the live selection on load; committed to `[bands] list` on re-lock.
    active_bands: Vec<Band>,
    /// Cached device/port lists for the pickers (refreshed on load / Refresh).
    audio_devices: Vec<String>,
    audio_output_devices: Vec<String>,
    serial_ports: Vec<String>,
}

impl Default for ConfigForm {
    fn default() -> Self {
        Self {
            loaded: false,
            audio_input: None,
            audio_output: None,
            port: None,
            baud: DEFAULT_BAUD,
            profile: LineProfile::Default,
            autodetect: true,
            protocol: Protocol::Ft8,
            active_bands: Vec::new(),
            audio_devices: Vec::new(),
            audio_output_devices: Vec::new(),
            serial_ports: Vec::new(),
        }
    }
}

impl ConfigForm {
    /// Seed the editable fields from the currently-applied config and refresh the
    /// device/port lists.
    fn load(&mut self, bus: &BusView) {
        let cfg = bus.current_config();
        self.audio_input = cfg.audio_input;
        self.audio_output = cfg.audio_output;
        self.port = cfg.serial.port;
        self.baud = cfg.serial.baud;
        self.profile = cfg.serial.profile;
        self.autodetect = cfg.serial.autodetect;
        self.protocol = cfg.protocol;
        self.active_bands = bus.active_bands();
        self.audio_devices = bus.audio_inputs();
        self.audio_output_devices = bus.audio_outputs();
        self.serial_ports = bus.serial_ports();
        self.loaded = true;
    }

    /// The edited fields as a `HardwareConfig` ready to apply.
    pub(super) fn to_config(&self) -> HardwareConfig {
        // Capture the chosen device's stable USB identity (vid/pid/serial) so a
        // later replug — which renumbers the `/dev/cu.usbserial-*` path — still
        // resolves to the same radio. The path is kept as a fallback hint.
        let (usb_vid, usb_pid, usb_serial) = match self.port.as_deref() {
            Some(p) if !p.is_empty() => app_core::usb_identity_for_port(p),
            _ => (None, None, None),
        };
        HardwareConfig {
            audio_input: self.audio_input.clone(),
            audio_output: self.audio_output.clone(),
            serial: SerialConfig {
                port: self.port.clone(),
                usb_serial,
                usb_vid,
                usb_pid,
                baud: self.baud,
                profile: self.profile,
                autodetect: self.autodetect,
            },
            protocol: self.protocol,
        }
    }

    /// The edited active-band selection, in canonical (longest-wavelength-first)
    /// order. Committed to `[bands] list` and pushed to the bus on re-lock.
    pub(super) fn active_bands(&self) -> Vec<Band> {
        HF_BANDS
            .iter()
            .copied()
            .filter(|b| self.active_bands.contains(b))
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        bus: &BusView,
        pal: &Palette,
        wide: &mut bool,
        auto_hop: &mut bool,
        contest: &mut ContestProfile,
        fd_class: &mut String,
        fd_section: &mut String,
    ) {
        if !self.loaded {
            self.load(bus);
        }
        ui.spacing_mut().item_spacing = egui::vec2(10.0, 8.0);
        ui.label(
            egui::RichText::new("RADIO SETUP")
                .color(pal.legend)
                .strong(),
        );

        // Audio device + decode mode are pushed to the live capture producer; in
        // WAV replay (or rig-only) there is none, so they're fixed at startup and
        // shown read-only rather than letting the operator edit dead controls.
        let live_audio = bus.has_live_audio();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("radio_setup_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Audio input");
                        let sel = self
                            .audio_input
                            .clone()
                            .unwrap_or_else(|| "(system default)".into());
                        ui.add_enabled_ui(live_audio, |ui| {
                            egui::ComboBox::from_id_salt("audio_input")
                                .selected_text(sel)
                                .width(240.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.audio_input,
                                        None,
                                        "(system default)",
                                    );
                                    for d in &self.audio_devices {
                                        ui.selectable_value(
                                            &mut self.audio_input,
                                            Some(d.clone()),
                                            d,
                                        );
                                    }
                                });
                        });
                        ui.end_row();

                        // TX audio output (the rig's data-in). Independent of
                        // capture, so it's always selectable in real mode.
                        ui.label("Audio output");
                        let out_sel = self
                            .audio_output
                            .clone()
                            .unwrap_or_else(|| "(system default)".into());
                        egui::ComboBox::from_id_salt("audio_output")
                            .selected_text(out_sel)
                            .width(240.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.audio_output,
                                    None,
                                    "(system default)",
                                );
                                for d in &self.audio_output_devices {
                                    ui.selectable_value(
                                        &mut self.audio_output,
                                        Some(d.clone()),
                                        d,
                                    );
                                }
                            });
                        ui.end_row();

                        ui.label("Mode");
                        ui.add_enabled_ui(live_audio, |ui| {
                            egui::ComboBox::from_id_salt("mode")
                                .selected_text(proto_label(self.protocol))
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut self.protocol, Protocol::Ft8, "FT8");
                                    ui.selectable_value(&mut self.protocol, Protocol::Ft4, "FT4");
                                });
                        });
                        ui.end_row();

                        if !live_audio {
                            ui.label("");
                            ui.label(
                                egui::RichText::new("WAV replay — set at startup")
                                    .color(pal.sub)
                                    .italics(),
                            );
                            ui.end_row();
                        }

                        ui.label("Rig port");
                        ui.checkbox(&mut self.autodetect, "Autodetect port / baud");
                        ui.end_row();
                    });

                // Manual serial fields are disabled (greyed) while autodetect is on.
                ui.add_enabled_ui(!self.autodetect, |ui| {
                    egui::Grid::new("serial_grid")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label("Port");
                            let sel = self.port.clone().unwrap_or_else(|| {
                                if self.serial_ports.is_empty() {
                                    "(no ports found)".into()
                                } else {
                                    "(select port)".into()
                                }
                            });
                            egui::ComboBox::from_id_salt("port")
                                .selected_text(sel)
                                .width(240.0)
                                .show_ui(ui, |ui| {
                                    for p in &self.serial_ports {
                                        ui.selectable_value(&mut self.port, Some(p.clone()), p);
                                    }
                                });
                            ui.end_row();

                            ui.label("Baud");
                            egui::ComboBox::from_id_salt("baud")
                                .selected_text(self.baud.to_string())
                                .show_ui(ui, |ui| {
                                    for &b in KENWOOD_BAUDS {
                                        ui.selectable_value(&mut self.baud, b, b.to_string());
                                    }
                                });
                            ui.end_row();

                            ui.label("Flow");
                            egui::ComboBox::from_id_salt("flow")
                                .selected_text(profile_label(self.profile))
                                .show_ui(ui, |ui| {
                                    for p in [
                                        LineProfile::Default,
                                        LineProfile::AssertDtrRts,
                                        LineProfile::HardwareFlow,
                                    ] {
                                        ui.selectable_value(&mut self.profile, p, profile_label(p));
                                    }
                                });
                            ui.end_row();
                        });
                });

                ui.add_space(6.0);
                if ui.button("Refresh devices").clicked() {
                    self.audio_devices = bus.audio_inputs();
                    self.audio_output_devices = bus.audio_outputs();
                    self.serial_ports = bus.serial_ports();
                }
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("Changes take effect when you lock the GUI.")
                        .color(pal.sub)
                        .italics(),
                );

                // Active bands: which HF bands the radio/antenna can use. Like the
                // hardware settings above, the selection commits on re-lock (persisted
                // to [bands] list); the scanner, Band Status, and Contacts map then
                // show only these bands.
                ui.add_space(10.0);
                ui.separator();
                ui.label(egui::RichText::new("BANDS").color(pal.legend).strong());
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(
                        "Check the bands your radio/antenna can use — only these appear \
                         in the scanner, Band Status, and map.",
                    )
                    .color(pal.sub)
                    .italics(),
                );
                ui.add_space(4.0);
                egui::Grid::new("active_bands_grid")
                    .num_columns(3)
                    .spacing([18.0, 6.0])
                    .show(ui, |ui| {
                        for (i, &band) in HF_BANDS.iter().enumerate() {
                            let mut on = self.active_bands.contains(&band);
                            if ui.checkbox(&mut on, crate::format::band_label(band)).changed() {
                                if on {
                                    self.active_bands.push(band);
                                } else {
                                    self.active_bands.retain(|b| *b != band);
                                }
                            }
                            // 3 columns → a row break after every third band (2 rows).
                            if i % 3 == 2 {
                                ui.end_row();
                            }
                        }
                    });

                // Contest: the active exchange profile. Editing it here edits the
                // station identity directly (like call/grid in the top bar); the
                // change is persisted to [station] and pushed to the QSO engine when
                // the GUI re-locks. ARRL Field Day swaps the engine into the FD flow
                // (CQ FD … + the <class> <section> exchange), so the class/section
                // fields only appear — and are only sent — for that profile.
                ui.add_space(10.0);
                ui.separator();
                ui.label(egui::RichText::new("CONTEST").color(pal.legend).strong());
                ui.add_space(4.0);
                egui::Grid::new("contest_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Contest");
                        egui::ComboBox::from_id_salt("contest")
                            .selected_text(contest_label(*contest))
                            .width(180.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    contest,
                                    ContestProfile::Standard,
                                    contest_label(ContestProfile::Standard),
                                );
                                ui.selectable_value(
                                    contest,
                                    ContestProfile::ArrlFieldDay,
                                    contest_label(ContestProfile::ArrlFieldDay),
                                );
                            });
                        ui.end_row();

                        if *contest == ContestProfile::ArrlFieldDay {
                            // Class + section are kept upper-case to match the on-air
                            // exchange convention, the same as the call/grid fields.
                            ui.label("Class");
                            if ui
                                .add(
                                    egui::TextEdit::singleline(fd_class)
                                        .char_limit(4)
                                        .desired_width(80.0)
                                        .hint_text("3A"),
                                )
                                .changed()
                            {
                                *fd_class = fd_class.to_uppercase();
                            }
                            ui.end_row();

                            ui.label("Section");
                            if ui
                                .add(
                                    egui::TextEdit::singleline(fd_section)
                                        .char_limit(4)
                                        .desired_width(80.0)
                                        .hint_text("CO"),
                                )
                                .changed()
                            {
                                *fd_section = fd_section.to_uppercase();
                            }
                            ui.end_row();
                        }
                    });
                if *contest == ContestProfile::ArrlFieldDay {
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new(
                            "Class = transmitters + power category (e.g. 3A); section = \
                             your ARRL/RAC section (e.g. CO). Sent in place of the grid.",
                        )
                        .color(pal.sub)
                        .italics(),
                    );
                }

                // Display preferences apply immediately and persist on their own (not
                // tied to the lock-to-apply hardware flow), so the change is in effect
                // the moment you re-lock and see the waterslide again.
                ui.add_space(10.0);
                ui.separator();
                ui.label(egui::RichText::new("DISPLAY").color(pal.legend).strong());
                ui.add_space(4.0);
                ui.label("Waterslide split");
                ui.horizontal(|ui| {
                    if ui.radio(!*wide, "1:1  (centered)").clicked() && *wide {
                        *wide = false;
                        crate::settings::save_waterslide_wide(false);
                    }
                    if ui.radio(*wide, "2:1  (wider decode)").clicked() && !*wide {
                        *wide = true;
                        crate::settings::save_waterslide_wide(true);
                    }
                });
                ui.label(
                    egui::RichText::new(
                        "2:1 gives decoded text 2/3 of the panel; both sides span the same time.",
                    )
                    .color(pal.sub)
                    .italics(),
                );

                ui.add_space(8.0);
                ui.separator();
                ui.label(egui::RichText::new("OPERATING").color(pal.legend).strong());
                ui.add_space(4.0);
                let prev_hop = *auto_hop;
                ui.checkbox(auto_hop, "Auto QSY — hop to clearest lane after 3 unanswered CQs");
                if *auto_hop != prev_hop {
                    bus.set_auto_hop(*auto_hop);
                }
            });
    }
}

fn contest_label(c: ContestProfile) -> &'static str {
    match c {
        ContestProfile::Standard => "None",
        ContestProfile::ArrlFieldDay => "ARRL Field Day",
    }
}

fn proto_label(p: Protocol) -> &'static str {
    match p {
        Protocol::Ft8 => "FT8",
        Protocol::Ft4 => "FT4",
    }
}

fn profile_label(p: LineProfile) -> &'static str {
    match p {
        LineProfile::Default => "None (default)",
        LineProfile::AssertDtrRts => "DTR/RTS",
        LineProfile::HardwareFlow => "RTS/CTS (hardware)",
    }
}
