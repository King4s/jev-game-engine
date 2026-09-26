use eframe::egui::{self, Color32, RichText, Stroke, Vec2};
use jev_game_engine::{
    engine::EngineHandle,
    model::{Command, Event, FixtureWaypoint, Mode, Observation, Settings, View},
    origin::event_origin,
};
use std::time::{Duration, Instant};

const ACCENT: Color32 = Color32::from_rgb(100, 222, 183);
const MUTED: Color32 = Color32::from_rgb(167, 180, 193);
const WARNING: Color32 = Color32::from_rgb(255, 191, 112);

pub struct GameApp {
    engine: EngineHandle,
    settings: Settings,
    replay_path: String,
    selected: Option<u64>,
    event_page: usize,
    screenshot_path: Option<String>,
    screenshot_after_goal: Option<String>,
    verdict_step_sent: bool,
    details_open: bool,
    verdict_frames: u32,
    tall_window: bool,
    screenshot_requested: bool,
    screenshot_status: Option<String>,
    opened_at: Instant,
    frame_count: u64,
}

impl GameApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        settings: Settings,
        auto_connect: bool,
        screenshot_path: Option<String>,
        screenshot_after_goal: Option<String>,
    ) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let engine = EngineHandle::new();
        if auto_connect {
            engine.send(Command::Connect(settings.clone()));
        }
        Self {
            engine,
            settings,
            replay_path: String::new(),
            selected: None,
            event_page: 0,
            screenshot_path,
            screenshot_after_goal,
            verdict_step_sent: false,
            details_open: false,
            verdict_frames: 0,
            tall_window: false,
            screenshot_requested: false,
            screenshot_status: None,
            opened_at: Instant::now(),
            frame_count: 0,
        }
    }

    fn capture_frame(&mut self, ctx: &egui::Context, view: &View) {
        self.frame_count += 1;
        let after_goal = self.screenshot_after_goal.is_some();
        if self.screenshot_path.is_none() && !after_goal {
            return;
        }
        // One offline decision is enough to produce a verdict, and the fixture answers it
        // without a provider call. Live mode is never stepped from here: the operator starts the
        // agent, and this gate only waits for the verdict that run records.
        if after_goal
            && !self.verdict_step_sent
            && self.settings.mode == Mode::Demo
            && view.observation.is_some()
        {
            self.engine.send(Command::Step);
            self.verdict_step_sent = true;
        }
        let verdict_seen = view.events.iter().any(|event| event.arrival.is_some());
        if after_goal && verdict_seen {
            // Put the verdict itself on screen: select the event that carries it and open the
            // detail panel, so the captured image contains the measured numbers instead of only
            // the executor row.
            self.selected = view
                .events
                .iter()
                .rev()
                .find(|event| event.arrival.is_some())
                .map(|event| event.sequence);
            self.details_open = true;
            if !self.tall_window {
                // The verdict panel sits under the decision trace and would otherwise fall
                // below the window edge, so the capture would miss the measured numbers.
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1100.0, 1150.0)));
                self.tall_window = true;
            }
            // This runs after the frame has been laid out, so the request must wait until the
            // resized window and the opened panel have actually been drawn; otherwise the
            // capture shows an earlier frame.
            self.verdict_frames = self.verdict_frames.saturating_add(1);
        }
        let bound = if after_goal {
            Duration::from_secs(30)
        } else {
            Duration::from_secs(20)
        };
        let ready = if after_goal {
            (verdict_seen && self.verdict_frames >= 8) || self.opened_at.elapsed() >= bound
        } else {
            self.settings.mode != Mode::Live
                || view.observation.is_some()
                || view.last_error.is_some()
                || self.opened_at.elapsed() >= bound
        };
        let gate = if after_goal {
            if verdict_seen {
                "after an arrival verdict"
            } else {
                "at the 30 s bound without an arrival verdict"
            }
        } else {
            "at the first observation"
        };
        let capture = ctx.input(|input| {
            input.events.iter().find_map(|event| {
                if let egui::Event::Screenshot { image, .. } = event {
                    Some(image.clone())
                } else {
                    None
                }
            })
        });
        if let Some(capture) = capture {
            let path = if after_goal {
                self.screenshot_after_goal.take()
            } else {
                self.screenshot_path.take()
            };
            if let Some(path) = path {
                let result = (|| -> Result<(), Box<dyn std::error::Error>> {
                    let target = std::path::Path::new(&path);
                    if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
                        std::fs::create_dir_all(parent)?;
                    }
                    let bytes: Vec<u8> = capture
                        .pixels
                        .iter()
                        .flat_map(|pixel| pixel.to_array())
                        .collect();
                    image::save_buffer_with_format(
                        target,
                        &bytes,
                        capture.size[0] as u32,
                        capture.size[1] as u32,
                        image::ColorType::Rgba8,
                        image::ImageFormat::Png,
                    )?;
                    Ok(())
                })();
                self.screenshot_status = Some(match result {
                    Ok(()) => format!("UI screenshot saved {gate}: {path}"),
                    Err(error) => format!("Could not save UI screenshot: {error}"),
                });
                if let Some(status) = &self.screenshot_status {
                    eprintln!("{status}");
                }
            }
        } else if !self.screenshot_requested
            && self.frame_count >= 5
            && self.opened_at.elapsed() >= Duration::from_millis(500)
            && ready
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            self.screenshot_requested = true;
        }
    }
    fn configuration(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Connection and run limits").default_open(true).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut self.settings.mode, Mode::Demo, "Offline fixture");
                ui.selectable_value(&mut self.settings.mode, Mode::Live, "Minecraft · Live Jev");
                ui.separator();
                ui.label("127.0.0.1 :");
                ui.add(egui::DragValue::new(&mut self.settings.port).range(1..=65535));
                ui.label("Bot name");
                ui.add(egui::TextEdit::singleline(&mut self.settings.bot_name).desired_width(100.0));
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Max. model requests");
                ui.add(egui::DragValue::new(&mut self.settings.max_requests).range(1..=100_000));
                ui.label("Max. seconds");
                ui.add(egui::DragValue::new(&mut self.settings.max_seconds).range(1..=604_800));
                ui.label("Pacing (s)");
                let mut pacing = self.settings.request_interval_ms / 1_000;
                if ui
                    .add(egui::DragValue::new(&mut pacing).range(0..=86_400))
                    .changed()
                {
                    self.settings.request_interval_ms = pacing * 1_000;
                }
                if ui.button("Connect with these settings").clicked() {
                    self.selected = None;
                    self.engine.send(Command::Connect(self.settings.clone()));
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Session objective");
                ui.add(
                    egui::TextEdit::singleline(&mut self.settings.objective)
                        .desired_width(420.0)
                        .hint_text("e.g. Survive the night and keep health"),
                );
                ui.checkbox(
                    &mut self.settings.safety_reflex,
                    "Engine safety reflex (engine-initiated flight goals)",
                );
            });
            ui.checkbox(&mut self.settings.legacy_forwarding, "Legacy forwarding for an authorized bot server");
            let mut reachable = self.settings.fixture_waypoint == FixtureWaypoint::Reachable;
            if ui
                .checkbox(
                    &mut reachable,
                    "Reachable fixture waypoint (offline demo arrives instead of expiring)",
                )
                .changed()
            {
                self.settings.fixture_waypoint = if reachable {
                    FixtureWaypoint::Reachable
                } else {
                    FixtureWaypoint::Distant
                };
            }
            ui.small("Live uses a separate bot player on a compatible local server. Settings apply when connecting.");
        });
    }

    fn controls(&mut self, ui: &mut egui::Ui, view: &View) {
        ui.horizontal_wrapped(|ui| {
            let available = !view.replay && view.observation.as_ref().is_some_and(|o| o.connected);
            if ui
                .add_enabled(
                    available,
                    egui::Button::new("Start agent").fill(Color32::from_rgb(30, 90, 72)),
                )
                .clicked()
            {
                self.engine.send(Command::Start);
            }
            if ui
                .add_enabled(!view.replay, egui::Button::new("Pause agent"))
                .clicked()
            {
                self.engine.send(Command::Pause);
            }
            if ui
                .add_enabled(available, egui::Button::new("One decision"))
                .clicked()
            {
                self.engine.send(Command::Step);
            }
            if ui.button(RichText::new("Stop").color(WARNING)).clicked() {
                self.engine.send(Command::Stop);
            }
            if ui.button("Reset session").clicked() {
                self.selected = None;
                self.engine.send(Command::Reset);
            }
            if ui
                .add_enabled(!view.events.is_empty(), egui::Button::new("Export"))
                .clicked()
            {
                self.engine.send(Command::Export);
            }
        });
        ui.small("Pause stops the agent; the Minecraft world continues. Reset session does not change the world.");
        egui::CollapsingHeader::new("Manual bot takeover").show(ui, |ui| {
            ui.small("A manual goal interrupts Jev control and is recorded as manual input. The adapter revalidates the goal.");
            if !view.manual_candidates.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for candidate in &view.manual_candidates {
                        if ui.add_enabled(!view.replay && view.observation.as_ref().is_some_and(|o| o.connected), egui::Button::new(&candidate.description)).clicked() && let Some(command) = manual_command(view, candidate) {
                            self.engine.send(command);
                        }
                    }
                });
            } else { ui.label("No observed goal candidates yet."); }
        });
    }

    fn overview(&self, ui: &mut egui::Ui, view: &View) {
        ui.heading("Observed world");
        if let Some(obs) = &view.observation {
            ui.label(format!(
                "Dimension: {}",
                obs.dimension
                    .as_deref()
                    .filter(|name| !name.is_empty())
                    .unwrap_or("Unknown")
            ));
            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "X {:.1}   Y {:.1}   Z {:.1}",
                    obs.position.x, obs.position.y, obs.position.z
                ));
                ui.label(format!("Health {:.0}  ·  Food {:.0}", obs.health, obs.food));
                ui.label(if obs.connected {
                    "Connected"
                } else {
                    "Disconnected · last observation"
                });
            });
            draw_map(ui, obs);
            ui.small(&obs.note);
            ui.label(format!(
                "Observation #{} · {} blocks · {} entities",
                obs.sequence,
                obs.blocks.len(),
                obs.entities.len()
            ));
            egui::CollapsingHeader::new("Inventory").show(ui, |ui| {
                if obs.inventory.is_empty() {
                    ui.label("No items reported.");
                }
                for item in &obs.inventory {
                    ui.label(item);
                }
            });
        } else {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_min_height(180.0);
                ui.label(RichText::new("Waiting for the first observation").size(20.0));
                ui.label("Choose a data source and connect. Position and map appear when the adapter provides data.");
            });
        }
    }

    fn decision(&self, ui: &mut egui::Ui, view: &View) {
        ui.heading("Agent goal");
        ui.label(
            RichText::new(view.active_goal.as_deref().unwrap_or("No active goal"))
                .color(ACCENT)
                .size(18.0),
        );
        ui.small("Jev selects the goal. The Rust executor handles local movement and stopping.");
        ui.small(format!(
            "Session objective: {}",
            if view.objective.is_empty() {
                "none"
            } else {
                view.objective.as_str()
            }
        ));
        if view.reflexes > 0 {
            ui.small(format!(
                "Engine safety-reflex actions this session: {}",
                view.reflexes
            ));
        }
        ui.add_space(8.0);
        let ms = |value: Option<u64>| {
            value
                .map(|n| format!("{n} ms"))
                .unwrap_or_else(|| "Unknown".into())
        };
        egui::Grid::new("metrics")
            .spacing([20.0, 8.0])
            .show(ui, |ui| {
                for (label, value) in [
                    ("Model requests", view.requests.to_string()),
                    ("Latency p50", ms(view.p50_ms)),
                    ("Latency p95", ms(view.p95_ms)),
                    ("Goal horizon", format!("{} ms", view.goal_ms)),
                    (
                        "Response freshness limit",
                        format!("{} ms", view.answer_age_limit_ms),
                    ),
                ] {
                    ui.label(RichText::new(label).color(MUTED));
                    ui.label(value);
                    ui.end_row();
                }
            });
        ui.separator();
        if let Some(event) = view.events.iter().rev().find(|e| e.decision.is_some()) {
            let decision = event.decision.as_ref().unwrap();
            ui.label(format!("Latest choice: {}", decision.choice));
            ui.small(format!(
                "Model: {} · {} ms",
                decision.model, decision.latency_ms
            ));
            if let Some(confidence) = decision.confidence {
                ui.label(format!("Confidence: {confidence:.3}"));
            } else {
                ui.label("Confidence: unknown");
            }
            if decision.probabilities.is_empty() {
                ui.small("No probabilities returned.");
            }
            for (choice, probability) in &decision.probabilities {
                ui.add(
                    egui::ProgressBar::new((*probability as f32).clamp(0.0, 1.0))
                        .text(format!("{choice}   {:.1}%", probability * 100.0)),
                );
            }
            ui.small("Model distribution; not the probability of winning.");
        } else {
            ui.label("No model decision received.");
        }
    }

    fn timeline(&mut self, ui: &mut egui::Ui, view: &View) {
        ui.heading("Decision trace");
        let page_count = view.events.len().div_ceil(100).max(1);
        self.event_page = self.event_page.min(page_count - 1);
        ui.horizontal_wrapped(|ui| {
            if ui
                .selectable_label(self.selected.is_none(), "Follow latest")
                .clicked()
            {
                self.selected = None;
            }
            ui.small(format!("{} events", view.events.len()));
            if ui
                .add_enabled(self.event_page > 0, egui::Button::new("Newer events"))
                .clicked()
            {
                self.event_page -= 1;
            }
            ui.label(format!("Page {} / {}", self.event_page + 1, page_count));
            if ui
                .add_enabled(
                    self.event_page + 1 < page_count,
                    egui::Button::new("Older events"),
                )
                .clicked()
            {
                self.event_page += 1;
            }
        });
        egui::ScrollArea::vertical()
            .id_salt("event_list")
            .max_height(140.0)
            .show(ui, |ui| {
                if view.events.is_empty() {
                    ui.label(
                        "Observations, model choices and execution appear here when a session starts.",
                    );
                }
                for event in view.events.iter().rev().skip(self.event_page * 100).take(100) {
                    let origin = match event_origin(event) {
                        Some(origin) => format!("{}   ", origin.label()),
                        None => String::new(),
                    };
                    let title = format!(
                        "#{:04}   {:>6.1}s   {}{}   {}",
                        event.sequence,
                        event.elapsed_ms as f64 / 1000.0,
                        origin,
                        event.kind,
                        event.message
                    );
                    if ui
                        .selectable_label(self.selected == Some(event.sequence), title)
                        .clicked()
                    {
                        self.selected = Some(event.sequence);
                    }
                }
            });
        let selected = self
            .selected
            .and_then(|id| view.events.iter().find(|e| e.sequence == id))
            .or_else(|| view.events.last());
        if let Some(event) = selected {
            let title = if self.selected.is_some() {
                "Historical event · details"
            } else {
                "Latest event · details"
            };
            if self.details_open {
                // Only the screenshot gate sets this, so the captured image contains the
                // verdict and its measured numbers instead of a collapsed header.
                ui.label(title);
                event_detail(ui, event);
            } else {
                egui::CollapsingHeader::new(title).show(ui, |ui| event_detail(ui, event));
            }
        }
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.label("Recording file");
            ui.add(
                egui::TextEdit::singleline(&mut self.replay_path)
                    .desired_width(340.0)
                    .hint_text("Path to exported JSON"),
            );
            if ui
                .add_enabled(
                    !self.replay_path.trim().is_empty(),
                    egui::Button::new("Open replay"),
                )
                .clicked()
            {
                self.selected = None;
                self.engine
                    .send(Command::Replay(self.replay_path.trim().to_owned()));
            }
        });
        if let Some(path) = &view.recording_path {
            ui.label(format!("Recording: {path}"));
        }
        ui.small("Replay shows recorded telemetry without a game connection or model requests. It does not recreate the world.");
    }
}

fn manual_command(view: &View, candidate: &jev_game_engine::model::Candidate) -> Option<Command> {
    if view.replay || !view.manual_candidates.contains(candidate) {
        return None;
    }
    let observation = view
        .observation
        .as_ref()
        .filter(|observation| observation.connected)?;
    Some(Command::Manual {
        candidate: candidate.clone(),
        world_epoch: observation.world_epoch,
        dimension: observation.dimension.clone(),
    })
}
fn draw_map(ui: &mut egui::Ui, observation: &Observation) {
    let size = Vec2::new(ui.available_width().max(120.0), 200.0);
    let (response, painter) = ui.allocate_painter(size, egui::Sense::hover());
    let rect = response.rect;
    painter.rect_filled(rect, 4.0, Color32::from_rgb(18, 27, 34));
    let scale = (rect.width().min(rect.height()) - 32.0) / 32.0;
    for offset in -4..=4 {
        let delta = offset as f32 * scale * 4.0;
        painter.line_segment(
            [
                rect.center() + Vec2::new(delta, -rect.height() / 2.0),
                rect.center() + Vec2::new(delta, rect.height() / 2.0),
            ],
            Stroke::new(1.0, Color32::from_rgb(35, 49, 59)),
        );
        painter.line_segment(
            [
                rect.center() + Vec2::new(-rect.width() / 2.0, delta),
                rect.center() + Vec2::new(rect.width() / 2.0, delta),
            ],
            Stroke::new(1.0, Color32::from_rgb(35, 49, 59)),
        );
    }
    for (items, color) in [
        (&observation.blocks, MUTED),
        (&observation.entities, WARNING),
    ] {
        for item in items {
            let point = rect.center()
                + Vec2::new(
                    (item.position.x - observation.position.x) as f32 * scale,
                    (item.position.z - observation.position.z) as f32 * scale,
                );
            if rect.shrink(8.0).contains(point) {
                painter.circle_filled(point, 4.0, color);
            }
        }
    }
    painter.circle_filled(rect.center(), 6.0, ACCENT);
    painter.text(
        rect.left_top() + Vec2::splat(8.0),
        egui::Align2::LEFT_TOP,
        "X/Z · bot at center · 4 blocks per cell",
        egui::FontId::proportional(12.0),
        MUTED,
    );
    ui.small("Green: bot · gray: observed blocks · amber: entities. Empty areas are unknown.");
}

fn event_detail(ui: &mut egui::Ui, event: &Event) {
    ui.label(format!(
        "#{} · {} · {}",
        event.sequence, event.kind, event.message
    ));
    if let Some(observation) = &event.observation {
        ui.label(format!(
            "Linked observation #{}: ({:.2}, {:.2}, {:.2})",
            observation.sequence,
            observation.position.x,
            observation.position.y,
            observation.position.z
        ));
    }
    for candidate in &event.candidates {
        ui.label(format!(
            "Candidate {}: {} · {} ms",
            candidate.id, candidate.description, candidate.duration_ms
        ));
    }
    if let Some(arrival) = &event.arrival {
        let arrived = arrival.arrived;
        let verdict = if arrived { "arrived" } else { "expired" };
        let measured = arrival
            .measured_distance_m
            .map(|distance| format!("{distance:.2} m"))
            .unwrap_or_else(|| "unknown".to_owned());
        let target = arrival
            .target
            .as_ref()
            .map(|target| format!("({:.2}, {:.2}, {:.2})", target.x, target.y, target.z))
            .unwrap_or_else(|| "no bound target".to_owned());
        let tolerance = format!("{:.2} m", arrival.tolerance_m);
        ui.label(format!(
            "Arrival: {verdict} · measured {measured} / tolerance {tolerance} · target {target} · elapsed {} ms / duration {} ms",
            arrival.elapsed_ms, arrival.duration_ms
        ));
    }
    egui::ScrollArea::vertical()
        .id_salt("event_json")
        .max_height(220.0)
        .show(ui, |ui| {
            if let Ok(json) = serde_json::to_string_pretty(event) {
                ui.monospace(json);
            }
        });
}

impl eframe::App for GameApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let view = self.engine.snapshot();
        self.capture_frame(ui.ctx(), &view);
        ui.ctx().request_repaint_after(Duration::from_millis(100));
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.heading(RichText::new("JEV / GAME ENGINE").color(ACCENT));
                    ui.label(RichText::new("Minecraft Java").color(MUTED));
                    ui.label(format!("Status: {}", view.status));
                });
                let source = if view.replay {
                    "REPLAY · recorded telemetry"
                } else if view.mode == Mode::Demo {
                    "Offline fixture — no Jev or game connection"
                } else {
                    "LIVE · separate Minecraft bot · Jev"
                };
                ui.colored_label(
                    if view.mode == Mode::Demo || view.replay {
                        WARNING
                    } else {
                        ACCENT
                    },
                    source,
                );
                if let Some(error) = &view.last_error {
                    ui.colored_label(WARNING, format!("Error: {error}"));
                }
                ui.separator();
                if let Some(status) = &self.screenshot_status {
                    ui.small(status);
                }
                self.configuration(ui);
                self.controls(ui, &view);
                ui.add_space(12.0);
                if ui.available_width() >= 860.0 {
                    ui.columns(2, |columns| {
                        self.overview(&mut columns[0], &view);
                        self.decision(&mut columns[1], &view);
                    });
                } else {
                    self.overview(ui, &view);
                    ui.add_space(12.0);
                    self.decision(ui, &view);
                }
                ui.add_space(12.0);
                self.timeline(ui, &view);
                ui.add_space(8.0);
                ui.small("Game library: automatic discovery is planned but not implemented yet.");
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jev_game_engine::model::{Candidate, Decision, Landmark, Position};
    use std::collections::BTreeMap;

    fn app() -> GameApp {
        GameApp {
            engine: EngineHandle::new(),
            settings: Settings::default(),
            replay_path: String::new(),
            selected: None,
            event_page: 0,
            screenshot_path: None,
            screenshot_after_goal: None,
            screenshot_requested: false,
            screenshot_status: None,
            opened_at: Instant::now(),
            frame_count: 0,
            verdict_step_sent: false,
            details_open: false,
            verdict_frames: 0,
            tall_window: false,
        }
    }

    fn connected_fixture() -> View {
        let observation = Observation {
            sequence: 7,
            world_epoch: 1,
            deaths: 0,
            dimension: Some("fixture:overworld".into()),
            connected: true,
            position: Position {
                x: 2.0,
                y: 64.0,
                z: -3.0,
            },
            health: 20.0,
            food: 18.0,
            inventory: vec!["stone x3".into()],
            blocks: vec![Landmark {
                name: "Observed stone".into(),
                position: Position {
                    x: 3.0,
                    y: 63.0,
                    z: -2.0,
                },
            }],
            entities: vec![],
            note: "Synthetic offline fixture; no Minecraft connection".into(),
        };
        View {
            status: "Connected".into(),
            observation: Some(observation.clone()),
            requests: 1,
            events: vec![Event {
                sequence: 1,
                elapsed_ms: 300,
                kind: "decision".into(),
                message: "Offline fixture decision".into(),
                observation: Some(observation),
                candidates: vec![Candidate {
                    id: "wait".into(),
                    description: "Wait at the observed position".into(),
                    target: None,
                    duration_ms: 2_000,
                }],
                decision: Some(Decision {
                    choice: "wait".into(),
                    probabilities: BTreeMap::new(),
                    confidence: None,
                    model: "offline-fixture".into(),
                    input_tokens: None,
                    output_tokens: None,
                    latency_ms: 300,
                }),
                arrival: None,
            }],
            ..View::default()
        }
    }

    fn text_from_shape(shape: &egui::epaint::Shape, output: &mut String) {
        match shape {
            egui::epaint::Shape::Text(text) => {
                output.push_str(&text.galley.job.text);
                output.push('\n');
            }
            egui::epaint::Shape::Vec(shapes) => {
                for shape in shapes {
                    text_from_shape(shape, output);
                }
            }
            _ => {}
        }
    }

    fn render(width: f32, mut paint: impl FnMut(&mut egui::Ui)) -> String {
        // Headless egui layout only: this is not an OS-window or GPU screenshot.
        let context = egui::Context::default();
        let mut text = String::new();
        for _ in 0..2 {
            let output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(width, 1_800.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| paint(ui));
                },
            );
            assert!(
                !output.shapes.is_empty(),
                "render produced no shapes at width {width}"
            );
            text.clear();
            for shape in &output.shapes {
                text_from_shape(&shape.shape, &mut text);
            }
            output.drop_without_applying_deltas();
        }
        text
    }

    #[test]
    fn connected_fixture_panels_render_at_narrow_and_wide_widths() {
        let mut application = app();
        let view = connected_fixture();
        for width in [600.0, 1_100.0] {
            let text = render(width, |ui| {
                application.configuration(ui);
                application.controls(ui, &view);
                if ui.available_width() >= 860.0 {
                    ui.columns(2, |columns| {
                        application.overview(&mut columns[0], &view);
                        application.decision(&mut columns[1], &view);
                    });
                } else {
                    application.overview(ui, &view);
                    application.decision(ui, &view);
                }
                application.timeline(ui, &view);
            });
            assert!(text.contains("Observed world"));
            assert!(text.contains("Decision trace"));
            assert!(text.contains("Synthetic offline fixture; no Minecraft connection"));
            assert!(text.contains("Confidence: unknown"));
            assert!(text.contains("No probabilities returned."));
        }
    }

    #[test]
    fn replay_with_selected_history_and_missing_metrics_renders() {
        let mut application = app();
        application.selected = Some(1);
        let mut view = connected_fixture();
        view.replay = true;
        view.status = "Replay".into();
        view.recording_path = Some("recordings/offline-example.json".into());
        let text = render(600.0, |ui| {
            application.controls(ui, &view);
            application.decision(ui, &view);
            application.timeline(ui, &view);
            event_detail(ui, &view.events[0]);
        });
        assert!(text.contains("Historical event"));
        assert!(text.contains("Unknown"));
        assert!(text.contains("offline-example.json"));
        assert!(text.contains("Linked observation #7"));
    }

    #[test]
    fn empty_and_error_views_render_without_synthetic_world_data() {
        let mut application = app();
        let view = View {
            status: "Connection failed".into(),
            last_error: Some("Local server unavailable".into()),
            ..View::default()
        };
        for width in [600.0, 1_100.0] {
            let text = render(width, |ui| {
                if let Some(error) = &view.last_error {
                    ui.colored_label(WARNING, format!("Error: {error}"));
                }
                application.controls(ui, &view);
                application.overview(ui, &view);
                application.decision(ui, &view);
                application.timeline(ui, &view);
            });
            assert!(text.contains("Error: Local server unavailable"));
            assert!(text.contains("Waiting for the first observation"));
            assert!(text.contains("No model decision received."));
            assert!(!text.contains("Health 20"));
        }
    }

    /// The timeline labels a safety-reflex action through the shared attribution rule, so
    /// an engine-initiated flight goal is never shown as the model's or the fixture's choice.
    #[test]
    fn timeline_labels_reflex_rows_safety_reflex_and_fixture_rows_fixture() {
        let mut application = app();
        let mut view = connected_fixture();
        let template = view.events[0].clone();
        view.events.push(Event {
            sequence: 2,
            elapsed_ms: 900,
            kind: "reflex".into(),
            message: "Engine safety action: Zombie is 3.0 blocks away; no goal in flight and fleeing via flee_0".into(),
            observation: None,
            candidates: vec![],
            decision: None,
            arrival: None,
        });
        view.events.push(Event {
            sequence: 3,
            elapsed_ms: 950,
            kind: "dispatched".into(),
            message: "SAFETY-REFLEX; engine-initiated bounded action; queued for adapter validation: flee_0".into(),
            observation: None,
            candidates: vec![],
            decision: None,
            arrival: None,
        });
        view.events.push(Event {
            sequence: 4,
            elapsed_ms: 1_000,
            kind: "action".into(),
            message: "SAFETY-REFLEX; engine-initiated bounded action; adapter accepted bounded action: flee_0".into(),
            ..template.clone()
        });
        view.events[3].decision = None;
        view.events[3].observation = None;
        let context = egui::Context::default();
        let mut rows: Vec<String> = Vec::new();
        for _ in 0..2 {
            let output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1100.0, 780.0),
                    )),
                    ..Default::default()
                },
                |ui| application.timeline(ui, &view),
            );
            rows.clear();
            for shape in &output.shapes {
                if let egui::epaint::Shape::Text(text) = &shape.shape
                    && text.galley.job.text.starts_with('#')
                {
                    rows.push(text.galley.job.text.clone());
                }
            }
            output.drop_without_applying_deltas();
        }
        let row = |sequence: &str| {
            rows.iter()
                .find(|row| row.starts_with(sequence))
                .unwrap_or_else(|| panic!("row {sequence} must render; rows: {rows:?}"))
                .clone()
        };
        assert!(
            row("#0001").contains("FIXTURE   decision"),
            "{}",
            row("#0001")
        );
        assert!(
            row("#0002").contains("SAFETY-REFLEX   reflex"),
            "{}",
            row("#0002")
        );
        assert!(
            row("#0003").contains("SAFETY-REFLEX   dispatched"),
            "{}",
            row("#0003")
        );
        assert!(
            row("#0004").contains("SAFETY-REFLEX   action"),
            "{}",
            row("#0004")
        );
        for sequence in ["#0002", "#0003", "#0004"] {
            let text = row(sequence);
            assert!(
                !text.contains("JEV-SELECTED")
                    && !text.contains("FIXTURE")
                    && !text.contains("MANUAL"),
                "{text}"
            );
        }
    }

    #[test]
    fn oldest_event_in_large_recording_is_visible_and_selectable() {
        let mut application = app();
        let mut view = connected_fixture();
        let template = view.events[0].clone();
        view.events = (1..=501)
            .map(|sequence| Event {
                sequence,
                message: format!("Recorded event {sequence}"),
                arrival: None,
                ..template.clone()
            })
            .collect();
        view.replay = true;
        application.event_page = 5;
        let context = egui::Context::default();
        let mut row_position = None;
        for _ in 0..2 {
            let output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1100.0, 780.0),
                    )),
                    ..Default::default()
                },
                |ui| application.timeline(ui, &view),
            );
            for shape in &output.shapes {
                if let egui::epaint::Shape::Text(text) = &shape.shape
                    && text.galley.job.text.contains("#0001")
                {
                    row_position = Some(text.pos + Vec2::new(8.0, 6.0));
                }
            }
            output.drop_without_applying_deltas();
        }
        let position = row_position.expect("oldest event must render on the final page");
        for pressed in [true, false] {
            let output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1100.0, 780.0),
                    )),
                    events: vec![
                        egui::Event::PointerMoved(position),
                        egui::Event::PointerButton {
                            pos: position,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::default(),
                        },
                    ],
                    ..Default::default()
                },
                |ui| application.timeline(ui, &view),
            );
            output.drop_without_applying_deltas();
        }
        assert_eq!(application.selected, Some(1));
    }

    #[test]
    fn manual_controls_use_refreshed_snapshot_not_historical_decision() {
        let mut view = connected_fixture();
        let historical = view.events[0].candidates[0].clone();
        let mut current = historical.clone();
        current.duration_ms = 8_000;
        current.description = "Current goal after latency adaptation".into();
        view.manual_candidates = vec![current.clone()];
        view.observation.as_mut().unwrap().world_epoch = 42;
        view.observation.as_mut().unwrap().dimension = Some("current:world".into());
        match manual_command(&view, &current).unwrap() {
            Command::Manual {
                candidate,
                world_epoch,
                dimension,
            } => {
                assert_eq!(candidate.duration_ms, 8_000);
                assert_eq!(candidate.target, current.target);
                assert_eq!(candidate.description, current.description);
                assert_eq!(world_epoch, 42);
                assert_eq!(dimension.as_deref(), Some("current:world"));
            }
            _ => panic!("expected a manual request"),
        }
        assert!(manual_command(&view, &historical).is_none());
        view.events.clear();
        assert!(
            manual_command(&view, &current).is_some(),
            "manual control must work without model history"
        );
        view.replay = true;
        assert!(manual_command(&view, &current).is_none());
        view.replay = false;
        view.manual_candidates.clear();
        assert!(
            manual_command(&view, &current).is_none(),
            "stale snapshot with cleared candidates must not execute"
        );
        view.manual_candidates.push(current.clone());
        view.observation = None;
        assert!(manual_command(&view, &current).is_none());
    }
}
