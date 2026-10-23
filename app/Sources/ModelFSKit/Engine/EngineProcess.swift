import Foundation
import Synchronization

/// What to run: the engine, its arguments, and its whole environment.
public struct EngineInvocation: Sendable, Equatable {
    public var executable: URL
    public var arguments: [String]
    public var environment: [String: String]

    public init(executable: URL, arguments: [String], environment: [String: String]) {
        self.executable = executable
        self.arguments = arguments
        self.environment = environment
    }
}

/// Why an engine run produced no result.
public enum EngineError: Error, Sendable, Equatable {
    /// No engine binary was found.
    case notFound
    /// The engine could not be started.
    case launchFailed(String)
    /// The engine reported a failure on an error line.
    case failed(EngineFailure)
    /// A line could not be read; the app and engine disagree on the format.
    case unreadable(line: String, reason: String)
    /// The engine exited without a result or error line.
    case noResult(status: Int32, stderr: String)
}

extension EngineError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .notFound:
            "The modeld engine is missing from the app."
        case .launchFailed(let reason):
            "modeld could not start: \(reason)"
        case .failed(let failure):
            failure.message
        case .unreadable(_, let reason):
            "modeld's output could not be read: \(reason)"
        case .noResult(let status, let stderr):
            stderr.isEmpty
                ? "modeld stopped unexpectedly (status \(status))."
                : "modeld stopped unexpectedly (status \(status)): \(stderr)"
        }
    }
}

/// Runs one engine process and decodes its JSON Lines.
///
/// Two rules keep a pass safe whatever the app does:
///
/// - **stdout and stderr are always read to the end**, each by a thread
///   of its own that the caller's cancellation does not reach. A child
///   blocked on a full pipe would stall a pass forever; a child whose
///   reader vanished keeps going, since `--json` makes it ignore write
///   errors.
/// - **Cancelling sends at most one SIGINT, and only before `locked`.**
///   The engine treats one SIGINT as "stop waiting for the lock" and
///   ignores it once it holds the lock, so a swap is never cut short. A
///   second signal would exit at once; it is never sent. Nor is a signal
///   sent before the engine's first line: until then its handler may not be
///   installed, and the default action would kill it.
public enum EngineProcess {
    /// Runs `invocation` and returns its result line's payload.
    ///
    /// `onEvent` hears each event as it arrives, off the main actor. With
    /// `interruptible`, cancelling the calling task asks the engine to
    /// stop (see the type's rules); the call still returns what the engine
    /// reports, which is the `cancelled` failure or, if the pass already
    /// held the lock, its full result.
    ///
    /// `acceptedStatuses` lists the exit codes that come with a result line
    /// worth returning; `rm` exits 1 or 3 after one too.
    ///
    /// - Throws: ``EngineError``.
    public static func run<Result: Decodable & Sendable>(
        _ invocation: EngineInvocation,
        as _: Result.Type = Result.self,
        interruptible: Bool = false,
        acceptedStatuses: Set<Int32> = [0],
        onEvent: @escaping @Sendable (PassEvent) -> Void = { _ in }
    ) async throws(EngineError) -> Result {
        let child = try Child.launch(invocation)
        let outcome = await withTaskCancellationHandler {
            await Task.detached {
                await child.collect(Result.self, accepting: acceptedStatuses, onEvent: onEvent)
            }.value
        } onCancel: {
            if interruptible {
                child.interruptUnlessLocked()
            }
        }
        return try outcome.get()
    }
}

/// A launched engine process and what it said so far.
private final class Child: Sendable {
    private let process: Process
    private let stdout: FileHandle
    private let stderr: FileHandle
    private let exit: AsyncStream<Int32>
    private let state = Mutex(Signals())

    /// What the engine reported so far, and what the caller asked.
    private struct Signals {
        /// The engine wrote a line, so its signal handler is installed.
        var started = false
        var locked = false
        /// The caller asked to interrupt before the engine started.
        var interruptRequested = false
        var interrupted = false

        /// Marks SIGINT as sent if it may be sent now; returns whether.
        mutating func takeInterrupt() -> Bool {
            guard started, !locked, !interrupted else { return false }
            interrupted = true
            return true
        }
    }

    private init(process: Process, stdout: FileHandle, stderr: FileHandle, exit: AsyncStream<Int32>) {
        self.process = process
        self.stdout = stdout
        self.stderr = stderr
        self.exit = exit
    }

    static func launch(_ invocation: EngineInvocation) throws(EngineError) -> Child {
        let process = Process()
        process.executableURL = invocation.executable
        process.arguments = invocation.arguments
        process.environment = invocation.environment
        process.standardInput = FileHandle.nullDevice
        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr
        let (exit, exited) = AsyncStream.makeStream(of: Int32.self, bufferingPolicy: .bufferingNewest(1))
        process.terminationHandler = { finished in
            exited.yield(finished.terminationStatus)
            exited.finish()
        }
        do {
            try process.run()
        } catch {
            throw .launchFailed(error.localizedDescription)
        }
        return Child(
            process: process,
            stdout: stdout.fileHandleForReading,
            stderr: stderr.fileHandleForReading,
            exit: exit
        )
    }

    /// Sends SIGINT once, unless the engine already said it holds the lock.
    ///
    /// Before the engine's first line, the request waits for that line.
    func interruptUnlessLocked() {
        let send = state.withLock { signals in
            signals.interruptRequested = true
            return signals.takeInterrupt()
        }
        if send {
            process.interrupt()
        }
    }

    /// Records that the engine wrote a line; sends a waiting interrupt.
    private func noteLine(locked: Bool) {
        let send = state.withLock { signals in
            signals.started = true
            signals.locked = signals.locked || locked
            return signals.interruptRequested && signals.takeInterrupt()
        }
        if send {
            process.interrupt()
        }
    }

    /// Reads stdout to the end, then waits for the process to exit.
    func collect<Result: Decodable & Sendable>(
        _: Result.Type,
        accepting statuses: Set<Int32>,
        onEvent: @Sendable (PassEvent) -> Void
    ) async -> Swift.Result<Result, EngineError> {
        let stderrLines = PipeReader.lines(of: stderr)
        let stderrTail = Task.detached { await Self.tail(of: stderrLines) }
        let decoder = JSONDecoder()
        var result: Result?
        var failure: EngineError?
        for await line in PipeReader.lines(of: stdout) where !line.isEmpty {
            let message = decode(line, as: Result.self, with: decoder)
            if case .success(.event(.locked)) = message {
                noteLine(locked: true)
            } else {
                noteLine(locked: false)
            }
            switch message {
            case .success(.event(let event)):
                onEvent(event)
            case .success(.result(let payload)):
                result = payload
            case .success(.failure(let reported)):
                failure = .failed(reported)
            case .success(.unknown):
                break
            case .failure(let unreadable):
                failure = failure ?? unreadable
            }
        }
        var status: Int32 = -1
        for await code in exit {
            status = code
        }
        let tail = await stderrTail.value
        if let result, statuses.contains(status) {
            return .success(result)
        }
        return .failure(failure ?? .noResult(status: status, stderr: tail))
    }

    private func decode<Result: Decodable & Sendable>(
        _ line: String,
        as _: Result.Type,
        with decoder: JSONDecoder
    ) -> Swift.Result<EngineMessage<Result>, EngineError> {
        do {
            return .success(try decoder.decode(EngineMessage<Result>.self, from: Data(line.utf8)))
        } catch {
            return .failure(.unreadable(line: line, reason: String(describing: error)))
        }
    }

    /// The last lines of stderr, for error messages.
    private static func tail(of stderr: AsyncStream<String>, keeping limit: Int = 20) async -> String {
        var lines: [String] = []
        for await line in stderr {
            lines.append(line)
            if lines.count > limit {
                lines.removeFirst()
            }
        }
        return lines.joined(separator: "\n")
    }
}

/// Reads a pipe's lines on a thread of its own.
///
/// Foundation's `FileHandle.bytes` runs every handle's blocking reads on one
/// shared serial queue. Reading stderr that way can block the stdout reader
/// until the child exits, and a child blocked writing a full stdout pipe
/// never exits: a deadlock. A thread per pipe, with plain `read(2)`, keeps
/// each pipe draining however the other one behaves.
enum PipeReader {
    /// The lines of `handle` until end of file, without their newlines.
    static func lines(of handle: FileHandle) -> AsyncStream<String> {
        let (lines, output) = AsyncStream.makeStream(of: String.self)
        let descriptor = handle.fileDescriptor
        let thread = Thread {
            // Keeps the handle, and so its descriptor, open while reading.
            withExtendedLifetime(handle) {
                read(descriptor, into: output)
            }
        }
        thread.name = "modeld-pipe-reader"
        thread.start()
        return lines
    }

    private static func read(_ descriptor: Int32, into output: AsyncStream<String>.Continuation) {
        var buffer = [UInt8](repeating: 0, count: 64 * 1024)
        var pending: [UInt8] = []
        while true {
            let count = Darwin.read(descriptor, &buffer, buffer.count)
            if count < 0, errno == EINTR {
                continue
            }
            guard count > 0 else { break }
            // Only the new bytes can hold a newline: earlier ones were scanned.
            var search = pending.count
            pending.append(contentsOf: buffer[..<count])
            var start = 0
            while let newline = pending[search...].firstIndex(of: UInt8(ascii: "\n")) {
                output.yield(String(decoding: pending[start..<newline], as: UTF8.self))
                start = newline + 1
                search = start
            }
            pending.removeSubrange(..<start)
        }
        if !pending.isEmpty {
            output.yield(String(decoding: pending, as: UTF8.self))
        }
        output.finish()
    }
}
