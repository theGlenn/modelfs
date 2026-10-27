import Foundation

/// Coalesces refresh requests into as few runs as possible.
///
/// A pass moves the generation file, appends to the daemon log, and
/// rewrites the journal within moments of each other. Each change asks for
/// a refresh; running one per request would start several engine processes
/// that all read the same thing. Requests made within `debounce` of each
/// other become one run; requests made while a run is in flight become
/// exactly one more run after it, so the last change is never missed.
public actor Refresher {
    private let debounce: Duration
    private let action: @Sendable () async -> Void
    private var scheduled = false
    private var running = false
    private var requested = false

    public init(debounce: Duration = .milliseconds(250), action: @escaping @Sendable () async -> Void) {
        self.debounce = debounce
        self.action = action
    }

    /// Asks for a refresh soon.
    public func request() {
        requested = true
        guard !scheduled, !running else { return }
        scheduled = true
        Task {
            try? await Task.sleep(for: debounce)
            await runRequested()
        }
    }

    /// Runs until no request arrived during the last run.
    private func runRequested() async {
        scheduled = false
        running = true
        while requested {
            requested = false
            await action()
        }
        running = false
    }
}
