import Foundation

/// How a model stands overall, for one column of the models table.
public enum ModelStatus: Int, Sendable, Comparable, CaseIterable {
    /// Apps hold independent copies a pass can turn into clones.
    case canDeduplicate
    /// A copy changed since ModelFS hashed it.
    case changed
    /// Several apps share one copy.
    case sharing
    /// One app holds it; nothing to share.
    case single
    /// No app uses it; only the store keeps a copy.
    case storeOnly

    public static func < (lhs: Self, rhs: Self) -> Bool {
        lhs.rawValue < rhs.rawValue
    }
}

extension Model {
    /// Copies an app still has on disk.
    public var presentReferences: [Reference] {
        references.filter { $0.state != .missing }
    }

    /// The apps using this model, in a fixed order.
    public var providers: [Provider] {
        Array(Set(presentReferences.map(\.provider))).sorted()
    }

    public var status: ModelStatus {
        if presentReferences.isEmpty {
            return .storeOnly
        }
        if space.pending > 0 {
            return .canDeduplicate
        }
        if presentReferences.contains(where: { $0.state == .changed }) {
            return .changed
        }
        if space.saved > 0 {
            return .sharing
        }
        return .single
    }

    /// Whether `query` matches the name, a source, a label, or a path.
    public func matches(_ query: String) -> Bool {
        let needle = query.trimmingCharacters(in: .whitespaces)
        guard !needle.isEmpty else { return true }
        let haystacks = [name, digest] + references.flatMap { [$0.source, $0.label, $0.path].compactMap(\.self) }
        return haystacks.contains { $0.localizedCaseInsensitiveContains(needle) }
    }
}

extension Inventory {
    /// Models with copies a pass can still turn into clones.
    public var deduplicable: [Model] {
        models.filter { $0.space.pending > 0 }
    }

    /// Models whose copies already share one set of blocks.
    public var sharing: [Model] {
        models.filter { $0.space.saved > 0 }
    }
}
