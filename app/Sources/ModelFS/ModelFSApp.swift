import AppKit
import ModelFSKit
import SwiftUI

/// Window identifiers used with `openWindow`.
enum WindowID {
    static let main = "main"
}

extension Notification.Name {
    /// Asks the menu bar label, which can open windows, to show the main one.
    static let showMainWindow = Notification.Name("dev.modeld.app.showMainWindow")
}

@main
struct ModelFSApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    var body: some Scene {
        MenuBarExtra {
            StatusPopover()
                .environment(delegate.model)
        } label: {
            MenuBarLabel()
                .environment(delegate.model)
        }
        .menuBarExtraStyle(.window)

        Window("ModelFS", id: WindowID.main) {
            MainWindow()
                .environment(delegate.model)
        }
        .defaultSize(width: 1040, height: 660)
        .defaultLaunchBehavior(.suppressed)

        Settings {
            SettingsView()
                .environment(delegate.model)
        }
    }
}

/// Starts the model once, and keeps the app out of the Dock by default.
///
/// The app lives in the menu bar (an accessory app). While its main window
/// is open it becomes a regular app, so the window shows in the Dock and
/// the app switcher; see `MainWindow`.
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    let model = AppModel()

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        Task { await model.start() }
        #if DEBUG
        Task { await Snapshots.renderIfRequested(model: model) }
        #endif
    }

    /// Opening the app again (Finder, Spotlight, `open`) shows its window.
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows: Bool) -> Bool {
        NotificationCenter.default.post(name: .showMainWindow, object: nil)
        return false
    }
}

/// The menu bar icon; it opens the main window on the very first launch.
struct MenuBarLabel: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Image(systemName: model.activity.isBusy ? "arrow.triangle.2.circlepath" : "square.stack.3d.up")
            .accessibilityLabel("ModelFS")
            .task(id: model.needsFirstScan) {
                if model.needsFirstScan {
                    openWindow(id: WindowID.main)
                }
            }
            .onReceive(NotificationCenter.default.publisher(for: .showMainWindow)) { _ in
                openWindow(id: WindowID.main)
                NSApp.activate()
            }
    }
}
