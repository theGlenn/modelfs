import CryptoKit
import Foundation
import Testing
@testable import ModelFSKit

/// The real engine (`MODELD_BIN`, a `cargo build` output) against a temp home.
@Suite(.enabled(if: ProcessInfo.processInfo.environment["MODELD_BIN"] != nil, "set MODELD_BIN to run"))
struct EndToEndTests {
    let home: TemporaryDirectory
    let client: EngineClient
    static let modelSize = 2 * 1024 * 1024

    init() throws {
        home = try TemporaryDirectory()
        let models = home.url.appending(path: "models")
        let store = home.url.appending(path: ".modeld")
        try FileManager.default.createDirectory(at: models, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: store, withIntermediateDirectories: true)
        try Data("[scan]\nroots = [\"~/models\"]\n".utf8).write(to: store.appending(path: "config.toml"))
        let weights = Data(repeating: 7, count: Self.modelSize)
        for name in ["a.gguf", "b.gguf"] {
            let path = models.appending(path: name)
            try weights.write(to: path)
            try FileManager.default.setAttributes(
                [.modificationDate: Date(timeIntervalSinceNow: -3600)],
                ofItemAtPath: path.path
            )
        }
        let executable = URL(fileURLWithPath: ProcessInfo.processInfo.environment["MODELD_BIN"] ?? "")
        client = EngineClient(
            executable: executable,
            environment: EngineEnvironment(home: home.url.path, variables: [:], source: .loginShell)
        )
    }

    @Test func scanThenDeduplicateSharesTheCopies() async throws {
        let scan = try await client.pass(dryRun: true) { _ in }
        let before = try await client.inventory()
        let deduplicate = try await client.pass(dryRun: false) { _ in }
        let after = try await client.inventory()

        #expect(scan.dryRun)
        #expect(scan.planned.count == 1)
        #expect(before.totals.pending == UInt64(Self.modelSize))
        #expect(deduplicate.cloned.count == 1)
        #expect(after.models.first?.references.map(\.state) == [.clone, .clone])
        #expect(after.totals.saved == UInt64(Self.modelSize))
        #expect(after.totals.pending == 0)
    }

    @Test @MainActor func appModelScansThenDeduplicates() async throws {
        let client = client
        let model = AppModel(
            home: home.url,
            locateEngine: { client.executable },
            resolveEnvironment: { _ in client.environment }
        )
        await model.start()

        model.runPass(dryRun: true)
        try await waitUntilIdle(model)
        let pendingAfterScan = model.totals.pending
        model.runPass(dryRun: false)
        try await waitUntilIdle(model)

        #expect(model.problem == nil)
        #expect(pendingAfterScan == UInt64(Self.modelSize))
        #expect(model.lastPass?.cloned.count == 1)
        #expect(model.totals.saved == UInt64(Self.modelSize))
        #expect(model.totals.pending == 0)
    }

    @Test @MainActor func appModelRemovesAHuggingFaceModelEverywhere() async throws {
        let weights = Data(repeating: 9, count: Self.modelSize)
        let hex = SHA256.hash(data: weights).map { String(format: "%02x", $0) }.joined()
        let repo = home.url.appending(path: ".cache/huggingface/hub/models--org--tiny")
        let blob = repo.appending(path: "blobs/\(hex)")
        let snapshot = repo.appending(path: "snapshots/abc")
        try FileManager.default.createDirectory(at: blob.deletingLastPathComponent(), withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: snapshot, withIntermediateDirectories: true)
        try weights.write(to: blob)
        try FileManager.default.setAttributes([.modificationDate: Date(timeIntervalSinceNow: -3600)], ofItemAtPath: blob.path)
        try FileManager.default.createSymbolicLink(at: snapshot.appending(path: "model.gguf"), withDestinationURL: blob)
        let client = client
        let model = AppModel(
            home: home.url,
            locateEngine: { client.executable },
            resolveEnvironment: { _ in client.environment }
        )
        await model.start()
        model.runPass(dryRun: true)
        try await waitUntilIdle(model)
        let target = try #require(model.inventory?.models.first { $0.providers == [.huggingface] })

        let options = await model.removalOptions(for: target)
        let option = try #require(options.first)
        guard case .success(let planned) = option.plan else {
            Issue.record("no plan: \(option.plan)")
            return
        }
        let outcome = await model.remove(target, scope: option.scope, plan: planned.plan)

        #expect(options.count == 1, "one app holds it, so one option")
        #expect(planned.plan.removesEverything)
        #expect(planned.plan.refusals.isEmpty)
        guard case .success(let removed) = outcome else {
            Issue.record("removal failed: \(String(describing: outcome))")
            return
        }
        #expect(removed.status == .removed)
        #expect(!FileManager.default.fileExists(atPath: repo.path))
        #expect(model.inventory?.models.contains { $0.digest == target.digest } == false)
        #expect(model.activity == .idle)
    }

    @MainActor
    private func waitUntilIdle(_ model: AppModel, within limit: Duration = .seconds(30)) async throws {
        let deadline = ContinuousClock.now + limit
        repeat {
            try await Task.sleep(for: .milliseconds(50))
        } while model.activity != .idle && ContinuousClock.now < deadline
        try #require(model.activity == .idle, "still busy after \(limit)")
    }

    @Test func doctorAndStatusDecode() async throws {
        let doctor = try await client.doctor()
        let status = try await client.daemonStatus()

        #expect(doctor.roots.map(\.provider) == [.manual])
        #expect(status.lastPass == nil)
    }
}
