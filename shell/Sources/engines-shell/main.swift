// engines-shell — thin macOS CLI shell over the engines_core C ABI (CE-R12).
//
// Streams a loop doc (or song) live (AVAudioEngine) while the Rust core runs the controller's
// drift / Orrery movement, optionally logging one JSON record per loop.
// Build: `cargo build --release` then `swift build` in shell/.

import AVFoundation
import CEngines
import Foundation

// ── CLI args ─────────────────────────────────────────────────────────────────
struct Args {
    var doc: String?
    var log: String?                 // optional per-loop record log (newline-delimited JSON)
    var loops: Int64 = -1            // <0 = run until Ctrl-C
    var sampleRate: Double = 48_000
    var maxChange: Float? = nil      // nil = the doc's own max_change (else 0.05)
    var seed: UInt64 = 0x5EED
    var play = true                  // --no-audio for a headless run
}

func parseArgs() -> Args {
    var a = Args()
    var it = CommandLine.arguments.dropFirst().makeIterator()
    func next(_ flag: String) -> String {
        guard let v = it.next() else { fail("missing value for \(flag)") }
        return v
    }
    while let arg = it.next() {
        switch arg {
        case "--doc": a.doc = next(arg)
        case "--log": a.log = next(arg)
        case "--loops": a.loops = Int64(next(arg)) ?? -1
        case "--sr": a.sampleRate = Double(next(arg)) ?? 48_000
        case "--max-change": a.maxChange = Float(next(arg))
        case "--seed": a.seed = UInt64(next(arg)) ?? 0x5EED
        case "--no-audio": a.play = false
        case "-h", "--help": usage(); exit(0)
        default: fail("unknown argument: \(arg)")
        }
    }
    if a.doc == nil { usage(); fail("--doc <loop_doc.json> is required") }
    return a
}

func usage() {
    FileHandle.standardError.write(Data("""
    engines-shell — live player over the engines_core C ABI
      --doc PATH         loop document JSON/JSON5 (required)
      --log PATH         per-loop records (newline-delimited JSON) [none]
      --loops N          number of loops; <0 = until Ctrl-C       [-1]
      --sr HZ            sample rate                               [48000]
      --max-change F     max drift step per loop (δ bound)         [the doc's, else 0.05]
      --seed N           drift / Orrery seed                       [0x5EED]
      --no-audio         render without live playback

    """.utf8))
}

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(Data("error: \(msg)\n".utf8))
    exit(2)
}

// ── session lifecycle ────────────────────────────────────────────────────────
let args = parseArgs()
let version = String(cString: engines_version())
FileHandle.standardError.write(Data("engines-shell → engines_core \(version)\n".utf8))

guard let docText = try? String(contentsOfFile: args.doc!, encoding: .utf8) else {
    fail("cannot read \(args.doc!)")
}

var errPtr: UnsafeMutablePointer<CChar>?
let session: OpaquePointer? = docText.withCString { docC in
    // -1 = no override: the session uses the doc's max_change (engines_session_new).
    engines_session_new(docC, args.sampleRate, args.loops, args.maxChange ?? -1, args.seed, nil, &errPtr)
}
guard let session else {
    let msg = errPtr.map { String(cString: $0) } ?? "unknown error"
    if let e = errPtr { engines_string_free(e) }
    fail("session: \(msg)")
}

// optional per-loop record log
var logFile: FileHandle?
if let path = args.log {
    FileManager.default.createFile(atPath: path, contents: nil)
    guard let f = FileHandle(forWritingAtPath: path) else { fail("cannot open \(path)") }
    logFile = f
}

// ── live audio (AVAudioEngine source node pulls from the core ring) ──────────
let engine = AVAudioEngine()
var srcNode: AVAudioSourceNode?
if args.play {
    guard let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: args.sampleRate, channels: 2, interleaved: true) else {
        fail("cannot build audio format")
    }
    let node = AVAudioSourceNode(format: format) { _, _, frameCount, ablPtr in
        let abl = UnsafeMutableAudioBufferListPointer(ablPtr)
        guard let raw = abl[0].mData else { return noErr }
        let out = raw.assumingMemoryBound(to: Float.self)
        let need = Int(frameCount) * 2   // interleaved stereo
        let got = Int(engines_session_pull_audio(session, out, UInt(need)))
        if got < need { for i in got..<need { out[i] = 0 } }  // underrun → silence
        return noErr
    }
    engine.attach(node)
    engine.connect(node, to: engine.mainMixerNode, format: format)
    srcNode = node
    do { try engine.start() } catch { fail("audio engine: \(error)") }
    FileHandle.standardError.write(Data("playing live @ \(Int(args.sampleRate)) Hz; Ctrl-C to stop\n".utf8))
} else {
    FileHandle.standardError.write(Data("rendering (no audio)\n".utf8))
}

// ── record drain + completion, on a background timer ─────────────────────────
var stopRequested = false
let jbufSize = 1 << 16
var jbuf = [CChar](repeating: 0, count: jbufSize)

func drainRecords() {
    while true {
        let n = jbuf.withUnsafeMutableBufferPointer { p in
            Int(engines_session_poll_records_json(session, p.baseAddress, UInt(jbufSize)))
        }
        if n == 0 { break }
        if let f = logFile { jbuf.withUnsafeBytes { f.write(Data(bytes: $0.baseAddress!, count: n)) } }
    }
}

let sigSrc = DispatchSource.makeSignalSource(signal: SIGINT, queue: .main)
signal(SIGINT, SIG_IGN)
sigSrc.setEventHandler { stopRequested = true }
sigSrc.resume()

let timer = DispatchSource.makeTimerSource(queue: .main)
timer.schedule(deadline: .now() + 0.1, repeating: 0.2)
timer.setEventHandler {
    drainRecords()
    if stopRequested { engines_session_stop(session) }
    if stopRequested || engines_session_finished(session) {
        timer.cancel()
        // headless render outpaces playback; let the audio device drain its buffer.
        if args.play { while !engines_session_finished(session) { drainRecords(); usleep(50_000) } }
        drainRecords()
        if args.play { engine.stop() }
        try? logFile?.close()
        engines_session_free(session)
        FileHandle.standardError.write(Data("done\n".utf8))
        exit(0)
    }
}
timer.resume()

dispatchMain()
