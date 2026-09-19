// swift-tools-version: 6.1
import PackageDescription

let package = Package(
    name: "firecrab-micromanager-macos",
    platforms: [
        .macOS(.v15),
    ],
    products: [
        .executable(
            name: "firecrab-micromanager-macos",
            targets: ["FirecrabMicroManager"]
        ),
    ],
    targets: [
        .executableTarget(
            name: "FirecrabMicroManager",
            path: "Sources/FirecrabMicroManager"
        ),
        .testTarget(
            name: "FirecrabMicroManagerTests",
            dependencies: ["FirecrabMicroManager"],
            path: "Tests/FirecrabMicroManagerTests"
        ),
    ]
)
