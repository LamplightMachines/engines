//! `engines-lsp` over the process boundary: LSP's Content-Length framing on stdio — initialize, open the
//! house reference piece, read its diagnostics, hover, shut down cleanly.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Lsp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Lsp {
    fn start() -> Lsp {
        let mut child = Command::new(env!("CARGO_BIN_EXE_engines-lsp")).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Lsp { child, stdin, stdout }
    }
    fn send(&mut self, msg: Value) {
        let body = msg.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.stdin.flush().unwrap();
    }
    fn recv(&mut self) -> Value {
        let mut len = 0;
        loop {
            let mut line = String::new();
            self.stdout.read_line(&mut line).unwrap();
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(n) = line.strip_prefix("Content-Length: ") {
                len = n.parse().unwrap();
            }
        }
        let mut buf = vec![0; len];
        self.stdout.read_exact(&mut buf).unwrap();
        serde_json::from_slice(&buf).unwrap()
    }
    /// The next message that answers request `id`.
    fn response(&mut self, id: i64) -> Value {
        loop {
            let m = self.recv();
            if m["id"] == id {
                return m;
            }
        }
    }
}

/// `--help` / `-h` answer and exit before the LSP loop (which would read stdin as a client).
#[test]
fn help_prints_a_usage_line_and_exits() {
    for flag in ["--help", "-h"] {
        let out = Command::new(env!("CARGO_BIN_EXE_engines-lsp")).arg(flag).output().unwrap();
        assert!(out.status.success(), "{flag}: {out:?}");
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert!(stdout.starts_with("usage: engines-lsp") && stdout.lines().count() == 1, "{flag}: {stdout:?}");
        assert!(out.stderr.is_empty(), "{flag}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn serves_a_house_style_doc_over_stdio() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lab/tests/fixtures/loop_drone_d_lydian.json5");
    let text = std::fs::read_to_string(&path).unwrap();
    let uri = "file:///fixtures/loop_drone_d_lydian.json5";
    let mut lsp = Lsp::start();
    lsp.send(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "capabilities": {} } }));
    let init = lsp.response(1);
    assert_eq!(init["result"]["capabilities"]["textDocumentSync"], 1, "{init}");
    lsp.send(json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
    lsp.send(json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": { "textDocument": { "uri": uri, "languageId": "engines-loop", "version": 1, "text": text } } }));
    let diags = lsp.recv();
    assert_eq!(diags["method"], "textDocument/publishDiagnostics");
    // The drone checks clean: no errors or warnings (out-of-scale hints are fine).
    let serious: Vec<&Value> = diags["params"]["diagnostics"].as_array().unwrap().iter().filter(|d| d["severity"].as_i64().unwrap() < 3).collect();
    assert!(serious.is_empty(), "{serious:#?}");

    // Hover over the harmony mode: D lydian's notes.
    let (line, col) = text.lines().enumerate().find_map(|(i, l)| l.find("mode: \"lydian\"").map(|c| (i, c + 8))).unwrap();
    lsp.send(json!({ "jsonrpc": "2.0", "id": 2, "method": "textDocument/hover", "params": { "textDocument": { "uri": uri }, "position": { "line": line, "character": col } } }));
    let hover = lsp.response(2);
    assert!(hover["result"]["contents"]["value"].as_str().unwrap().starts_with("**D lydian**: D E F# G# A B C#"), "{hover}");

    lsp.send(json!({ "jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": null }));
    assert!(lsp.response(3)["error"].is_null());
    lsp.send(json!({ "jsonrpc": "2.0", "method": "exit", "params": null }));
    let status = lsp.child.wait().unwrap();
    assert!(status.success());
}
