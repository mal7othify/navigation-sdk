// swift-tools-version:5.9
import PackageDescription

// Approach B (push). `Sources/BoltFFI/` and `NavCorePush.xcframework` are
// written by `boltffi pack apple` (crates/navcore-boltffi/boltffi.toml) and
// gitignored; `Sources/NavSDKPush/` is the hand-written wrapper. Both live in
// one module, `NavCorePush`, because the generated code is not a separate
// target in BoltFFI's bundled layout.
let package = Package(
    name: "NavCorePush",
    platforms: [.iOS(.v15)],
    products: [
        .library(name: "NavCorePush", targets: ["NavCorePush"]),
    ],
    targets: [
        .binaryTarget(name: "NavCorePushFFI", path: "NavCorePush.xcframework"),
        .target(
            name: "NavCorePush",
            dependencies: ["NavCorePushFFI"],
            path: "Sources"
        ),
        .testTarget(
            name: "NavSDKPushTests",
            dependencies: ["NavCorePush"],
            path: "Tests/NavSDKPushTests",
            resources: [.copy("Fixtures")]
        ),
    ]
)
