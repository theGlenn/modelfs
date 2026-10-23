import Foundation

/// The environment every engine run gets.
///
/// A Mac app does not inherit the shell's environment, yet `HF_HOME`,
/// `HF_HUB_CACHE`, and `OLLAMA_MODELS` decide which folders modeld scans.
/// Running a pass with the wrong folders would be harmful, not just
/// incomplete: a complete pass prunes references it cannot see, so every
/// copy under a custom folder would be forgotten.
///
/// The variables come from the installed background service first, so the
/// app scans exactly what the daemon scans. Without a service, they come
/// from a login shell, the way a terminal would see them. The engine also
/// gets `HOME` and a fixed `PATH`, and nothing else.
public struct EngineEnvironment: Sendable, Equatable {
    /// Variables that move model folders.
    public static let modelVariables = ["HF_HOME", "HF_HUB_CACHE", "OLLAMA_MODELS"]

    /// The engine only runs system tools, by absolute path.
    public static let path = "/usr/bin:/bin:/usr/sbin:/sbin"

    /// Where the model-folder variables came from.
    public enum Source: Sendable, Equatable {
        /// The installed background service's launchd plist.
        case backgroundService
        /// A login shell, as a terminal would see them.
        case loginShell
        /// Neither was readable; modeld's default folders apply.
        case defaults
    }

    public var home: String
    /// The model-folder variables that are set, such as `HF_HOME`.
    public var variables: [String: String]
    public var source: Source

    public init(home: String, variables: [String: String], source: Source) {
        self.home = home
        self.variables = variables
        self.source = source
    }

    /// The complete environment for an engine process.
    public var processEnvironment: [String: String] {
        variables.merging(["HOME": home, "PATH": Self.path]) { _, fixed in fixed }
    }

    /// Resolves the variables: the service's plist first, else `probe`.
    ///
    /// `probe` runs a login shell; tests pass a stub.
    public static func resolve(
        home: URL,
        probe: @Sendable () async -> [String: String]? = { await LoginShell.environment() }
    ) async -> EngineEnvironment {
        if let fromService = serviceVariables(home: home) {
            return EngineEnvironment(home: home.path, variables: fromService, source: .backgroundService)
        }
        if let fromShell = await probe() {
            return EngineEnvironment(
                home: home.path,
                variables: modelOnly(fromShell),
                source: .loginShell
            )
        }
        return EngineEnvironment(home: home.path, variables: [:], source: .defaults)
    }

    /// The model-folder variables in the installed service's plist.
    ///
    /// `nil` when no service is installed. An installed service without
    /// those variables yields an empty map: it scans the default folders,
    /// so the app must too.
    public static func serviceVariables(home: URL) -> [String: String]? {
        let plist = home.appending(path: "Library/LaunchAgents/dev.modeld.daemon.plist")
        guard
            let data = try? Data(contentsOf: plist),
            let root = try? PropertyListSerialization.propertyList(from: data, format: nil),
            let agent = root as? [String: Any]
        else {
            return nil
        }
        let environment = agent["EnvironmentVariables"] as? [String: String] ?? [:]
        return modelOnly(environment)
    }

    private static func modelOnly(_ environment: [String: String]) -> [String: String] {
        environment.filter { modelVariables.contains($0.key) && !$0.value.isEmpty }
    }
}

/// Reads environment variables the way the user's terminal sees them.
public enum LoginShell {
    /// Printed before the environment, so shell start-up chatter is skipped.
    static let marker = "__MODELFS_ENV_7f3a__"

    /// Runs the user's login shell and reads its environment.
    ///
    /// Interactive too (`-i`), since many people export variables from
    /// `.zshrc`. Returns `nil` if the shell fails or takes over `timeout`.
    /// Output is read without waiting for end-of-file: a background job a
    /// dotfile starts may keep the pipe open long after the shell exits.
    public static func environment(timeout: Duration = .seconds(5)) async -> [String: String]? {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: userShell())
        process.arguments = ["-l", "-i", "-c", "printf '%s' '\(marker)'; /usr/bin/env -0"]
        process.standardInput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        let output = Pipe()
        process.standardOutput = output
        do {
            try process.run()
        } catch {
            return nil
        }
        let reader = output.fileHandleForReading.fileDescriptor
        _ = fcntl(reader, F_SETFL, fcntl(reader, F_GETFL) | O_NONBLOCK)
        let deadline = ContinuousClock.now + timeout
        var data = Data()
        var exited = false
        while true {
            let drained = drain(reader, into: &data)
            if drained || exited {
                break
            }
            exited = !process.isRunning
            if ContinuousClock.now >= deadline {
                kill(process.processIdentifier, SIGKILL)
                return nil
            }
            if !exited {
                try? await Task.sleep(for: .milliseconds(20))
            }
        }
        process.waitUntilExit()
        guard process.terminationReason == .exit, process.terminationStatus == 0 else {
            return nil
        }
        return parse(data)
    }

    /// Reads everything available now; returns whether end-of-file was hit.
    private static func drain(_ descriptor: Int32, into data: inout Data) -> Bool {
        var buffer = [UInt8](repeating: 0, count: 64 * 1024)
        while true {
            let count = read(descriptor, &buffer, buffer.count)
            if count > 0 {
                data.append(contentsOf: buffer[..<count])
            } else {
                return count == 0
            }
        }
    }

    /// Parses `env -0` output that follows the marker.
    static func parse(_ output: Data) -> [String: String]? {
        guard
            let text = String(data: output, encoding: .utf8),
            let start = text.range(of: marker)
        else {
            return nil
        }
        var environment: [String: String] = [:]
        for entry in text[start.upperBound...].split(separator: "\u{0}") {
            guard let equals = entry.firstIndex(of: "=") else { continue }
            environment[String(entry[..<equals])] = String(entry[entry.index(after: equals)...])
        }
        return environment
    }

    /// The user's login shell from the account database, else zsh.
    private static func userShell() -> String {
        guard let entry = getpwuid(getuid()), let shell = entry.pointee.pw_shell else {
            return "/bin/zsh"
        }
        return String(cString: shell)
    }
}
