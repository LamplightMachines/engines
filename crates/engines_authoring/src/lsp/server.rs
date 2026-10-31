//! The LSP edge: JSON-RPC over a [`Connection`] (stdio in `engines-lsp`, in-memory in tests), full-text
//! sync, and conversion between byte spans and LSP positions (UTF-16). Each request is answered
//! from a fresh [`Analysis`] of the document's current text — loop docs are small, so analysing on
//! every change (parse + strict check + polymeter) costs about a millisecond.

use super::actions::{actions, add_part_actions, ActionKind};
use super::complete::{complete, ItemKind};
use super::diagnostics::{diagnostics, Level};
use super::hover::hover;
use super::inlay::{inlays, InlayConfig, InlayKind};
use super::lens::lenses;
use super::navigate::{definition, path_span, prepare_rename, references, rename};
use super::steps::step_spans;
use super::Analysis;
use crate::cst::path::Seg;
use crate::cst::{Node, Span};
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types as lt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::error::Error;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// The capabilities `engines-lsp` announces.
pub fn capabilities() -> lt::ServerCapabilities {
    lt::ServerCapabilities {
        text_document_sync: Some(lt::TextDocumentSyncCapability::Kind(lt::TextDocumentSyncKind::FULL)),
        hover_provider: Some(lt::HoverProviderCapability::Simple(true)),
        completion_provider: Some(lt::CompletionOptions { trigger_characters: Some(vec!["\"".into(), ".".into(), ":".into()]), ..Default::default() }),
        definition_provider: Some(lt::OneOf::Left(true)),
        references_provider: Some(lt::OneOf::Left(true)),
        rename_provider: Some(lt::OneOf::Right(lt::RenameOptions { prepare_provider: Some(true), work_done_progress_options: Default::default() })),
        code_action_provider: Some(lt::CodeActionProviderCapability::Options(lt::CodeActionOptions {
            code_action_kinds: Some(vec![lt::CodeActionKind::QUICKFIX, lt::CodeActionKind::REFACTOR_REWRITE, ADD_PART]),
            ..Default::default()
        })),
        code_lens_provider: Some(lt::CodeLensOptions { resolve_provider: Some(false) }),
        inlay_hint_provider: Some(lt::OneOf::Left(true)),
        document_symbol_provider: Some(lt::OneOf::Left(true)),
        ..Default::default()
    }
}

/// The kind of the **Add part: …** source actions (ADR 0039).
pub const ADD_PART: lt::CodeActionKind = lt::CodeActionKind::new("source.addPart");

/// Serve one client until it shuts down.
pub fn run(conn: Connection) -> Result<()> {
    let init = conn.initialize(serde_json::to_value(capabilities())?)?;
    let refresh = init.pointer("/capabilities/workspace/inlayHint/refreshSupport").and_then(Value::as_bool).unwrap_or(false);
    let mut server = Server { docs: HashMap::new(), inlay: InlayConfig::default(), inlay_refresh: refresh.then_some(0) };
    if let Some(cfg) = init.pointer("/initializationOptions/inlayHints") {
        server.inlay = serde_json::from_value(cfg.clone()).unwrap_or_default();
    }
    for msg in &conn.receiver {
        match msg {
            Message::Request(req) => {
                if conn.handle_shutdown(&req)? {
                    return Ok(());
                }
                conn.sender.send(Message::Response(server.request(req)))?;
            }
            Message::Notification(n) => {
                for out in server.notification(n) {
                    conn.sender.send(out)?;
                }
            }
            Message::Response(_) => {}
        }
    }
    Ok(())
}

struct Server {
    /// uri → analysed current text.
    docs: HashMap<String, Analysis>,
    inlay: InlayConfig,
    /// `Some(requests sent)` when the client re-asks for inlay hints on `workspace/inlayHint/refresh`
    /// (sent after a settings change, so toggling a hint kind shows at once).
    inlay_refresh: Option<u32>,
}

fn pos(a: &Analysis, offset: usize) -> lt::Position {
    let (line, character) = a.lines.position(&a.text, offset);
    lt::Position { line, character }
}

fn range(a: &Analysis, s: Span) -> lt::Range {
    lt::Range { start: pos(a, s.start), end: pos(a, s.end) }
}

fn offset(a: &Analysis, p: lt::Position) -> usize {
    a.lines.offset(&a.text, p.line, p.character)
}

fn span(a: &Analysis, r: lt::Range) -> Span {
    Span::new(offset(a, r.start), offset(a, r.end))
}

fn edits(a: &Analysis, uri: &lt::Uri, edits: &[crate::cst::edit::TextEdit]) -> lt::WorkspaceEdit {
    let list = edits.iter().map(|e| lt::TextEdit { range: range(a, e.span), new_text: e.text.clone() }).collect();
    lt::WorkspaceEdit { changes: Some(HashMap::from([(uri.clone(), list)])), ..Default::default() }
}

impl Server {
    fn notification(&mut self, n: Notification) -> Vec<Message> {
        match n.method.as_str() {
            "textDocument/didOpen" => {
                let Ok(p) = serde_json::from_value::<lt::DidOpenTextDocumentParams>(n.params) else { return vec![] };
                self.update(p.text_document.uri, p.text_document.text)
            }
            "textDocument/didChange" => {
                let Ok(p) = serde_json::from_value::<lt::DidChangeTextDocumentParams>(n.params) else { return vec![] };
                match p.content_changes.into_iter().last() {
                    Some(change) => self.update(p.text_document.uri, change.text),
                    None => vec![],
                }
            }
            "textDocument/didClose" => {
                let Ok(p) = serde_json::from_value::<lt::DidCloseTextDocumentParams>(n.params) else { return vec![] };
                self.docs.remove(p.text_document.uri.as_str());
                vec![Message::Notification(publish(&p.text_document.uri, vec![]))]
            }
            "workspace/didChangeConfiguration" => {
                let Some(cfg) = n.params.pointer("/settings/engines/inlayHints").or(n.params.pointer("/settings/inlayHints")) else { return vec![] };
                self.inlay = serde_json::from_value(cfg.clone()).unwrap_or(self.inlay);
                let Some(sent) = self.inlay_refresh.as_mut() else { return vec![] };
                *sent += 1;
                vec![Message::Request(Request { id: RequestId::from(format!("engines-inlay-refresh-{sent}")), method: "workspace/inlayHint/refresh".into(), params: Value::Null })]
            }
            _ => vec![],
        }
    }

    fn update(&mut self, uri: lt::Uri, text: String) -> Vec<Message> {
        let a = Analysis::new(text).with_fallback(self.docs.get(uri.as_str()));
        let diags = diagnostics(&a)
            .into_iter()
            .map(|d| lt::Diagnostic {
                range: range(&a, d.span),
                severity: Some(match d.level {
                    Level::Error => lt::DiagnosticSeverity::ERROR,
                    Level::Warning => lt::DiagnosticSeverity::WARNING,
                    Level::Hint => lt::DiagnosticSeverity::HINT,
                }),
                code: Some(lt::NumberOrString::String(d.code)),
                source: Some("engines".into()),
                message: d.message,
                ..Default::default()
            })
            .collect();
        self.docs.insert(uri.as_str().to_string(), a);
        vec![Message::Notification(publish(&uri, diags))]
    }

    fn request(&mut self, req: Request) -> Response {
        let id = req.id.clone();
        match self.dispatch(req) {
            Ok(v) => Response { id, result: Some(v), error: None },
            Err(Failure(code, message)) => Response::new_err(id, code, message),
        }
    }

    fn doc(&self, uri: &lt::Uri) -> std::result::Result<&Analysis, Failure> {
        self.docs.get(uri.as_str()).ok_or_else(|| Failure(ErrorCode::InvalidParams as i32, format!("unknown document {}", uri.as_str())))
    }

    fn dispatch(&mut self, req: Request) -> std::result::Result<Value, Failure> {
        let p = req.params;
        let ok = |v: Value| Ok::<Value, Failure>(v);
        match req.method.as_str() {
            "textDocument/hover" => {
                let p: lt::HoverParams = parse(p)?;
                let a = self.doc(&p.text_document_position_params.text_document.uri)?;
                let h = hover(a, offset(a, p.text_document_position_params.position));
                ok(json!(h.map(|h| lt::Hover {
                    contents: lt::HoverContents::Markup(lt::MarkupContent { kind: lt::MarkupKind::Markdown, value: h.markdown }),
                    range: Some(range(a, h.span)),
                })))
            }
            "textDocument/completion" => {
                let p: lt::CompletionParams = parse(p)?;
                let a = self.doc(&p.text_document_position.text_document.uri)?;
                let items: Vec<lt::CompletionItem> = complete(a, offset(a, p.text_document_position.position))
                    .into_iter()
                    .map(|i| lt::CompletionItem {
                        filter_text: Some(filter_text(a, &i)),
                        kind: Some(match i.kind {
                            ItemKind::Key => lt::CompletionItemKind::PROPERTY,
                            ItemKind::Value => lt::CompletionItemKind::ENUM_MEMBER,
                            ItemKind::Id => lt::CompletionItemKind::REFERENCE,
                            ItemKind::Note => lt::CompletionItemKind::CONSTANT,
                        }),
                        detail: i.detail,
                        documentation: i.doc.map(|d| lt::Documentation::MarkupContent(lt::MarkupContent { kind: lt::MarkupKind::Markdown, value: d })),
                        sort_text: Some(i.sort),
                        insert_text_format: Some(if i.snippet { lt::InsertTextFormat::SNIPPET } else { lt::InsertTextFormat::PLAIN_TEXT }),
                        text_edit: Some(lt::CompletionTextEdit::Edit(lt::TextEdit { range: range(a, i.replace), new_text: i.insert })),
                        label: i.label,
                        ..Default::default()
                    })
                    .collect();
                ok(json!(items))
            }
            "textDocument/inlayHint" => {
                let p: lt::InlayHintParams = parse(p)?;
                let a = self.doc(&p.text_document.uri)?;
                let hints: Vec<lt::InlayHint> = inlays(a, span(a, p.range), &self.inlay)
                    .into_iter()
                    .map(|h| lt::InlayHint {
                        position: pos(a, h.offset),
                        label: lt::InlayHintLabel::String(h.label),
                        kind: Some(match h.kind {
                            InlayKind::Note | InlayKind::Unit => lt::InlayHintKind::TYPE,
                            InlayKind::Time | InlayKind::Cycle => lt::InlayHintKind::PARAMETER,
                        }),
                        text_edits: None,
                        tooltip: h.tooltip.map(lt::InlayHintTooltip::String),
                        padding_left: Some(true),
                        padding_right: None,
                        data: None,
                    })
                    .collect();
                ok(json!(hints))
            }
            "textDocument/codeLens" => {
                let p: lt::CodeLensParams = parse(p)?;
                let uri = p.text_document.uri;
                let a = self.doc(&uri)?;
                let out: Vec<lt::CodeLens> = lenses(a)
                    .into_iter()
                    .map(|l| lt::CodeLens {
                        range: range(a, l.span),
                        command: Some(lt::Command { title: l.title, command: l.command, arguments: Some(std::iter::once(json!(uri.as_str())).chain(l.args).collect()) }),
                        data: None,
                    })
                    .collect();
                ok(json!(out))
            }
            "textDocument/codeAction" => {
                let p: lt::CodeActionParams = parse(p)?;
                let uri = p.text_document.uri;
                let a = self.doc(&uri)?;
                // Parts only when asked for (the Source Action menu / the extension's command): each
                // is a trial merge + check, too much for every cursor move.
                let wants_parts = p.context.only.as_ref().is_some_and(|only| only.iter().any(|k| ADD_PART.as_str().starts_with(k.as_str())));
                let mut found = actions(a, span(a, p.range));
                if wants_parts {
                    found.extend(add_part_actions(a, offset(a, p.range.start)));
                }
                let out: Vec<lt::CodeActionOrCommand> = found
                    .into_iter()
                    .map(|x| {
                        lt::CodeActionOrCommand::CodeAction(lt::CodeAction {
                            title: x.title,
                            kind: Some(match x.kind {
                                ActionKind::QuickFix => lt::CodeActionKind::QUICKFIX,
                                ActionKind::Refactor => lt::CodeActionKind::REFACTOR_REWRITE,
                                ActionKind::AddPart => ADD_PART,
                            }),
                            diagnostics: x.fixes.map(|d| {
                                vec![lt::Diagnostic { range: range(a, d.span), code: Some(lt::NumberOrString::String(d.code)), source: Some("engines".into()), message: d.message, ..Default::default() }]
                            }),
                            edit: Some(edits(a, &uri, &x.edits)),
                            is_preferred: x.preferred.then_some(true),
                            ..Default::default()
                        })
                    })
                    .collect();
                ok(json!(out))
            }
            "textDocument/definition" => {
                let p: lt::GotoDefinitionParams = parse(p)?;
                let uri = p.text_document_position_params.text_document.uri;
                let a = self.doc(&uri)?;
                let target = definition(a, offset(a, p.text_document_position_params.position));
                ok(json!(target.map(|s| lt::GotoDefinitionResponse::Scalar(lt::Location { uri: uri.clone(), range: range(a, s) }))))
            }
            "textDocument/references" => {
                let p: lt::ReferenceParams = parse(p)?;
                let uri = p.text_document_position.text_document.uri;
                let a = self.doc(&uri)?;
                let locs: Vec<lt::Location> = references(a, offset(a, p.text_document_position.position), p.context.include_declaration)
                    .into_iter()
                    .map(|s| lt::Location { uri: uri.clone(), range: range(a, s) })
                    .collect();
                ok(json!(locs))
            }
            "textDocument/prepareRename" => {
                let p: lt::TextDocumentPositionParams = parse(p)?;
                let a = self.doc(&p.text_document.uri)?;
                let r = prepare_rename(a, offset(a, p.position));
                ok(json!(r.map(|(s, name)| lt::PrepareRenameResponse::RangeWithPlaceholder { range: range(a, s), placeholder: name })))
            }
            "textDocument/rename" => {
                let p: lt::RenameParams = parse(p)?;
                let uri = p.text_document_position.text_document.uri;
                let a = self.doc(&uri)?;
                match rename(a, offset(a, p.text_document_position.position), &p.new_name) {
                    Ok(e) => ok(json!(edits(a, &uri, &e))),
                    Err(msg) => Err(Failure(ErrorCode::RequestFailed as i32, msg)),
                }
            }
            "textDocument/documentSymbol" => {
                let p: lt::DocumentSymbolParams = parse(p)?;
                let a = self.doc(&p.text_document.uri)?;
                ok(json!(symbols(a)))
            }
            // ADR 0038: `{ "<modulator>": [{ step, range }] }` — the extension highlights the step
            // `engines-live` reports playing.
            "engines/stepRanges" => {
                let p: lt::TextDocumentIdentifier = parse(p.get("textDocument").cloned().unwrap_or(Value::Null))?;
                let a = self.doc(&p.uri)?;
                let out: serde_json::Map<String, Value> = step_spans(a)
                    .into_iter()
                    .map(|(id, steps)| (id, steps.into_iter().map(|(step, s)| json!({ "step": step, "range": range(a, s) })).collect()))
                    .collect();
                ok(Value::Object(out))
            }
            // ADR 0040: `{ textDocument, path: (string | number)[], label? }` → `{ range, keyRange,
            // complete }` (or `null` for an empty doc) — the live panel's jump-to-source.
            "engines/pathRange" => {
                let doc: lt::TextDocumentIdentifier = parse(p.get("textDocument").cloned().unwrap_or(Value::Null))?;
                let segs: Option<Vec<Seg>> = p.get("path").and_then(Value::as_array).and_then(|xs| {
                    xs.iter()
                        .map(|x| match x {
                            Value::String(k) => Some(Seg::Key(k.clone())),
                            Value::Number(n) => n.as_u64().map(|i| Seg::Index(i as usize)),
                            _ => None,
                        })
                        .collect()
                });
                let Some(segs) = segs else {
                    return Err(Failure(ErrorCode::InvalidParams as i32, "`path` must be an array of keys (strings) and indices (numbers)".into()));
                };
                let a = self.doc(&doc.uri)?;
                ok(match path_span(a, &segs, p.get("label").and_then(Value::as_str)) {
                    Some(ps) => json!({ "range": range(a, ps.span), "keyRange": range(a, ps.key), "complete": ps.complete }),
                    None => Value::Null,
                })
            }
            // ADR 0054 U6: `{ text }` → `{ text, changed, notes }` — a doc through `engines-migrate`'s
            // conversion (the extension's revert to a pre-v0.9 history revision); a v0.9 doc comes
            // back unchanged, any other version is refused.
            "engines/migrate" => {
                let Some(text) = p.get("text").and_then(Value::as_str) else {
                    return Err(Failure(ErrorCode::InvalidParams as i32, "`text` must be the doc's text".into()));
                };
                match crate::migrate::migrate(text) {
                    Ok(m) => ok(json!({ "text": m.text, "changed": !m.unchanged, "notes": m.notes })),
                    Err(e) => Err(Failure(ErrorCode::RequestFailed as i32, e)),
                }
            }
            other => Err(Failure(ErrorCode::MethodNotFound as i32, format!("unhandled method {other}"))),
        }
    }
}

/// What the client matches the typed text (the `replace` range) against: a plain item's insert text
/// (already quoted where the value is a string); a snippet's label — quoted when the typed text
/// opens with a quote (`"glo` must match `"global.master_gain_db"`).
fn filter_text(a: &Analysis, i: &super::complete::Item) -> String {
    if !i.snippet {
        return i.insert.clone();
    }
    match i.replace.text(&a.text).chars().next() {
        Some(q @ ('"' | '\'')) => format!("{q}{}", i.label),
        _ => i.label.clone(),
    }
}

struct Failure(i32, String);

fn parse<T: serde::de::DeserializeOwned>(v: Value) -> std::result::Result<T, Failure> {
    serde_json::from_value(v).map_err(|e| Failure(ErrorCode::InvalidParams as i32, e.to_string()))
}

fn publish(uri: &lt::Uri, diagnostics: Vec<lt::Diagnostic>) -> Notification {
    Notification::new("textDocument/publishDiagnostics".into(), lt::PublishDiagnosticsParams { uri: uri.clone(), diagnostics, version: None })
}

/// The outline: each top-level section, with its ids (instruments, modulators, routes, …) as
/// children.
#[allow(deprecated)] // `DocumentSymbol::deprecated` must still be filled in
fn symbols(a: &Analysis) -> Vec<lt::DocumentSymbol> {
    let Some(root) = a.root() else { return vec![] };
    let sym = |name: String, detail: Option<String>, kind, full: Span, sel: Span, children: Option<Vec<lt::DocumentSymbol>>| lt::DocumentSymbol {
        name,
        detail,
        kind,
        tags: None,
        deprecated: None,
        range: range(a, full),
        selection_range: range(a, sel),
        children,
    };
    root.members()
        .iter()
        .map(|m| {
            let full = m.span();
            let id_map = matches!(m.key.name.as_str(), "instruments" | "modulators" | "matrix" | "buses" | "sequence_pools");
            let children = id_map.then(|| {
                m.value
                    .as_ref()
                    .map(Node::members)
                    .unwrap_or_default()
                    .iter()
                    .map(|c| {
                        let detail = c.value.as_ref().and_then(|v| v.get("type")).and_then(Node::as_str).map(str::to_string);
                        sym(c.key.name.clone(), detail, lt::SymbolKind::OBJECT, c.span(), c.key.span, None)
                    })
                    .collect()
            });
            sym(m.key.name.clone(), None, if id_map { lt::SymbolKind::NAMESPACE } else { lt::SymbolKind::PROPERTY }, full, m.key.span, children)
        })
        .collect()
}

#[doc(hidden)]
pub fn request(id: i32, method: &str, params: Value) -> Message {
    Message::Request(Request { id: RequestId::from(id), method: method.into(), params })
}

#[cfg(test)]
mod tests;
