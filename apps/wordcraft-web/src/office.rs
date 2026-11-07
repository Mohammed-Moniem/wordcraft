//! Token-bound host requests dispatched on the visible WordApp, with acknowledged saves.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;

use serde_json::{Value, json};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use wordcraft_ui_egui::async_saves::{AsyncSaves, SaveOutcome};
use wordcraft_ui_egui::{Services, WordApp};

const MAX_BYTES: usize = 32 << 20;
const MAX_JSON: usize = 1 << 20;
const MAX_QUEUE: usize = 32;
const MAX_IDS: usize = 10000;
const METHODS: &[&str] = &["office.open", "office.commands", "office.execute", "office.inspect", "office.export", "office.save", "office.close"];

struct Request {
    id: String,
    method: String,
    params: Value,
    bytes: Option<Vec<u8>>,
}
enum Incoming {
    Hello(String),
    Request(Request),
    Cancel(String),
    Saved { id: String, revision: Option<u64>, failed: bool },
}
struct Save {
    ticket: u64,
    request: Option<String>,
    document: u64,
    engine_revision: u64,
    revision: u64,
    deadline: f64,
}
#[derive(Default)]
struct State {
    token: Option<String>,
    queue: VecDeque<Incoming>,
    seen: BTreeSet<String>,
    cancelled: BTreeSet<String>,
    saves: BTreeMap<String, Save>,
    current_request: Option<String>,
    current: Option<(u64, u64)>,
    revision: u64,
    last_dirty: Option<bool>,
    last_notified_revision: Option<u64>,
    completed: Vec<(String, Save, bool)>,
}

struct Inner {
    parent: web_sys::Window,
    origin: String,
    ctx: egui::Context,
    saves: AsyncSaves,
    state: RefCell<State>,
    listener: RefCell<Option<Closure<dyn FnMut(web_sys::MessageEvent)>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(window) = web_sys::window()
            && let Some(listener) = self.listener.borrow().as_ref()
        {
            let _ = window.remove_event_listener_with_callback("message", listener.as_ref().unchecked_ref());
        }
    }
}

#[derive(Clone)]
pub struct Bridge(Rc<Inner>);

fn field(value: &JsValue, name: &str) -> JsValue {
    js_sys::Reflect::get(value, &JsValue::from_str(name)).unwrap_or(JsValue::UNDEFINED)
}
fn text(value: &JsValue, name: &str) -> Option<String> {
    field(value, name).as_string()
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
}
fn number(value: &JsValue, name: &str) -> Option<u64> {
    let n = field(value, name).as_f64()?;
    (n.is_finite() && n >= 0.0 && n <= 9_007_199_254_740_991.0 && n.fract() == 0.0).then_some(n as u64)
}

impl Bridge {
    pub fn connect(services: &mut Services, ctx: egui::Context, saves: AsyncSaves) -> Option<Self> {
        let window = web_sys::window()?;
        if !window.location().search().ok()?.trim_start_matches('?').split('&').any(|v| v == "host=parent") {
            return None;
        }
        let parent = window.parent().ok()??;
        if js_sys::Object::is(window.as_ref(), parent.as_ref()) {
            return None;
        }
        let origin = window.location().origin().ok()?;
        if !origin.starts_with("http://") && !origin.starts_with("https://") {
            return None;
        }
        if parent.location().origin().ok().as_deref() != Some(origin.as_str()) {
            return None;
        }
        let inner = Rc::new(Inner { parent, origin, ctx, saves: saves.clone(), state: RefCell::new(State::default()), listener: RefCell::new(None) });
        let weak = Rc::downgrade(&inner);
        let listener = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            let Some(inner) = weak.upgrade() else { return };
            if event.origin() != inner.origin || !event.source().is_some_and(|source| js_sys::Object::is(source.as_ref(), inner.parent.as_ref())) {
                return;
            }
            let data = event.data();
            let kind = text(&data, "type").unwrap_or_default();
            let incoming = if kind == "wordcraft:saved" {
                text(&data, "id").filter(|id| valid_id(id)).map(|id| Incoming::Saved { id, revision: None, failed: false })
            } else if kind == "wordcraft:office" && text(&data, "protocol").as_deref() == Some("office") && number(&data, "version") == Some(1) {
                let Some(token) = text(&data, "sessionId").filter(|v| valid_id(v)) else { return };
                let request_kind = text(&data, "kind").unwrap_or_default();
                if request_kind == "hello" {
                    Some(Incoming::Hello(token))
                } else {
                    if inner.state.borrow().token.as_deref() != Some(token.as_str()) {
                        return;
                    }
                    match request_kind.as_str() {
                        "cancel" => text(&data, "id").filter(|id| valid_id(id)).map(Incoming::Cancel),
                        "saved" | "save-failed" => text(&data, "id").filter(|id| valid_id(id)).map(|id| Incoming::Saved {
                            id,
                            revision: number(&data, "revision"),
                            failed: request_kind == "save-failed",
                        }),
                        "request" => parse_request(&data).map(Incoming::Request),
                        _ => None,
                    }
                }
            } else {
                None
            };
            let Some(incoming) = incoming else { return };
            let mut state = inner.state.borrow_mut();
            if state.queue.len() >= MAX_QUEUE {
                return;
            }
            state.queue.push_back(incoming);
            inner.ctx.request_repaint();
        });
        if window.add_event_listener_with_callback("message", listener.as_ref().unchecked_ref()).is_err() {
            return None;
        }
        *inner.listener.borrow_mut() = Some(listener);
        let bridge = Self(inner);
        let save_bridge = bridge.clone();
        services.host_save_versioned =
            Some(Box::new(move |name, bytes, save_as, document, revision| save_bridge.post_save(name, bytes, save_as, document, revision)));
        services.async_saves = Some(saves);
        Some(bridge)
    }

    fn post(&self, value: Value, bytes: Option<&[u8]>) -> Result<(), String> {
        let object = js_sys::JSON::parse(&value.to_string()).map_err(|_| "cannot encode host response")?;
        if let Some(bytes) = bytes {
            if bytes.len() > MAX_BYTES {
                return Err("host file exceeds the 32 MiB limit".into());
            }
            let array = js_sys::Uint8Array::from(bytes);
            js_sys::Reflect::set(&object, &JsValue::from_str("bytes"), &array.buffer()).map_err(|_| "cannot attach saved bytes")?;
        }
        self.0.parent.post_message(&object, &self.0.origin).map_err(|_| "cannot post host response".into())
    }

    fn envelope(&self, kind: &str) -> Value {
        json!({"type":"wordcraft:office","protocol":"office","version":1,"sessionId":self.0.state.borrow().token,"kind":kind})
    }

    fn sync(&self, document: u64, engine_revision: u64) -> u64 {
        let mut state = self.0.state.borrow_mut();
        let current = (document, engine_revision);
        if state.current != Some(current) {
            state.revision = match state.current {
                None => engine_revision,
                Some((old_document, old_revision)) if old_document == document && engine_revision > old_revision => {
                    state.revision.saturating_add(engine_revision - old_revision)
                }
                Some(_) => state.revision.saturating_add(1),
            };
            state.current = Some(current);
        }
        state.revision
    }

    pub fn observe(&self, app: &WordApp) {
        let revision = self.sync(app.session.document_id(), app.session.rev());
        let dirty = app.host_has_unsaved_documents();
        let changed = {
            let mut state = self.0.state.borrow_mut();
            let changed = state.last_dirty != Some(dirty) || state.last_notified_revision != Some(revision);
            state.last_dirty = Some(dirty);
            state.last_notified_revision = Some(revision);
            changed
        };
        if changed && self.0.state.borrow().token.is_some() {
            let mut message = self.envelope("revision");
            message["revision"] = json!(revision);
            message["dirty"] = json!(dirty);
            let _ = self.post(message, None);
        }
    }

    fn response(&self, id: &str, result: Result<Value, (&str, &str)>) {
        let mut message = self.envelope("response");
        message["id"] = json!(id);
        message["revision"] = json!(self.0.state.borrow().revision);
        match result {
            Ok(value) => {
                message["ok"] = json!(true);
                message["result"] = value;
            }
            Err((code, error)) => {
                message["ok"] = json!(false);
                message["error"] = json!({"code":code,"message":error});
            }
        }
        let _ = self.post(message, None);
    }

    fn post_save(&self, name: &str, bytes: &[u8], save_as: bool, document: u64, engine_revision: u64) -> Result<(), String> {
        if bytes.len() > MAX_BYTES || self.0.state.borrow().saves.len() >= 8 {
            return Err("host save capacity exceeded".into());
        }
        let revision = self.sync(document, engine_revision);
        let request = self.0.state.borrow().current_request.clone();
        let extended = self.0.state.borrow().token.is_some();
        if extended {
            // Announce the snapshot's actual engine revision before asking the host to store
            // its bytes. Reading a later UI frame could associate newer edits with old bytes.
            let mut changed = self.envelope("revision");
            changed["revision"] = json!(revision);
            changed["dirty"] = json!(true);
            self.post(changed, None)?;
            let mut state = self.0.state.borrow_mut();
            state.last_dirty = Some(true);
            state.last_notified_revision = Some(revision);
        }
        let ticket = self.0.saves.start();
        let id = format!("word-save-{ticket}");
        let mut message = if extended { self.envelope("save") } else { json!({"type":"wordcraft:save"}) };
        message["id"] = json!(id);
        message["name"] = json!(name);
        message["saveAs"] = json!(save_as);
        if extended {
            message["revision"] = json!(revision);
            if let Some(request) = &request {
                message["requestId"] = json!(request);
            }
        }
        self.0
            .state
            .borrow_mut()
            .saves
            .insert(id.clone(), Save { ticket, request, document, engine_revision, revision, deadline: js_sys::Date::now() + 30000.0 });
        if let Err(error) = self.post(message, Some(bytes)) {
            self.0.state.borrow_mut().saves.remove(&id);
            self.0.saves.finish(ticket, SaveOutcome::Failed(error.clone()));
            return Err(error);
        }
        self.0.ctx.request_repaint_after(std::time::Duration::from_secs(1));
        Ok(())
    }

    pub fn drain(&self, app: &mut WordApp, ctx: &egui::Context) {
        self.observe(app);
        // The preceding app.logic settled these tickets against their exact snapshot. Report
        // success only now, so a programmatic save also observes the settled UI state.
        let completed = std::mem::take(&mut self.0.state.borrow_mut().completed);
        for (request, save, failed) in completed {
            if failed {
                self.response(&request, Err(("SAVE_FAILED", "Host storage rejected the save")));
            } else {
                self.response(&request, Ok(json!({"saved":true,"revision":self.0.state.borrow().revision,"savedRevision":save.revision,"documentId":save.document,"engineRevision":save.engine_revision,"dirty":app.host_has_unsaved_documents()})));
            }
        }
        let incoming = std::mem::take(&mut self.0.state.borrow_mut().queue);
        // A cancellation queued before this frame's dispatch prevents that request from running.
        for message in &incoming {
            if let Incoming::Cancel(id) = message {
                let mut state = self.0.state.borrow_mut();
                if state.cancelled.len() < MAX_IDS {
                    state.cancelled.insert(id.clone());
                }
            }
        }
        for message in incoming {
            match message {
                Incoming::Hello(token) => {
                    let accepted = {
                        let mut state = self.0.state.borrow_mut();
                        if state.token.is_some() {
                            false
                        } else {
                            state.token = Some(token);
                            true
                        }
                    };
                    if accepted {
                        let mut ready = self.envelope("ready");
                        ready["revision"] = json!(self.0.state.borrow().revision);
                        ready["capabilities"] = json!({"methods":METHODS,"revisions":true,"durableSave":true});
                        let _ = self.post(ready, None);
                        self.0.state.borrow_mut().last_dirty = None;
                        self.observe(app);
                    }
                }
                Incoming::Cancel(id) => {
                    if self.0.state.borrow().cancelled.len() < MAX_IDS {
                        self.0.state.borrow_mut().cancelled.insert(id);
                    }
                }
                Incoming::Saved { id, revision, failed } => {
                    let matches = {
                        let state = self.0.state.borrow();
                        state.saves.get(&id).is_some_and(|save| match revision {
                            Some(rev) => rev == save.revision,
                            None => state.token.is_none(),
                        })
                    };
                    if !matches {
                        continue;
                    }
                    let Some(save) = self.0.state.borrow_mut().saves.remove(&id) else { continue };
                    self.0
                        .saves
                        .finish(save.ticket, if failed { SaveOutcome::Failed("host storage rejected the save".into()) } else { SaveOutcome::Saved });
                    if let Some(request) = save.request.clone() {
                        self.0.state.borrow_mut().completed.push((request, save, failed));
                    }
                    ctx.request_repaint();
                }
                Incoming::Request(request) => self.dispatch(app, ctx, request),
            }
            self.observe(app);
        }
        let expired: Vec<String> =
            self.0.state.borrow().saves.iter().filter(|(_, save)| save.deadline <= js_sys::Date::now()).map(|(id, _)| id.clone()).collect();
        for id in expired {
            let save = self.0.state.borrow_mut().saves.remove(&id);
            if let Some(save) = save {
                self.0.saves.finish(save.ticket, SaveOutcome::Failed("host save acknowledgement timed out".into()));
                if let Some(request) = save.request {
                    self.response(&request, Err(("TIMEOUT", "Host save acknowledgement timed out")));
                }
            }
        }
        if !self.0.state.borrow().saves.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
    }

    fn dispatch(&self, app: &mut WordApp, ctx: &egui::Context, request: Request) {
        let id = &request.id;
        {
            let mut state = self.0.state.borrow_mut();
            if state.seen.len() >= MAX_IDS || !state.seen.insert(id.clone()) {
                drop(state);
                self.response(id, Err(("REQUEST_LIMIT", "Duplicate request or exhausted session")));
                return;
            }
            if state.cancelled.remove(id) {
                drop(state);
                self.response(id, Err(("ABORTED", "Request cancelled before dispatch")));
                return;
            }
        }
        let revision = self.sync(app.session.document_id(), app.session.rev());
        if let Some(expected) = request.params.get("expectedRevision")
            && expected.as_u64() != Some(revision)
        {
            self.response(id, Err(("STALE_REVISION", "Document revision does not match")));
            return;
        }
        if !METHODS.contains(&request.method.as_str()) {
            self.response(id, Err(("UNSUPPORTED", "Method is not implemented")));
            return;
        }
        if request.method == "office.open" && app.host_has_unsaved_documents() {
            self.response(
                id,
                Err(("UNSAVED_CHANGES", "Save or close unsaved documents and wait for pending writes before opening another document")),
            );
            return;
        }
        let result = match request.method.as_str() {
            "office.open" => {
                let name = request.params.get("name").and_then(Value::as_str).unwrap_or("Document.docx");
                if let Some(bytes) = request.bytes {
                    app.open_from_host(&craft_web_host::file_name(Some(name), "Document.docx"), &bytes).map(|_| { self.sync(app.session.document_id(), app.session.rev()); json!({"confirmed":true,"app":"wordcraft","sessionId":self.0.state.borrow().token,"revision":self.0.state.borrow().revision,"documentId":app.session.document_id()}) })
                } else {
                    Err("open requires ArrayBuffer bytes".into())
                }
            }
            "office.commands" => {
                let query = request.params.get("query").and_then(Value::as_str).unwrap_or("").to_ascii_lowercase();
                let commands: Vec<Value> = app.session.registry.all().iter().filter(|c| !restricted(c.id) && (query.is_empty() || c.id.to_ascii_lowercase().contains(&query) || c.label.to_ascii_lowercase().contains(&query)))
                    .take(256).map(|c| json!({"id":c.id,"label":c.label,"location":c.location,"params":c.params,"enabled":app.session.why_disabled(c.id).is_none()})).collect();
                Ok(json!(commands))
            }
            "office.inspect" => {
                if !app.has_document() {
                    Err("no document is open".into())
                } else {
                    app.session.run("document.inspect", &request.params).map_err(|_| "document inspection failed".into())
                }
            }
            "office.execute" => {
                let command = request.params.get("command").and_then(Value::as_str).unwrap_or("");
                if restricted(command) {
                    Err("command requires a dedicated host capability".into())
                } else {
                    app.execute(command, request.params.get("params").cloned().unwrap_or(json!({})))
                }
            }
            "office.save" => {
                self.0.state.borrow_mut().current_request = Some(id.clone());
                let command = if request.params.get("saveAs").and_then(Value::as_bool) == Some(true) { "file.saveAs" } else { "file.save" };
                let result = app.execute(command, json!({}));
                self.0.state.borrow_mut().current_request = None;
                if result.as_ref().is_ok_and(|v| v.get("pendingSave").is_some()) {
                    self.observe(app);
                    return;
                }
                Err("save did not start an acknowledgement ticket".into())
            }
            "office.export" => {
                let format = request.params.get("format").and_then(Value::as_str).unwrap_or("docx").to_ascii_lowercase();
                if !app.has_document() || !["docx", "docm", "dotx", "dotm", "odt", "rtf", "txt", "md", "html", "pdf"].contains(&format.as_str()) {
                    Err("unsupported export format".into())
                } else {
                    let name = format!("Document.{format}");
                    match wordcraft_engine::io::save_bytes(&name, &app.session.doc) {
                        Ok(bytes) if bytes.len() <= MAX_BYTES => {
                            self.sync(app.session.document_id(), app.session.rev());
                            let mut message = self.envelope("response");
                            message["id"] = json!(id);
                            message["ok"] = json!(true);
                            message["revision"] = json!(self.0.state.borrow().revision);
                            message["result"] = json!({"name":name,"revision":self.0.state.borrow().revision});
                            let object = js_sys::JSON::parse(&message.to_string());
                            if let Ok(object) = object {
                                let result = field(&object, "result");
                                let array = js_sys::Uint8Array::from(bytes.as_slice());
                                if js_sys::Reflect::set(&result, &JsValue::from_str("bytes"), &array.buffer()).is_ok() {
                                    let _ = self.0.parent.post_message(&object, &self.0.origin);
                                }
                            }
                            return;
                        }
                        _ => Err("export failed or exceeded the byte limit".into()),
                    }
                }
            }
            "office.close" => {
                if app.host_has_unsaved_other_documents() || self.0.saves.pending() > 0 {
                    Err("another open document or save still contains unconfirmed work".into())
                } else {
                    app.execute("file.close", json!({"force":request.params.get("discardChanges").and_then(Value::as_bool) == Some(true)}))
                }
            }
            _ => Err("unsupported method".into()),
        };
        self.observe(app);
        match result {
            Ok(result) => self.response(id, Ok(result)),
            Err(_) => self.response(id, Err(("ENGINE_ERROR", "Document operation failed; no success was confirmed"))),
        }
        ctx.request_repaint();
    }
}

fn restricted(command: &str) -> bool {
    command.starts_with("file.")
        || command.starts_with("app.")
        || command.starts_with("ui.")
        || command.starts_with("font.")
        || command.starts_with("tools.")
        || command.starts_with("mailings.")
        || command.starts_with("view.switch")
        || command.starts_with("view.newWindow")
        || command.starts_with("insert.link")
        || command.starts_with("insert.picture")
        || command.starts_with("insert.object")
        || command.starts_with("zotero.")
}

fn parse_request(data: &JsValue) -> Option<Request> {
    let id = text(data, "id").filter(|v| valid_id(v))?;
    let method = text(data, "method").filter(|v| v.len() <= 100)?;
    let params = field(data, "params");
    let mut budget = MAX_JSON;
    if !bounded_json(&params, 0, &mut budget) {
        return None;
    }
    let raw = js_sys::JSON::stringify(&params).ok()?.as_string()?;
    if raw.len() > MAX_JSON {
        return None;
    }
    let parsed: Value = serde_json::from_str(&raw).ok()?;
    if !parsed.is_object() {
        return None;
    }
    let bytes = if method == "office.open" {
        let value = field(&params, "bytes");
        if !value.is_instance_of::<js_sys::ArrayBuffer>() {
            return None;
        }
        let array = js_sys::Uint8Array::new(&value);
        if array.length() as usize > MAX_BYTES {
            return None;
        }
        Some(array.to_vec())
    } else {
        None
    };
    Some(Request { id, method, params: parsed, bytes })
}

/// Structured-clone messages cannot contain getters, but may still carry huge strings or
/// nested values. Bound them before JSON.stringify allocates the parameter encoding.
fn bounded_json(value: &JsValue, depth: usize, budget: &mut usize) -> bool {
    if depth > 32 || *budget < 8 {
        return false;
    }
    *budget -= 8;
    if value.is_null() || value.is_undefined() || value.as_bool().is_some() || value.as_f64().is_some() {
        return true;
    }
    if value.is_string() {
        let length = js_sys::JsString::from(value.clone()).length() as usize;
        let Some(left) = budget.checked_sub(length.saturating_mul(2)) else { return false };
        *budget = left;
        return true;
    }
    if value.is_instance_of::<js_sys::ArrayBuffer>() {
        return js_sys::Uint8Array::new(value).length() as usize <= MAX_BYTES;
    }
    if !value.is_object() || value.is_function() {
        return false;
    }
    if value.dyn_ref::<js_sys::Array>().is_some_and(|array| array.length() > 100000) {
        return false;
    }
    let keys = js_sys::Object::keys(&js_sys::Object::from(value.clone()));
    if keys.length() > 10000 {
        return false;
    }
    for key in keys.iter() {
        if !bounded_json(&key, depth + 1, budget) {
            return false;
        }
        let Ok(child) = js_sys::Reflect::get(value, &key) else { return false };
        if !bounded_json(&child, depth + 1, budget) {
            return false;
        }
    }
    true
}
