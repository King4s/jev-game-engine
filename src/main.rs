mod ui;

use jev_game_engine::model::{FixtureWaypoint, Mode, Settings};

fn main() -> eframe::Result {
    let mut settings = Settings::default();
    let mut auto_connect = false;
    let mut screenshot = None;
    let mut screenshot_after_goal = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--fixture-preview" => {
                settings.mode = Mode::Demo;
                auto_connect = true;
            }
            "--live-preview" => {
                settings.mode = Mode::Live;
                auto_connect = true;
            }
            "--legacy-forwarding" => settings.legacy_forwarding = true,
            "--fixture-reachable-waypoint" => {
                settings.fixture_waypoint = FixtureWaypoint::Reachable
            }
            "--objective" => {
                settings.objective = args
                    .next()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| {
                        eprintln!("--objective requires the session goal as text");
                        std::process::exit(2)
                    });
            }
            "--max-requests" => {
                settings.max_requests = number(&mut args, "--max-requests", 1, 100_000) as u32
            }
            "--max-seconds" => {
                settings.max_seconds = number(&mut args, "--max-seconds", 1, 604_800)
            }
            "--request-interval-seconds" => {
                settings.request_interval_ms =
                    number(&mut args, "--request-interval-seconds", 0, 86_400) * 1_000
            }
            "--safety-reflex" => settings.safety_reflex = true,
            "--port" => {
                settings.port = args
                    .next()
                    .and_then(|value| value.parse::<u16>().ok())
                    .filter(|port| *port != 0)
                    .unwrap_or_else(|| {
                        eprintln!("--port requires a number between 1 and 65535");
                        std::process::exit(2)
                    });
            }
            "--bot" => {
                settings.bot_name = args
                    .next()
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| {
                        eprintln!("--bot requires a name");
                        std::process::exit(2)
                    });
            }
            "--screenshot" => {
                screenshot = Some(
                    args.next()
                        .filter(|value| !value.is_empty())
                        .unwrap_or_else(|| {
                            eprintln!("--screenshot requires a PNG path");
                            std::process::exit(2)
                        }),
                );
            }
            "--screenshot-after-goal" => {
                screenshot_after_goal = Some(
                    args.next()
                        .filter(|value| !value.is_empty())
                        .unwrap_or_else(|| {
                            eprintln!("--screenshot-after-goal requires a PNG path");
                            std::process::exit(2)
                        }),
                );
            }
            _ => {
                eprintln!("Unknown argument: {arg}");
                std::process::exit(2);
            }
        }
    }
    if screenshot.is_some() && screenshot_after_goal.is_some() {
        eprintln!(
            "Use either --screenshot (first observation) or --screenshot-after-goal (first recorded arrival verdict), not both."
        );
        std::process::exit(2);
    }
    if (screenshot.is_some() || screenshot_after_goal.is_some())
        && std::env::var_os("EFRAME_SCREENSHOT_TO").is_some()
    {
        eprintln!(
            "Remove EFRAME_SCREENSHOT_TO from the environment when using --screenshot; the framework capture would otherwise exit the app early."
        );
        std::process::exit(2);
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 780.0])
            .with_min_inner_size([600.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Jev Game Engine",
        options,
        Box::new(move |cc| {
            Ok(Box::new(ui::GameApp::new(
                cc,
                settings,
                auto_connect,
                screenshot,
                screenshot_after_goal,
            )))
        }),
    )
}

/// Reads one bounded integer argument. A missing or out-of-range value exits rather than
/// silently keeping a default, because these bounds decide provider spend.
fn number(args: &mut std::iter::Skip<std::env::Args>, flag: &str, low: u64, high: u64) -> u64 {
    args.next()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (low..=high).contains(value))
        .unwrap_or_else(|| {
            eprintln!("{flag} requires an integer from {low} to {high}");
            std::process::exit(2)
        })
}
