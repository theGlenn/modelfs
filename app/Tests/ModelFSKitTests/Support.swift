import Foundation
@testable import ModelFSKit

/// `schema/v1` at the repository root.
let schemaDirectory = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()
    .appending(path: "../../../schema/v1")
    .standardizedFileURL

/// One fixture file's contents.
func fixture(_ name: String) throws -> Data {
    try Data(contentsOf: schemaDirectory.appending(path: name))
}

/// Decodes one JSON line for a command whose result is `Result`.
func decodeLine<Result: Decodable & Sendable>(_ data: Data, as _: Result.Type) throws -> EngineMessage<Result> {
    try JSONDecoder().decode(EngineMessage<Result>.self, from: data)
}

/// A fresh temporary directory, removed when the value is released.
final class TemporaryDirectory {
    let url: URL

    init() throws {
        url = FileManager.default.temporaryDirectory
            .appending(path: "modelfs-tests-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
    }

    deinit {
        try? FileManager.default.removeItem(at: url)
    }
}

/// Counts calls across tasks.
actor Counter {
    private(set) var value = 0

    func increment() {
        value += 1
    }
}
