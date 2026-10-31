//! The server over an in-memory connection: the JSON-RPC round trip, position conversion, and each
//! request kind answering.

use super::*;
use std::thread;

const URI: &str = "file:///tmp/pieces/demo/demo.json5";
const DOC: &str = "{\n  schema_version: \"0.9\",\n  transport: { bpm: 60, master_steps: 4 },\n  harmony: { root: \"D\", mode: \"dorian\" },\n  instruments: { pad: { note_engine: { polyphony: 2 }, params: { wave: \"saw_soft\", level_db: -7.96, cutof_hz: 800 } } },\n  modulators: { seq: { type: \"step_seq\", output: \"note\", length: 4, steps: [ { step: 0, voicing: { v1: { offset: 7 } } } ] } },\n  matrix: { m_notes: { source: \"seq\", dest: \"instruments.pad.notes\", kind: \"note\", voices: 2 } },\n  song: { sequence: [ { loops: 2 } ] },\n}\n";

struct Client {
    conn: Connection,
    next: i32,
}

impl Client {
    fn request(&mut self, method: &str, params: Value) -> Response {
        self.next += 1;
        self.conn.sender.send(request(self.next, method, params)).unwrap();
        loop {
            match self.conn.receiver.recv().unwrap() {
                Message::Response(r) => return r,
                _ => continue, // diagnostics etc.
            }
        }
    }
    fn notify(&self, method: &str, params: Value) {
        self.conn.sender.send(Message::Notification(Notification::new(method.into(), params))).unwrap();
    }
    fn next_notification(&self) -> Notification {
        loop {
            if let Message::Notification(n) = self.conn.receiver.recv().unwrap() {
                return n;
            }
        }
    }
    /// (line, UTF-16 column) of the first `needle` + `delta` in DOC.
    fn at(needle: &str, delta: usize) -> Value {
        let a = Analysis::new(DOC);
        let (line, character) = a.lines.position(DOC, DOC.find(needle).unwrap() + delta);
        json!({ "line": line, "character": character })
    }
    fn pos(needle: &str, delta: usize) -> Value {
        json!({ "textDocument": { "uri": URI }, "position": Self::at(needle, delta) })
    }
}

fn start() -> (Client, thread::JoinHandle<()>) {
    let (server, client) = Connection::memory();
    let handle = thread::spawn(move || run(server).unwrap());
    let mut c = Client { conn: client, next: 0 };
    let init = c.request("initialize", json!({ "capabilities": {}, "initializationOptions": { "inlayHints": { "units": false } } }));
    let caps = &init.result.unwrap()["capabilities"];
    assert_eq!(caps["hoverProvider"], true);
    assert_eq!(caps["renameProvider"]["prepareProvider"], true);
    c.notify("initialized", json!({}));
    c.notify("textDocument/didOpen", json!({ "textDocument": { "uri": URI, "languageId": "engines-loop", "version": 1, "text": DOC } }));
    (c, handle)
}

fn stop(mut c: Client, handle: thread::JoinHandle<()>) {
    let r = c.request("shutdown", Value::Null);
    assert!(r.error.is_none());
    c.notify("exit", Value::Null);
    handle.join().unwrap();
}

#[test]
fn a_session_answers_every_request_kind() {
    let (mut c, handle) = start();
    // didOpen ⇒ diagnostics, located in UTF-16 positions.
    let diags = c.next_notification();
    assert_eq!(diags.method, "textDocument/publishDiagnostics");
    let d = &diags.params["diagnostics"][0];
    assert_eq!(d["code"], "unknown-key");
    assert_eq!(d["range"]["start"], Client::at("cutof", 0));
    assert_eq!(d["severity"], 1);

    let hover = c.request("textDocument/hover", Client::pos("offset: 7", 8)).result.unwrap();
    assert!(hover["contents"]["value"].as_str().unwrap().starts_with("**A4** · 5 of D dorian"), "{hover}");

    let items = c.request("textDocument/completion", Client::pos("offset: 7", 8)).result.unwrap();
    assert_eq!(items[0]["label"], "7");
    assert_eq!(items[0]["detail"], "A4 · 5 · D dorian");
    assert_eq!(items[0]["textEdit"]["range"]["start"], Client::at("offset: 7", 8));

    let hints = c.request("textDocument/inlayHint", json!({ "textDocument": { "uri": URI }, "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 99, "character": 0 } } })).result.unwrap();
    let labels: Vec<&str> = hints.as_array().unwrap().iter().map(|h| h["label"].as_str().unwrap()).collect();
    assert!(labels.contains(&"A4") && labels.contains(&"4.0 s loop"), "{labels:?}");

    let lenses = c.request("textDocument/codeLens", json!({ "textDocument": { "uri": URI } })).result.unwrap();
    assert_eq!(lenses[0]["command"]["command"], "engines.solo");
    assert_eq!(lenses[0]["command"]["arguments"], json!([URI, "pad"]));
    assert_eq!(lenses[2]["command"]["command"], "engines.loopBlock");

    // Source actions: the parts library, only when asked for.
    let top = Client::at("transport", 0);
    let source = c.request("textDocument/codeAction", json!({ "textDocument": { "uri": URI }, "range": { "start": top, "end": top }, "context": { "diagnostics": [], "only": ["source"] } })).result.unwrap();
    let parts: Vec<&Value> = source.as_array().unwrap().iter().filter(|x| x["kind"] == "source.addPart").collect();
    assert!(parts.len() >= 4, "{source}");
    assert!(parts.iter().any(|x| x["title"].as_str().unwrap().starts_with("Add part: plucked pulse")));
    let plain = c.request("textDocument/codeAction", json!({ "textDocument": { "uri": URI }, "range": { "start": top, "end": top }, "context": { "diagnostics": [] } })).result.unwrap();
    assert!(plain.as_array().unwrap().iter().all(|x| x["kind"] != "source.addPart"));

    let cutof = Client::at("cutof", 1);
    let fixes = c.request("textDocument/codeAction", json!({ "textDocument": { "uri": URI }, "range": { "start": cutof, "end": cutof }, "context": { "diagnostics": [] } })).result.unwrap();
    assert_eq!(fixes[0]["title"], "Change to `cutoff_hz`");
    assert_eq!(fixes[0]["edit"]["changes"][URI][0]["newText"], "cutoff_hz");

    // ADR 0054 U6: a text through the migrator (the extension's revert to a pre-v0.9 revision).
    let v08 = "{ schema_version: \"0.8\", transport: { bpm: 60, master_steps: 4 }, harmony: { root: \"D\", mode: \"dorian\" }, instruments: { pad: { params: { wave: \"sine\", level: 0.5 } } } }";
    let m = c.request("engines/migrate", json!({ "text": v08 })).result.unwrap();
    assert_eq!(m["changed"], true);
    assert!(m["text"].as_str().unwrap().contains("level_db: -6.02") && m["text"].as_str().unwrap().contains("schema_version: \"0.9\""), "{m}");
    assert_eq!(c.request("engines/migrate", json!({ "text": DOC })).result.unwrap()["changed"], false, "a v0.9 doc comes back as it was");
    assert!(c.request("engines/migrate", json!({ "text": "{ schema_version: \"0.7\" }" })).error.is_some());
    assert!(c.request("engines/migrate", json!({})).error.is_some());

    let def = c.request("textDocument/definition", Client::pos("\"seq\"", 2)).result.unwrap();
    assert_eq!(def["range"]["start"], Client::at("seq: {", 0));
    let refs = c.request("textDocument/references", json!({ "textDocument": { "uri": URI }, "position": Client::at("seq: {", 1), "context": { "includeDeclaration": true } })).result.unwrap();
    assert_eq!(refs.as_array().unwrap().len(), 2);

    let prep = c.request("textDocument/prepareRename", Client::pos("pad", 1)).result.unwrap();
    assert_eq!(prep["placeholder"], "pad");
    let mut rename_params = Client::pos("pad", 1);
    rename_params["newName"] = json!("halo");
    let edit = c.request("textDocument/rename", rename_params.clone()).result.unwrap();
    assert_eq!(edit["changes"][URI].as_array().unwrap().len(), 2);
    rename_params["newName"] = json!("a.b");
    assert!(c.request("textDocument/rename", rename_params).error.is_some());

    let syms = c.request("textDocument/documentSymbol", json!({ "textDocument": { "uri": URI } })).result.unwrap();
    let inst = syms.as_array().unwrap().iter().find(|s| s["name"] == "instruments").unwrap();
    assert_eq!(inst["children"][0]["name"], "pad");

    let steps = c.request("engines/stepRanges", json!({ "textDocument": { "uri": URI } })).result.unwrap();
    assert_eq!(steps["seq"][0]["step"], 0);
    assert_eq!(steps["seq"][0]["range"]["start"], Client::at("{ step: 0", 0));

    // ADR 0040: a JSON path → where it's written (the key, and the whole member).
    let path = |c: &mut Client, path: Value, label: Value| c.request("engines/pathRange", json!({ "textDocument": { "uri": URI }, "path": path, "label": label }));
    let r = path(&mut c, json!(["instruments", "pad"]), Value::Null).result.unwrap();
    assert_eq!((r["keyRange"]["start"].clone(), r["range"]["start"].clone(), r["complete"].clone()), (Client::at("pad:", 0), Client::at("pad:", 0), json!(true)));
    assert_eq!(r["keyRange"]["end"], Client::at("pad:", 3));
    // An array item: the cursor goes on its `{`.
    let r = path(&mut c, json!(["song", "sequence", 0]), json!("base")).result.unwrap();
    assert_eq!(r["keyRange"]["start"], Client::at("{ loops: 2 }", 0));
    // A path that runs out: its deepest prefix, not complete.
    let r = path(&mut c, json!(["instruments", "pad", "params", "nope"]), Value::Null).result.unwrap();
    assert_eq!((r["keyRange"]["start"].clone(), r["complete"].clone()), (Client::at("params:", 0), json!(false)));
    assert!(path(&mut c, json!(["instruments", true]), Value::Null).error.is_some(), "a malformed path is refused");

    // An edit republishes; a fixed doc clears the error.
    c.notify("textDocument/didChange", json!({ "textDocument": { "uri": URI, "version": 2 }, "contentChanges": [ { "text": DOC.replace("cutof_hz", "cutoff_hz") } ] }));
    let diags = c.next_notification();
    assert!(diags.params["diagnostics"].as_array().unwrap().iter().all(|d| d["severity"] != 1), "{}", diags.params);

    assert!(c.request("textDocument/unknownThing", json!({})).error.is_some());

    // A quoted key being typed: the filter text carries the quote so the client keeps the items.
    let typed = DOC.replace("song: {", "safe_bounds: { \"glo\" }, song: {");
    c.notify("textDocument/didChange", json!({ "textDocument": { "uri": URI, "version": 3 }, "contentChanges": [ { "text": typed } ] }));
    let _ = c.next_notification();
    let a = Analysis::new(typed.as_str());
    let (line, character) = a.lines.position(&typed, typed.find("\"glo").unwrap() + 4);
    let items = c.request("textDocument/completion", json!({ "textDocument": { "uri": URI }, "position": { "line": line, "character": character } })).result.unwrap();
    let gain = items.as_array().unwrap().iter().find(|i| i["label"] == "global.master_gain_db").expect("bound path offered");
    assert_eq!(gain["filterText"], "\"global.master_gain_db");
    assert!(gain["textEdit"]["newText"].as_str().unwrap().starts_with("\"global.master_gain_db\": {"), "{gain}");
    stop(c, handle);
}

#[test]
fn utf16_positions_round_trip_through_requests() {
    let (mut c, handle) = start();
    let _ = c.next_notification();
    // A doc with astral characters before the target: the column must count UTF-16 units.
    let text = "{\n  meta: { note: \"🎹🎹 ♭\" }, schema_version: \"0.9\", harmony: { root: \"D\", mode: \"dorian\" },\n}";
    c.notify("textDocument/didChange", json!({ "textDocument": { "uri": URI, "version": 3 }, "contentChanges": [ { "text": text } ] }));
    let _ = c.next_notification();
    let col = text.lines().nth(1).unwrap();
    let byte = col.find("\"dorian\"").unwrap() + 2;
    let utf16 = col[..byte].encode_utf16().count();
    let hover = c.request("textDocument/hover", json!({ "textDocument": { "uri": URI }, "position": { "line": 1, "character": utf16 } })).result.unwrap();
    assert!(hover["contents"]["value"].as_str().unwrap().starts_with("**D dorian**"), "{hover}");
    stop(c, handle);
}

#[test]
fn a_settings_change_refreshes_inlay_hints() {
    let (server, client) = Connection::memory();
    let handle = thread::spawn(move || run(server).unwrap());
    let mut c = Client { conn: client, next: 0 };
    c.request("initialize", json!({ "capabilities": { "workspace": { "inlayHint": { "refreshSupport": true } } } }));
    c.notify("initialized", json!({}));
    c.notify("textDocument/didOpen", json!({ "textDocument": { "uri": URI, "languageId": "engines-loop", "version": 1, "text": DOC } }));
    let _ = c.next_notification();
    // VS Code's settings are camelCase (`engines.inlayHints.loopLength`).
    c.notify("workspace/didChangeConfiguration", json!({ "settings": { "engines": { "inlayHints": { "loopLength": false } } } }));
    let refresh = loop {
        if let Message::Request(r) = c.conn.receiver.recv().unwrap() {
            break r;
        }
    };
    assert_eq!(refresh.method, "workspace/inlayHint/refresh");
    c.conn.sender.send(Message::Response(Response { id: refresh.id, result: Some(Value::Null), error: None })).unwrap();
    let hints = c.request("textDocument/inlayHint", json!({ "textDocument": { "uri": URI }, "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 99, "character": 0 } } })).result.unwrap();
    let labels: Vec<&str> = hints.as_array().unwrap().iter().map(|h| h["label"].as_str().unwrap()).collect();
    assert!(labels.contains(&"A4") && !labels.contains(&"4.0 s loop"), "{labels:?}");
    stop(c, handle);
}
