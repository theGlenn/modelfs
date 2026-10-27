// swift-tools-version: 6.2
// The ModelFS Mac app. `scripts/build-app.sh` builds it together with the
// bundled `modeld` engine; Xcode can open this package for previews and
// debugging (set MODELD_BIN to a built `modeld` in the scheme's environment).

import PackageDescription

let package = Package(
    name: "ModelFS",
    platforms: [
        // `/usr/bin/trash`, `Mutex`, and `defaultLaunchBehavior` need macOS 15.
        .macOS(.v15),
    ],
    products: [
        .executable(name: "ModelFS", targets: ["ModelFS"]),
    ],
    targets: [
        .target(name: "ModelFSKit"),
        .executableTarget(
            name: "ModelFS",
            dependencies: ["ModelFSKit"]
        ),
        .testTarget(
            name: "ModelFSKitTests",
            dependencies: ["ModelFSKit"]
        ),
    ]
)
