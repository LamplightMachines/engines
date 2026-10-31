// swift-tools-version:5.7
import Foundation
import PackageDescription

// The Rust core builds to repo-root/target/release/libengines_core.dylib (run
// `cargo build --release` first). Resolve that directory absolutely from the manifest
// location so `swift build` works regardless of the invocation cwd.
let shellDir = URL(fileURLWithPath: #filePath).deletingLastPathComponent().path
let libDir = URL(fileURLWithPath: shellDir + "/../target/release").standardizedFileURL.path

let package = Package(
    name: "engines-shell",
    platforms: [.macOS(.v11)],
    targets: [
        // C module exposing the cbindgen-generated header (symlinked to the crate's).
        .systemLibrary(name: "CEngines", path: "Sources/CEngines"),
        .executableTarget(
            name: "engines-shell",
            dependencies: ["CEngines"],
            linkerSettings: [
                .linkedFramework("AVFoundation"),
                .unsafeFlags([
                    "-L\(libDir)", "-lengines_core",
                    // embed an rpath so the executable finds libengines_core.dylib at runtime
                    "-Xlinker", "-rpath", "-Xlinker", libDir,
                ]),
            ]
        ),
    ]
)
