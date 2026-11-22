#![warn(clippy::all, rust_2018_idioms)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

// When compiling natively:
#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    env_logger::init(); // Log to stderr (if you run with `RUST_LOG=debug`).

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Midi App")
            .with_inner_size([1100.0, 700.0])
            .with_min_inner_size([480.0, 320.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "Midi App",
        native_options,
        Box::new(|cc| Ok(Box::new(midi_app::MidiApp::new(cc)))),
    )
}

// When compiling to web using trunk:
#[cfg(target_arch = "wasm32")]
fn main() {
    use wasm_bindgen::JsCast;

    // Redirect `log` messages to the browser console.
    eframe::WebLogger::init(log::LevelFilter::Info).ok();

    wasm_bindgen_futures::spawn_local(async {
        let document = web_sys::window()
            .and_then(|w| w.document())
            .expect("no document");
        let canvas = document
            .get_element_by_id("midi_app_canvas")
            .expect("index.html is missing <canvas id=\"midi_app_canvas\">")
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .expect("#midi_app_canvas is not a <canvas>");

        let result = eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|cc| Ok(Box::new(midi_app::MidiApp::new(cc)))),
            )
            .await;

        // Replace the loading spinner with the app, or with an error message.
        if let Some(loading) = document.get_element_by_id("loading") {
            match result {
                Ok(()) => loading.remove(),
                Err(e) => {
                    loading.set_inner_html(
                        "<p>Sorry, the MIDI editor failed to start.</p>\
                         <p style=\"font-size:14px\">Your browser may not support WebGL. \
                         See the developer console for details.</p>",
                    );
                    log::error!("Failed to start: {e:?}");
                }
            }
        }
    });
}
