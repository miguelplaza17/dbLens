// swift-tools-version: 6.2
import PackageDescription

/// Client libraries from Homebrew: brew install postgresql@16 mariadb-connector-c mongo-c-driver
/// (SQLite comes with macOS.) build.sh copies them into the app.
let libpq = "/opt/homebrew/opt/postgresql@16/lib"
let mariadb = "/opt/homebrew/opt/mariadb-connector-c/lib/mariadb"
/// The MongoDB C driver keeps its headers in versioned folders; update with `brew upgrade`.
let mongoVersion = "2.5.7"
let mongo = "/opt/homebrew/opt/mongo-c-driver"
/// Every target that sees DBLensKit also builds the MongoDB module, so all of them need its headers.
let mongoHeaders: SwiftSetting = .unsafeFlags(["-Xcc", "-I\(mongo)/include/mongoc-\(mongoVersion)",
                                               "-Xcc", "-I\(mongo)/include/bson-\(mongoVersion)"])

let package = Package(
    name: "dbLens",
    platforms: [.macOS(.v26)],
    dependencies: [
        // Updates: checks the appcast on GitHub Releases, downloads, verifies, and installs.
        .package(url: "https://github.com/sparkle-project/Sparkle", exact: "2.10.1"),
    ],
    targets: [
        .systemLibrary(name: "CLibPQ", path: "Sources/CLibPQ"),
        .systemLibrary(name: "CMariaDB", path: "Sources/CMariaDB"),
        .systemLibrary(name: "CMongoC", path: "Sources/CMongoC"),
        // The Postgres client and URL rules, with no SwiftUI, so tests run without the app.
        .target(
            name: "DBLensKit",
            dependencies: ["CLibPQ", "CMariaDB", "CMongoC"],
            swiftSettings: [mongoHeaders],
            linkerSettings: [.unsafeFlags(["-L\(libpq)", "-L\(mariadb)", "-L\(mongo)/lib"]), .linkedLibrary("sqlite3")]
        ),
        .executableTarget(
            name: "dbLens",
            dependencies: ["DBLensKit", .product(name: "Sparkle", package: "Sparkle")],
            // App code runs on the main thread unless you say otherwise.
            swiftSettings: [.defaultIsolation(MainActor.self), mongoHeaders]
        ),
        .testTarget(name: "DBLensKitTests", dependencies: ["DBLensKit"], swiftSettings: [mongoHeaders]),
    ]
)
