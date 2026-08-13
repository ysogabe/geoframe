// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "GeoFrameMac",
    platforms: [
        .macOS(.v13),
    ],
    targets: [
        .executableTarget(name: "GeoFrameMac"),
        .testTarget(name: "GeoFrameMacTests", dependencies: ["GeoFrameMac"]),
    ]
)
