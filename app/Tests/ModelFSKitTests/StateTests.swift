import Foundation
import Testing
@testable import ModelFSKit

@Suite struct RefresherTests {
    @Test func requestsCloseTogetherRunOnce() async throws {
        let runs = Counter()
        let refresher = Refresher(debounce: .milliseconds(50)) { await runs.increment() }

        for _ in 0..<10 {
            await refresher.request()
        }
        try await Task.sleep(for: .milliseconds(400))

        #expect(await runs.value == 1)
    }

    @Test func requestsDuringARunCauseExactlyOneMore() async throws {
        let runs = Counter()
        let refresher = Refresher(debounce: .milliseconds(10)) {
            await runs.increment()
            try? await Task.sleep(for: .milliseconds(150))
        }

        await refresher.request()
        try await Task.sleep(for: .milliseconds(60))
        for _ in 0..<5 {
            await refresher.request()
        }
        try await Task.sleep(for: .milliseconds(600))

        #expect(await runs.value == 2)
    }
}

@Suite struct PassProgressTests {
    @Test func cancellableOnlyUntilTheLockIsHeld() {
        var progress = PassProgress(dryRun: false)
        #expect(progress.canCancel)

        progress.apply(.waitingForLock)
        #expect(progress.canCancel)

        progress.apply(.locked)
        #expect(!progress.canCancel)

        progress.apply(.waitingForLock)
        #expect(!progress.canCancel, "a late waiting event must not reopen cancelling")

        progress.apply(.hashing(path: "/m/a.gguf", size: 1))
        #expect(progress.step == .hashing(path: "/m/a.gguf"))
    }
}

@Suite struct ModelSummaryTests {
    func model(states: [ReferenceState], space: Space) -> Model {
        Model(
            digest: "sha256:00",
            name: "Qwen3",
            kind: .model,
            format: "GGUF",
            quant: nil,
            params: nil,
            architecture: nil,
            size: 100,
            stored: StoredCopy(path: "/s", present: true),
            references: states.enumerated().map { index, state in
                Reference(
                    path: "/m/\(index).gguf",
                    provider: index == 0 ? .ollama : .lmstudio,
                    label: nil,
                    source: "lmstudio-community/Qwen3-GGUF",
                    tags: [],
                    state: state
                )
            },
            space: space
        )
    }

    @Test func statusFollowsWhatAPassCouldDo() {
        let pending = Space(onDisk: 200, saved: 0, pending: 100, storeOnly: 0, logical: 200)
        let shared = Space(onDisk: 100, saved: 100, pending: 0, storeOnly: 0, logical: 200)
        let alone = Space(onDisk: 100, saved: 0, pending: 0, storeOnly: 0, logical: 100)
        let orphan = Space(onDisk: 100, saved: 0, pending: 0, storeOnly: 100, logical: 0)

        #expect(model(states: [.clone, .copy], space: pending).status == .canDeduplicate)
        #expect(model(states: [.clone, .clone], space: shared).status == .sharing)
        #expect(model(states: [.clone], space: alone).status == .single)
        #expect(model(states: [.clone, .changed], space: alone).status == .changed)
        #expect(model(states: [.missing], space: orphan).status == .storeOnly)
    }

    @Test func providersAreDistinctPresentAndOrdered() {
        let model = model(states: [.missing, .clone, .clone], space: .zero)

        #expect(model.providers == [.lmstudio])
    }

    @Test func searchMatchesNamesSourcesAndPaths() {
        let model = model(states: [.clone], space: .zero)

        #expect(model.matches("qwen"))
        #expect(model.matches("lmstudio-community"))
        #expect(model.matches("/m/0"))
        #expect(model.matches("  "))
        #expect(!model.matches("gemma"))
    }
}

@Suite struct EngineEnvironmentTests {
    @Test func loginShellOutputIsReadAfterTheMarker() throws {
        let output = Data("Welcome back!\n\(LoginShell.marker)HF_HOME=/Volumes/hf\u{0}PATH=/bin\u{0}EMPTY=\u{0}".utf8)

        let environment = try #require(LoginShell.parse(output))

        #expect(environment["HF_HOME"] == "/Volumes/hf")
        #expect(environment["PATH"] == "/bin")
        #expect(environment["EMPTY"] == "")
    }

    @Test func invalidUTF8InOneVariableKeepsTheOthers() throws {
        var output = Data("\(LoginShell.marker)HF_HOME=/Volumes/hf\u{0}WEIRD=".utf8)
        output.append(contentsOf: [0xFF, 0xFE, 0x00])

        let environment = try #require(LoginShell.parse(output))

        #expect(environment["HF_HOME"] == "/Volumes/hf")
    }

    @Test func failedProbeLeavesTheFoldersUnknown() async throws {
        let home = try TemporaryDirectory()

        let environment = await EngineEnvironment.resolve(home: home.url) { nil }

        #expect(environment.source == .unavailable)
        #expect(!environment.canChangeFiles)
    }

    @Test func outputWithoutTheMarkerIsRejected() {
        #expect(LoginShell.parse(Data("HF_HOME=/x\u{0}".utf8)) == nil)
    }

    @Test func installedServiceWinsOverTheShell() async throws {
        let home = try TemporaryDirectory()
        try writeAgentPlist(in: home.url, environment: ["HOME": home.url.path, "OLLAMA_MODELS": "/Volumes/ollama"])

        let environment = await EngineEnvironment.resolve(home: home.url) { ["HF_HOME": "/from/shell"] }

        #expect(environment.source == .backgroundService)
        #expect(environment.variables == ["OLLAMA_MODELS": "/Volumes/ollama"])
    }

    @Test func withoutAServiceTheShellDecidesAndOnlyModelVariablesPass() async throws {
        let home = try TemporaryDirectory()

        let environment = await EngineEnvironment.resolve(home: home.url) {
            ["HF_HOME": "/from/shell", "SECRET_TOKEN": "x", "HF_HUB_CACHE": ""]
        }

        #expect(environment.source == .loginShell)
        #expect(environment.variables == ["HF_HOME": "/from/shell"])
    }

    @Test func processEnvironmentIsHomePathAndModelVariablesOnly() {
        let environment = EngineEnvironment(
            home: "/Users/me",
            variables: ["HF_HOME": "/Volumes/hf"],
            source: .loginShell
        )

        #expect(
            environment.processEnvironment == [
                "HOME": "/Users/me",
                "PATH": EngineEnvironment.path,
                "HF_HOME": "/Volumes/hf",
            ]
        )
    }

    @Test func realLoginShellAnswersWithinTheTimeout() async {
        let environment = await LoginShell.environment(timeout: .seconds(10))

        #expect(environment?["HOME"] != nil)
    }

    private func writeAgentPlist(in home: URL, environment: [String: String]) throws {
        let agents = home.appending(path: "Library/LaunchAgents")
        try FileManager.default.createDirectory(at: agents, withIntermediateDirectories: true)
        let plist: [String: Any] = [
            "Label": "dev.modeld.daemon",
            "ProgramArguments": ["\(home.path)/.modeld/bin/modeld", "daemon"],
            "EnvironmentVariables": environment,
        ]
        let data = try PropertyListSerialization.data(fromPropertyList: plist, format: .xml, options: 0)
        try data.write(to: agents.appending(path: "dev.modeld.daemon.plist"))
    }
}

@Suite struct StoreWatcherTests {
    @Test func onlyTheFourStoreFilesAreRelevant() {
        let store = "/Users/me/.modeld"

        #expect(StoreWatcher.isRelevant("/Users/me/.modeld/generation", in: store))
        #expect(StoreWatcher.isRelevant("/Users/me/.modeld/config.toml", in: store))
        #expect(!StoreWatcher.isRelevant("/Users/me/.modeld/registry.db-wal", in: store))
        #expect(!StoreWatcher.isRelevant("/Users/me/.modeld/.generation.tmp-42", in: store))
        #expect(!StoreWatcher.isRelevant("/Users/me/.modeld/blobs/generation", in: store))
    }

    @Test func writingTheGenerationFileIsNoticed() async throws {
        let store = try TemporaryDirectory()
        let (changes, notice) = AsyncStream.makeStream(of: Void.self)
        let watcher = StoreWatcher(directory: store.url) { notice.yield() }
        try await Task.sleep(for: .milliseconds(300))

        try Data("1\n".utf8).write(to: store.url.appending(path: "generation"))
        let noticed = await firstValue(of: changes, within: .seconds(5))

        #expect(noticed)
        withExtendedLifetime(watcher) {}
    }

    private func firstValue(of stream: AsyncStream<Void>, within limit: Duration) async -> Bool {
        await withTaskGroup(of: Bool.self) { group in
            group.addTask {
                for await _ in stream {
                    return true
                }
                return false
            }
            group.addTask {
                try? await Task.sleep(for: limit)
                return false
            }
            let first = await group.next() ?? false
            group.cancelAll()
            return first
        }
    }
}
