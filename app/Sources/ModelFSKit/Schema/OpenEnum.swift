import Foundation

/// A string enum that decodes values it does not know as `.unknown`.
///
/// The engine may be newer than the app (a Homebrew `modeld`, the daemon's
/// copy). Adding an enum value is an additive change under schema 1, so the
/// app must keep decoding instead of failing on it. See `schema/README.md`.
public protocol OpenEnum: RawRepresentable, Decodable, Sendable where RawValue == String {
    static var unknown: Self { get }
}

extension OpenEnum {
    public init(from decoder: any Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = Self(rawValue: raw) ?? .unknown
    }
}
