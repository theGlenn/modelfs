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

/// A latch tasks wait on until it opens; it stays open.
actor Gate {
    private var isOpen = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        guard !isOpen else { return }
        await withCheckedContinuation { waiters.append($0) }
    }

    func open() {
        isOpen = true
        for waiter in waiters {
            waiter.resume()
        }
        waiters.removeAll()
    }
}

/// Polls `condition` until it holds; fails after `limit`, so slow CI
/// machines wait longer instead of failing on a fixed sleep.
func waitUntil(
    within limit: Duration = .seconds(10),
    _ condition: @Sendable () async -> Bool
) async throws {
    let deadline = ContinuousClock.now + limit
    while !(await condition()) {
        guard ContinuousClock.now < deadline else {
            throw WaitTimedOut()
        }
        try await Task.sleep(for: .milliseconds(10))
    }
}

struct WaitTimedOut: Error {}
