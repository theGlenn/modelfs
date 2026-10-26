import Foundation

/// Variants that share a model's name, each retaining its digest and copies.
public struct ModelGroup: Identifiable, Sendable, Equatable {
    public var id: String
    public var name: String
    public var models: [Model]

    /// The combined size of the distinct variants, rather than their app copies.
    public var size: UInt64 {
        models.reduce(0) { total, model in
            let (sum, overflow) = total.addingReportingOverflow(model.size)
            return overflow ? .max : sum
        }
    }

    public var providers: [Provider] {
        Array(Set(models.flatMap(\.providers))).sorted()
    }

    /// Groups names without discarding parameter sizes, quantizations, or paths.
    /// The order of groups and their variants follows the input order.
    public static func grouping(_ models: [Model]) -> [Self] {
        var groups: [Self] = []
        var indexes: [String: Int] = [:]
        for model in models {
            let name = model.name.split(whereSeparator: \.isWhitespace).joined(separator: " ")
            // A missing name must not combine unrelated digests into one model.
            let key = name.isEmpty ? model.digest : name.lowercased()
            if let index = indexes[key] {
                groups[index].models.append(model)
            } else {
                indexes[key] = groups.count
                groups.append(Self(id: "group:\(key)", name: name.isEmpty ? model.digest : name, models: [model]))
            }
        }
        return groups
    }
}
