//! Fogbreak Vector Studio browser runner (WebGPU with WebGL2 fallback, `<canvas data-host="page">`).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

#[cfg(target_arch = "wasm32")]
mod host_bridge;
#[cfg(target_arch = "wasm32")]
mod theme;

#[cfg(target_arch = "wasm32")]
mod web {
    use std::sync::{Arc, Mutex};

    use vectorcraft_engine::Session;
    use vectorcraft_ui_egui::{Inbox, Services, VectorcraftApp};

    use crate::host_bridge::{self, IoQueue, LOADING_ID};
    use crate::theme::apply_palette;

    struct Shell {
        app: VectorcraftApp,
        io_queue: IoQueue,
        dark: bool,
    }

    impl eframe::App for Shell {
        fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            self.io_queue.drain_into(&mut self.app);
            self.app.tick(ctx);
            self.app.finish_ime_logic(ctx);
            apply_palette(ctx, self.dark);
            let dirty = self.app.session.active().is_some_and(|d| d.is_dirty());
            host_bridge::emit_dirty(dirty);
        }

        fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
            self.app.raw_input_hook(ctx, raw);
        }

        fn persist_egui_memory(&self) -> bool {
            false
        }

        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            self.app.ui(ui);
        }
    }

    fn query() -> String {
        web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default()
    }

    fn prefers_dark() -> bool {
        let q = query();
        if q.contains("theme=light") {
            return false;
        }
        if q.contains("theme=dark") {
            return true;
        }
        web_sys::window()
            .and_then(|w| w.match_media("(prefers-color-scheme: dark)").ok().flatten())
            .is_some_and(|m| m.matches())
    }

    pub fn start() {
        eframe::WebLogger::init(log::LevelFilter::Info).ok();
        let canvas = match host_bridge::find_page_canvas() {
            Ok(c) => c,
            Err(err) => {
                log::error!("Fogbreak Vector host error: {err}");
                return;
            }
        };
        let dark = prefers_dark();

        wasm_bindgen_futures::spawn_local(async move {
            let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
            let mut options = eframe::WebOptions::default();
            if query().contains("webgl")
                && let eframe::egui_wgpu::WgpuSetup::CreateNew(create) = &mut options.wgpu_options.wgpu_setup
            {
                create.instance_descriptor.backends = eframe::wgpu::Backends::GL;
            }

            let result = eframe::WebRunner::new()
                .start(
                    canvas.clone(),
                    options,
                    Box::new(move |cc| {
                        let page_inbox: Inbox = Arc::new(Mutex::new(Vec::new()));
                        let io_queue = IoQueue::default();
                        let session = Session::new();
                        let capabilities: Vec<String> = session
                            .commands()
                            .into_iter()
                            .map(|c| c.id.to_string())
                            .collect();
                        let control_rx = host_bridge::install_rpc(
                            cc.egui_ctx.clone(),
                            page_inbox.clone(),
                            io_queue.clone(),
                            capabilities.clone(),
                        );
                        host_bridge::listen_open(&canvas, page_inbox.clone(), cc.egui_ctx.clone());

                        let services = Services {
                            download: Some(Box::new(|name: &str, bytes: &[u8]| {
                                host_bridge::deliver_save(name, bytes)?;
                                Ok(())
                            })),
                            page_inbox: Some(page_inbox),
                            ..Services::default()
                        };
                        let app = VectorcraftApp::new(session, services).with_control(control_rx);
                        apply_palette(&cc.egui_ctx, dark);
                        host_bridge::emit_ready(&capabilities);
                        Ok(Box::new(Shell { app, io_queue, dark }))
                    }),
                )
                .await;

            if let Some(el) = document.get_element_by_id(LOADING_ID) {
                match result {
                    Ok(()) => el.remove(),
                    Err(e) => el.set_inner_html(&format!("<p>Fogbreak Vector Studio failed to start: {e:?}</p>")),
                }
            }
        });
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {
    web::start();
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("fogbreak-vector runs in the browser via trunk build --release");
}
