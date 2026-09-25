// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "NavSDK",
    platforms: [.iOS(.v15)],
    products: [
        .library(name: "NavSDK", targets: ["NavSDK"]),
    ],
    targets: [
        // Rust core + UniFFI C header, built by scripts/build-ios.sh.
        .binaryTarget(name: "NavCoreFFI", path: "NavCoreFFI.xcframework"),
        // Generated Swift bindings (Sources/NavSDK/Generated, gitignored) + wrapper.
        .target(
            name: "NavSDK",
            dependencies: ["NavCoreFFI"],
            path: "Sources/NavSDK"
        ),
        .testTarget(
            name: "NavSDKTests",
            dependencies: ["NavSDK"],
            path: "Tests/NavSDKTests",
            resources: [.copy("Fixtures")]
        ),
    ]
)
