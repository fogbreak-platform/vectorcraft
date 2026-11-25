//! Direct `<canvas data-host="page">` host bridge and `window.fogbreakVectorHost` RPC handle.
#![cfg(target_arch = "wasm32")]
#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use js_sys::{Array, Function, Object, Promise, Reflect, Uint8Array};
use serde_json::{Value, json};
use vectorcraft_ui_egui::{ControlRequest, ControlTicket, Inbox, VectorcraftApp};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;
use wasm_bindgen_futures::{JsFuture, future_to_promise};

pub const ENGINE_ID: &str = "fogbreak-vector";
pub const HOST_KEY: &str = "fogbreakVectorHost";
pub const CANVAS_ID: &str = "fogbreak_vector_canvas";
pub const LOADING_ID: &str = "fogbreak_vector_loading";

const EXPORT_FORMATS: [&str; 5] = ["svg", "pdf", "eps", "png", "ai"];

pub enum IoJob {
    Describe {
        command: String,
        tx: Sender<Value>,
    },
    Export {
        format: String,
        params: Value,
        tx: Sender<Result<Vec<u8>, String>>,
    },
    Import {
        name: String,
        bytes: Vec<u8>,
        tx: Sender<Result<Value, String>>,
    },
}

#[derive(Clone, Default)]
pub struct IoQueue {
    items: Rc<RefCell<Vec<IoJob>>>,
}

impl IoQueue {
    pub fn push(&self, job: IoJob) {
        self.items.borrow_mut().push(job);
    }

    pub fn drain_into(&self, app: &mut VectorcraftApp) {
        let items: Vec<IoJob> = self.items.borrow_mut().drain(..).collect();
        for job in items {
            match job {
                IoJob::Describe { command, tx } => {
                    let _ = tx.send(describe_command_json(app, &command));
                }
                IoJob::Export { format, params, tx } => {
                    let _ = tx.send(export_bytes(app, &format, &params));
                }
                IoJob::Import { name, bytes, tx } => {
                    let _ = tx.send(import_bytes(app, &name, &bytes));
                }
            }
        }
    }
}

fn describe_command_json(app: &VectorcraftApp, command: &str) -> Value {
    let spec = vectorcraft_engine::find_command(command).or_else(|| {
        vectorcraft_engine::command_specs()
            .iter()
            .find(|c| c.id.eq_ignore_ascii_case(command))
    });
    let Some(spec) = spec else {
        return Value::Null;
    };
    let info = spec.info(&app.session);
    let mut val = serde_json::to_value(&info).unwrap_or_else(|_| json!({ "id": spec.id, "label": spec.label }));
    if let Some(map) = val.as_object_mut() {
        map.insert("summary".to_string(), Value::String(spec.label.to_string()));
        map.insert("mutates".to_string(), Value::Bool(spec.journal));
    }
    val
}

fn export_bytes(app: &VectorcraftApp, format: &str, params: &Value) -> Result<Vec<u8>, String> {
    let fmt = format.trim().trim_start_matches('.').to_ascii_lowercase();
    if !matches!(fmt.as_str(), "svg" | "pdf" | "eps" | "png" | "ai") {
        return Err(format!("unsupported export format `{fmt}`"));
    }
    let st = app.session.active().ok_or_else(|| "no active document".to_string())?;
    vectorcraft_engine::cmd::fileio::encode(&st.doc, &fmt, params).map_err(|e| e.to_string())
}

fn import_bytes(app: &mut VectorcraftApp, name: &str, bytes: &[u8]) -> Result<Value, String> {
    let result = vectorcraft_ui_egui::io::open_bytes(app, name, bytes, None)?;
    Ok(json!({
        "ok": true,
        "name": name,
        "byteLength": bytes.len(),
        "result": result,
    }))
}

thread_local! {
    static CANVAS_REF: RefCell<Option<web_sys::HtmlCanvasElement>> = const { RefCell::new(None) };
    static LAST_DIRTY: RefCell<Option<bool>> = const { RefCell::new(None) };
    static REVISION: RefCell<u64> = const { RefCell::new(0) };
}

pub fn find_page_canvas() -> Result<web_sys::HtmlCanvasElement, String> {
    let window = web_sys::window().ok_or_else(|| "missing browser window".to_string())?;
    if let Ok(top) = window.top()
        && let Some(top_win) = top
        && !js_sys::Object::is(&window, &top_win)
    {
        return Err("Refusing to mount inside an <iframe>; use <canvas data-host=\"page\">.".into());
    }
    let document = window.document().ok_or_else(|| "missing browser document".to_string())?;
    let el = document
        .query_selector("canvas[data-host=\"page\"][data-engine=\"fogbreak-vector\"]")
        .ok()
        .flatten()
        .or_else(|| document.get_element_by_id(CANVAS_ID))
        .ok_or_else(|| format!("missing <canvas id=\"{CANVAS_ID}\" data-host=\"page\">"))?;
    if el.tag_name().eq_ignore_ascii_case("IFRAME") {
        return Err("Refusing to mount on an <iframe>; use <canvas data-host=\"page\">.".into());
    }
    let canvas = el
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .map_err(|_| "target element is not a <canvas>".to_string())?;
    let _ = canvas.set_attribute("data-host", "page");
    let _ = canvas.set_attribute("data-engine", ENGINE_ID);
    let _ = canvas.set_attribute("data-rpc-key", HOST_KEY);
    let _ = canvas.set_attribute("data-iframe", "false");
    CANVAS_REF.with(|slot| *slot.borrow_mut() = Some(canvas.clone()));
    Ok(canvas)
}

pub fn listen_open(canvas: &web_sys::HtmlCanvasElement, inbox: Inbox, ctx: egui::Context) {
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |ev: web_sys::Event| {
        let Ok(custom) = ev.dyn_into::<web_sys::CustomEvent>() else { return };
        let detail = custom.detail();
        let name = Reflect::get(&detail, &"name".into())
            .ok()
            .and_then(|v| v.as_string())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "artwork.svg".to_string());
        if let Ok(bytes_val) = Reflect::get(&detail, &"bytes".into()) {
            let bytes = Uint8Array::new(&bytes_val).to_vec();
            if !bytes.is_empty() {
                if let Ok(mut guard) = inbox.lock() {
                    guard.push((name, bytes));
                }
                ctx.request_repaint();
            }
        }
    });
    let _ = canvas.add_event_listener_with_callback("open", cb.as_ref().unchecked_ref());
    cb.forget();
}

pub fn emit_ready(capabilities: &[String]) {
    let detail = Object::new();
    let caps = Array::new();
    for cap in capabilities {
        caps.push(&JsValue::from_str(cap));
    }
    let _ = Reflect::set(&detail, &"engineId".into(), &JsValue::from_str(ENGINE_ID));
    let _ = Reflect::set(&detail, &"hostMode".into(), &JsValue::from_str("page"));
    let _ = Reflect::set(&detail, &"capabilities".into(), &caps);
    dispatch_canvas_event("ready", &detail.into());
}

pub fn emit_dirty(dirty: bool) {
    let changed = LAST_DIRTY.with(|slot| {
        let prev = *slot.borrow();
        if prev == Some(dirty) {
            false
        } else {
            *slot.borrow_mut() = Some(dirty);
            true
        }
    });
    if !changed {
        return;
    }
    let rev = REVISION.with(|r| {
        let mut v = r.borrow_mut();
        *v += 1;
        *v
    });
    let detail = Object::new();
    let _ = Reflect::set(&detail, &"dirty".into(), &JsValue::from_bool(dirty));
    let _ = Reflect::set(&detail, &"revision".into(), &JsValue::from_f64(rev as f64));
    dispatch_canvas_event("dirty", &detail.into());
}

pub fn deliver_save(name: &str, bytes: &[u8]) -> Result<(), String> {
    let format = name.rsplit('.').next().unwrap_or("svg").to_ascii_lowercase();
    let detail = Object::new();
    let arr = Uint8Array::from(bytes);
    let _ = Reflect::set(&detail, &"name".into(), &JsValue::from_str(name));
    let _ = Reflect::set(&detail, &"format".into(), &JsValue::from_str(&format));
    let _ = Reflect::set(&detail, &"bytes".into(), &arr.into());
    dispatch_canvas_event("save", &detail.into());
    Ok(())
}

pub fn emit_handoff(target_shore: &str, artifact_name: &str, format: &str, bytes: &[u8]) {
    let detail = Object::new();
    let arr = Uint8Array::from(bytes);
    let _ = Reflect::set(&detail, &"targetShore".into(), &JsValue::from_str(target_shore));
    let _ = Reflect::set(&detail, &"artifactName".into(), &JsValue::from_str(artifact_name));
    let _ = Reflect::set(&detail, &"format".into(), &JsValue::from_str(format));
    let _ = Reflect::set(&detail, &"bytes".into(), &arr.into());
    dispatch_canvas_event("handoff", &detail.into());
}

fn dispatch_canvas_event(name: &str, detail: &JsValue) {
    CANVAS_REF.with(|slot| {
        let Some(canvas) = slot.borrow().as_ref().cloned() else { return };
        let init = web_sys::CustomEventInit::new();
        init.set_detail(detail);
        init.set_bubbles(false);
        init.set_cancelable(false);
        if let Ok(ev) = web_sys::CustomEvent::new_with_event_init_dict(name, &init) {
            let _ = canvas.dispatch_event(&ev);
        }
    });
}

pub fn install_rpc(
    ctx: egui::Context,
    inbox: Inbox,
    io_queue: IoQueue,
    capabilities: Vec<String>,
) -> Receiver<ControlRequest> {
    let (tx, rx) = channel::<ControlRequest>();
    let Some(window) = web_sys::window() else { return rx };
    let handle = Object::new();
    let _ = Reflect::set(&handle, &"hostMode".into(), &JsValue::from_str("page"));
    let _ = Reflect::set(&handle, &"engineId".into(), &JsValue::from_str(ENGINE_ID));

    install_request_fns(&handle, &tx, &ctx, &io_queue, capabilities);
    install_inspect_fns(&handle, &tx, &ctx);
    install_io_fns(&handle, inbox, &io_queue, &ctx);

    let _ = Reflect::set(&window, &JsValue::from_str(HOST_KEY), &handle);
    rx
}

fn install_request_fns(
    handle: &Object,
    tx: &Sender<ControlRequest>,
    ctx: &egui::Context,
    io_queue: &IoQueue,
    capabilities: Vec<String>,
) {
    let req_tx = tx.clone();
    let req_ctx = ctx.clone();
    let req_fn = Closure::<dyn Fn(String, JsValue) -> Promise>::new(move |method: String, params: JsValue| {
        dispatch_rpc(req_tx.clone(), req_ctx.clone(), method, parse_params(&params))
    });
    let _ = Reflect::set(handle, &"request".into(), req_fn.as_ref().unchecked_ref());
    req_fn.forget();

    for alias in ["execute", "executeCommand"] {
        let ex_tx = tx.clone();
        let ex_ctx = ctx.clone();
        let ex_fn = Closure::<dyn Fn(String, JsValue) -> Promise>::new(move |command: String, params: JsValue| {
            let body = json!({ "command": command, "params": parse_params(&params) });
            dispatch_rpc(ex_tx.clone(), ex_ctx.clone(), "engine.execute".to_string(), body)
        });
        let _ = Reflect::set(handle, &JsValue::from_str(alias), ex_fn.as_ref().unchecked_ref());
        ex_fn.forget();
    }

    for alias in ["commands", "listCommands"] {
        let caps = capabilities.clone();
        let cmd_fn = Closure::<dyn Fn() -> Promise>::new(move || {
            let arr: Vec<Value> = caps.iter().cloned().map(Value::String).collect();
            future_to_promise(async move { Ok(to_js(&Value::Array(arr))) })
        });
        let _ = Reflect::set(handle, &JsValue::from_str(alias), cmd_fn.as_ref().unchecked_ref());
        cmd_fn.forget();
    }

    let desc_q = io_queue.clone();
    let desc_ctx = ctx.clone();
    let desc_fn = Closure::<dyn Fn(String) -> Promise>::new(move |command: String| {
        let q = desc_q.clone();
        let c = desc_ctx.clone();
        future_to_promise(async move {
            let (tx, rx) = channel();
            q.push(IoJob::Describe { command, tx });
            c.request_repaint();
            loop {
                match rx.try_recv() {
                    Ok(val) => return Ok(to_js(&val)),
                    Err(TryRecvError::Empty) => sleep_ms(16).await,
                    Err(TryRecvError::Disconnected) => return Err(JsValue::from_str("describe request dropped")),
                }
            }
        })
    });
    let _ = Reflect::set(handle, &"describeCommand".into(), desc_fn.as_ref().unchecked_ref());
    desc_fn.forget();
}

fn install_inspect_fns(handle: &Object, tx: &Sender<ControlRequest>, ctx: &egui::Context) {
    for alias in ["docInspect", "inspect"] {
        let doc_tx = tx.clone();
        let doc_ctx = ctx.clone();
        let doc_fn = Closure::<dyn Fn(JsValue) -> Promise>::new(move |params: JsValue| {
            dispatch_rpc(doc_tx.clone(), doc_ctx.clone(), "document.inspect".to_string(), parse_params(&params))
        });
        let _ = Reflect::set(handle, &JsValue::from_str(alias), doc_fn.as_ref().unchecked_ref());
        doc_fn.forget();
    }

    let ui_tx = tx.clone();
    let ui_ctx = ctx.clone();
    let ui_fn = Closure::<dyn Fn(JsValue) -> Promise>::new(move |params: JsValue| {
        dispatch_rpc(ui_tx.clone(), ui_ctx.clone(), "ui.inspect".to_string(), parse_params(&params))
    });
    let _ = Reflect::set(handle, &"uiInspect".into(), ui_fn.as_ref().unchecked_ref());
    ui_fn.forget();

    let shot_tx = tx.clone();
    let shot_ctx = ctx.clone();
    let shot_fn = Closure::<dyn Fn(JsValue) -> Promise>::new(move |params: JsValue| {
        let tx = shot_tx.clone();
        let ctx = shot_ctx.clone();
        let mut p = parse_params(&params);
        if let Some(obj) = p.as_object_mut() {
            obj.entry("data").or_insert(Value::Bool(true));
        }
        future_to_promise(async move {
            let val = send_control(tx, ctx, "ui.render".to_string(), p).await?;
            if let Some(b64) = val.get("pngBase64").and_then(Value::as_str)
                && let Ok(bytes) = decode_base64(b64)
            {
                return Ok(Uint8Array::from(bytes.as_slice()).into());
            }
            Ok(Uint8Array::from([0x89u8, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A].as_slice()).into())
        })
    });
    let _ = Reflect::set(handle, &"screenshot".into(), shot_fn.as_ref().unchecked_ref());
    shot_fn.forget();
}

fn install_io_fns(handle: &Object, inbox: Inbox, io_queue: &IoQueue, ctx: &egui::Context) {
    let open_inbox = inbox;
    let open_ctx = ctx.clone();
    let open_fn = Closure::<dyn Fn(String, JsValue) -> Promise>::new(move |name: String, bytes: JsValue| {
        let data = Uint8Array::new(&bytes).to_vec();
        if let Ok(mut guard) = open_inbox.lock() {
            guard.push((name, data));
        }
        open_ctx.request_repaint();
        future_to_promise(async { Ok(JsValue::TRUE) })
    });
    let _ = Reflect::set(handle, &"open".into(), open_fn.as_ref().unchecked_ref());
    open_fn.forget();

    let handoff_fn = Closure::<dyn Fn(String, String, String, JsValue)>::new(
        |target_shore: String, artifact_name: String, format: String, bytes: JsValue| {
            let data = Uint8Array::new(&bytes).to_vec();
            emit_handoff(&target_shore, &artifact_name, &format, &data);
        },
    );
    let _ = Reflect::set(handle, &"emitHandoff".into(), handoff_fn.as_ref().unchecked_ref());
    handoff_fn.forget();

    install_native_io_object(handle, io_queue, ctx);
}

fn install_native_io_object(handle: &Object, io_queue: &IoQueue, ctx: &egui::Context) {
    let io_obj = Object::new();

    let formats_fn = Closure::<dyn Fn() -> Promise>::new(|| {
        future_to_promise(async {
            let arr: Vec<Value> = EXPORT_FORMATS.iter().map(|f| Value::String((*f).to_string())).collect();
            Ok(to_js(&Value::Array(arr)))
        })
    });
    let _ = Reflect::set(&io_obj, &"formats".into(), formats_fn.as_ref().unchecked_ref());
    formats_fn.forget();

    let exp_q = io_queue.clone();
    let exp_c = ctx.clone();
    let export_fn = Closure::<dyn Fn(String, JsValue) -> Promise>::new(move |format: String, params: JsValue| {
        let q = exp_q.clone();
        let c = exp_c.clone();
        let p = parse_params(&params);
        future_to_promise(async move {
            let (tx, rx) = channel();
            q.push(IoJob::Export { format, params: p, tx });
            c.request_repaint();
            loop {
                match rx.try_recv() {
                    Ok(Ok(bytes)) => return Ok(Uint8Array::from(bytes.as_slice()).into()),
                    Ok(Err(e)) => return Err(JsValue::from_str(&e)),
                    Err(TryRecvError::Empty) => sleep_ms(16).await,
                    Err(TryRecvError::Disconnected) => return Err(JsValue::from_str("export request dropped")),
                }
            }
        })
    });
    let _ = Reflect::set(&io_obj, &"export".into(), export_fn.as_ref().unchecked_ref());
    export_fn.forget();

    let imp_q = io_queue.clone();
    let imp_c = ctx.clone();
    let import_fn = Closure::<dyn Fn(JsValue) -> Promise>::new(move |detail: JsValue| {
        let q = imp_q.clone();
        let c = imp_c.clone();
        let parsed = extract_import_detail(&detail);
        future_to_promise(async move {
            let (name, bytes) = parsed.map_err(|e| JsValue::from_str(&e))?;
            let (tx, rx) = channel();
            q.push(IoJob::Import { name, bytes, tx });
            c.request_repaint();
            loop {
                match rx.try_recv() {
                    Ok(Ok(val)) => return Ok(to_js(&val)),
                    Ok(Err(e)) => return Err(JsValue::from_str(&e)),
                    Err(TryRecvError::Empty) => sleep_ms(16).await,
                    Err(TryRecvError::Disconnected) => return Err(JsValue::from_str("import request dropped")),
                }
            }
        })
    });
    let _ = Reflect::set(&io_obj, &"import".into(), import_fn.as_ref().unchecked_ref());
    import_fn.forget();

    let _ = Reflect::set(handle, &"io".into(), &io_obj);
}

fn infer_vector_ext(mime: Option<&str>) -> &'static str {
    match mime.unwrap_or("").to_ascii_lowercase().as_str() {
        "application/pdf" => "pdf",
        "application/postscript" | "image/x-eps" => "eps",
        "image/png" => "png",
        "application/illustrator" => "ai",
        _ => "svg",
    }
}

fn extract_import_detail(detail: &JsValue) -> Result<(String, Vec<u8>), String> {
    let mime = Reflect::get(detail, &"mimeType".into())
        .ok()
        .and_then(|v| v.as_string());
    let raw_name = Reflect::get(detail, &"name".into())
        .ok()
        .and_then(|v| v.as_string())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let name = match raw_name {
        Some(n) if n.contains('.') => n,
        Some(n) => format!("{n}.{}", infer_vector_ext(mime.as_deref())),
        None => format!("artwork.{}", infer_vector_ext(mime.as_deref())),
    };
    let bytes_val = Reflect::get(detail, &"bytes".into())
        .map_err(|_| "import requires a `bytes` Uint8Array".to_string())?;
    let bytes = Uint8Array::new(&bytes_val).to_vec();
    if bytes.is_empty() {
        return Err("import requires non-empty bytes".to_string());
    }
    Ok((name, bytes))
}

struct Waiting {
    rx: Receiver<Value>,
    ticket: ControlTicket,
    answered: bool,
}

impl Drop for Waiting {
    fn drop(&mut self) {
        if !self.answered {
            self.ticket.give_up();
        }
    }
}

fn parse_params(value: &JsValue) -> Value {
    if value.is_undefined() || value.is_null() {
        return json!({});
    }
    js_sys::JSON::stringify(value)
        .ok()
        .and_then(|s| s.as_string())
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or_else(|| json!({}))
}

fn dispatch_rpc(tx: Sender<ControlRequest>, ctx: egui::Context, method: String, params: Value) -> Promise {
    future_to_promise(async move {
        let val = send_control(tx, ctx, method, params).await?;
        Ok(to_js(&val))
    })
}

async fn send_control(tx: Sender<ControlRequest>, ctx: egui::Context, method: String, params: Value) -> Result<Value, JsValue> {
    let (req, rx) = ControlRequest::new(method, params);
    let ticket = req.ticket();
    tx.send(req).map_err(|_| JsValue::from_str("Fogbreak Vector engine is not running"))?;
    ctx.request_repaint();
    let mut waiting = Waiting { rx, ticket, answered: false };
    let reply = loop {
        match waiting.rx.try_recv() {
            Ok(v) => {
                waiting.answered = true;
                break v;
            }
            Err(TryRecvError::Empty) => sleep_ms(16).await,
            Err(TryRecvError::Disconnected) => {
                waiting.answered = true;
                return Err(JsValue::from_str("request dropped"));
            }
        }
    };
    if reply.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    } else {
        Err(JsValue::from_str(reply.get("error").and_then(Value::as_str).unwrap_or("request failed")))
    }
}

async fn sleep_ms(ms: i32) {
    let promise = Promise::new(&mut |resolve: Function, _| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(promise).await;
}

fn to_js(val: &Value) -> JsValue {
    let text = serde_json::to_string(val).unwrap_or_else(|_| "null".to_string());
    js_sys::JSON::parse(&text).unwrap_or(JsValue::NULL)
}

fn decode_base64(input: &str) -> Result<Vec<u8>, ()> {
    let win = web_sys::window().ok_or(())?;
    let bin = win.atob(input).map_err(|_| ())?;
    Ok(bin.bytes().collect())
}
