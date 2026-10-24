import CoreServices
import Foundation

/// Watches `~/.modeld` for the few files that mean "refresh".
///
/// - `generation`: rewritten by every command that changes the store and
///   by every daemon pass.
/// - `daemon.log`: the service started, stopped, or logged a pass.
/// - `journal.jsonl`: a replacement was journaled or restored.
/// - `config.toml`: the scanned folders changed.
///
/// The registry is deliberately not watched: SQLite creates and deletes
/// `registry.db-wal` on reads too, so every refresh would trigger the next.
public final class StoreWatcher: @unchecked Sendable {
    /// File names inside the store directory that trigger a refresh.
    public static let watchedNames: Set<String> = ["generation", "daemon.log", "journal.jsonl", "config.toml"]

    // Created in `init`, released in `deinit`; never touched in between.
    private let stream: FSEventStreamRef?

    /// Starts watching `directory`, creating it if needed.
    public init(directory: URL, onChange: @escaping @Sendable () -> Void) {
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let target = Target(directory: Self.realPath(of: directory), onChange: onChange)
        stream = Self.start(target)
    }

    deinit {
        guard let stream else { return }
        FSEventStreamStop(stream)
        FSEventStreamInvalidate(stream)
        FSEventStreamRelease(stream)
    }

    /// Whether a path FSEvents reported is a watched file in `directory`.
    ///
    /// `directory` must be a real path, as ``realPath(of:)`` returns:
    /// FSEvents reports `/private/var/...` for `/var/...`.
    public static func isRelevant(_ path: String, in directory: String) -> Bool {
        let url = URL(fileURLWithPath: path)
        return watchedNames.contains(url.lastPathComponent)
            && url.deletingLastPathComponent().path == directory
    }

    /// `url` with every symlink resolved, as FSEvents spells paths.
    public static func realPath(of url: URL) -> String {
        guard let resolved = realpath(url.path, nil) else { return url.path }
        defer { free(resolved) }
        return String(cString: resolved)
    }

    /// What the stream's callback needs; FSEvents owns one reference to it,
    /// so a callback still in flight after `deinit` finds it alive.
    private final class Target: Sendable {
        let directory: String
        let onChange: @Sendable () -> Void

        init(directory: String, onChange: @escaping @Sendable () -> Void) {
            self.directory = directory
            self.onChange = onChange
        }

        func handle(_ paths: [String]) {
            if paths.contains(where: { StoreWatcher.isRelevant($0, in: directory) }) {
                onChange()
            }
        }
    }

    private static func start(_ target: Target) -> FSEventStreamRef? {
        var context = FSEventStreamContext(
            version: 0,
            info: Unmanaged.passRetained(target).toOpaque(),
            retain: nil,
            release: { info in
                guard let info else { return }
                Unmanaged<Target>.fromOpaque(info).release()
            },
            copyDescription: nil
        )
        let callback: FSEventStreamCallback = { _, info, count, paths, _, _ in
            guard let info else { return }
            let target = Unmanaged<Target>.fromOpaque(info).takeUnretainedValue()
            let changed = unsafeBitCast(paths, to: NSArray.self) as? [String] ?? []
            target.handle(Array(changed.prefix(count)))
        }
        let flags = FSEventStreamCreateFlags(
            kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagUseCFTypes | kFSEventStreamCreateFlagNoDefer
        )
        guard
            let stream = FSEventStreamCreate(
                kCFAllocatorDefault,
                callback,
                &context,
                [target.directory] as CFArray,
                FSEventStreamEventId(kFSEventStreamEventIdSinceNow),
                0.1,
                flags
            )
        else {
            Unmanaged.passUnretained(target).release()
            return nil
        }
        FSEventStreamSetDispatchQueue(stream, DispatchQueue(label: "dev.modeld.app.store-watcher"))
        FSEventStreamStart(stream)
        return stream
    }
}
