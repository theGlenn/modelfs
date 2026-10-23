#if DEBUG
import AppKit
import ModelFSKit
import SwiftUI

/// Debug builds only: renders the app's screens to PNG files for reviews.
///
/// Launch with `MODELFS_SNAPSHOT_DIR=<folder>`; once the store has been
/// read, the app draws each screen with the live data into that folder and
/// quits. Views draw themselves (`cacheDisplay`), so no Screen Recording
/// permission is needed.
@MainActor
enum Snapshots {
    static func renderIfRequested(model: AppModel) async {
        guard let folder = ProcessInfo.processInfo.environment["MODELFS_SNAPSHOT_DIR"] else { return }
        while model.inventory == nil || model.daemon == nil || model.doctor == nil {
            try? await Task.sleep(for: .milliseconds(100))
        }
        let shared = model.inventory?.models.first { $0.presentReferences.count > 1 }
        let selected = shared ?? model.inventory?.models.first
        let screens: [(name: String, view: AnyView, size: CGSize)] = [
            ("models", AnyView(MainWindow(section: .models, selection: selected?.id)), CGSize(width: 1280, height: 720)),
            ("duplicates", AnyView(MainWindow(section: .duplicates)), CGSize(width: 1040, height: 660)),
            ("service", AnyView(MainWindow(section: .service)), CGSize(width: 1040, height: 760)),
            ("popover", AnyView(StatusPopover()), CGSize(width: 330, height: 300)),
        ]
        for screen in screens {
            let url = URL(fileURLWithPath: folder).appending(path: "\(screen.name).png")
            await render(screen.view.environment(model), size: screen.size, to: url)
        }
        NSApp.terminate(nil)
    }

    private static func render(_ view: some View, size: CGSize, to url: URL) async {
        let window = NSWindow(
            contentRect: CGRect(origin: CGPoint(x: -20_000, y: -20_000), size: size),
            styleMask: [.titled, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        let host = NSHostingView(
            rootView: view
                .frame(width: size.width, height: size.height)
                .background(Color(nsColor: .windowBackgroundColor))
        )
        window.appearance = NSApp.effectiveAppearance
        window.contentView = host
        window.orderFrontRegardless()
        // Let SwiftUI lay out, load, and settle before drawing.
        try? await Task.sleep(for: .milliseconds(1200))
        host.layoutSubtreeIfNeeded()
        if let bitmap = host.bitmapImageRepForCachingDisplay(in: host.bounds) {
            host.cacheDisplay(in: host.bounds, to: bitmap)
            try? bitmap.representation(using: .png, properties: [:])?.write(to: url)
        }
        window.orderOut(nil)
    }
}
#endif
