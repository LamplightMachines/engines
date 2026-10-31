//! `engines-lsp` — the loop-doc language server (ADR 0037): diagnostics, completion, hover, inlay hints,
//! CodeLens, code actions, go to definition / references / rename for `pieces/<slug>/<slug>.json5`
//! (and any loop doc). Speaks LSP over stdio; the VS Code extension (P7) starts it.
//!
//! ```text
//! engines-lsp            # serve over stdio
//! engines-lsp --version
//! engines-lsp --help
//! ```

fn main() -> std::process::ExitCode {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("engines-lsp {}", env!("CARGO_PKG_VERSION"));
        return std::process::ExitCode::SUCCESS;
    }
    // Before the LSP loop: on a terminal it would only wait for (or, on a closed stdin, fail to get)
    // a client's Content-Length framed messages.
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!("usage: engines-lsp [--version | --help] — the loop-doc language server: LSP over stdio, started by an editor (the Engines VS Code extension)");
        return std::process::ExitCode::SUCCESS;
    }
    let (conn, io) = lsp_server::Connection::stdio();
    let served = engines_authoring::lsp::server::run(conn);
    let joined = io.join();
    match (served, joined) {
        (Ok(()), Ok(())) => std::process::ExitCode::SUCCESS,
        (Err(e), _) => {
            eprintln!("engines-lsp: {e}");
            std::process::ExitCode::FAILURE
        }
        (_, Err(e)) => {
            eprintln!("engines-lsp: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
