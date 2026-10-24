import Foundation

/// Finds the `modeld` engine the app runs.
///
/// Release builds only use the copy bundled at
/// `ModelFS.app/Contents/MacOS/modeld`, so the app and the engine always
/// share a version and a JSON schema. Debug builds may point `MODELD_BIN`
/// at a `cargo build` output, which `swift run` and Xcode need.
public enum EngineLocator {
    /// The engine's executable, or `nil` when none is available.
    public static func locate(
        bundle: Bundle = .main,
        environment: [String: String] = ProcessInfo.processInfo.environment
    ) -> URL? {
        #if DEBUG
        if let override = environment["MODELD_BIN"], !override.isEmpty {
            return URL(fileURLWithPath: override)
        }
        #endif
        return bundle.url(forAuxiliaryExecutable: "modeld")
    }
}
