import Testing
@testable import ModelFSKit

@Suite struct ModelGroupTests {
    @Test func variantsShareANameWithoutLosingTheirIdentity() throws {
        let first = model("a", name: "Qwen3.5-0.8B", quant: "Q4_K_M", size: 100)
        let second = model("b", name: " qwen3.5-0.8B \n", quant: "Q6_K", size: 160)
        let third = model("c", name: "Qwen3.5-0.8B", quant: "Q4_K_M", size: 101)

        let group = try #require(ModelGroup.grouping([first, second, third]).first)

        #expect(group.name == "Qwen3.5-0.8B")
        #expect(group.models.map(\.id) == [first.id, second.id, third.id])
        #expect(group.size == 361)
        #expect(group.providers == [.ollama, .manual])
    }

    @Test func differentParameterSizesRemainSeparate() {
        let groups = ModelGroup.grouping([
            model("a", name: "Qwen3.5-0.8B"),
            model("b", name: "Qwen3.5-4B"),
            model("c", name: "Qwen3.5-0.8B"),
        ])

        #expect(groups.count == 2)
        #expect(groups.map(\.name) == ["Qwen3.5-0.8B", "Qwen3.5-4B"])
        #expect(groups.map { $0.models.count } == [2, 1])
    }

    @Test func blankNamesDoNotCombineUnrelatedModels() {
        let groups = ModelGroup.grouping([model("a", name: " "), model("b", name: "\n")])

        #expect(groups.count == 2)
        #expect(groups.map(\.name) == ["sha256:a", "sha256:b"])
    }

    @Test func groupIdentitySurvivesSortingAndFiltering() throws {
        let first = model("a", name: "Qwen3.5-0.8B")
        let second = model("b", name: "qwen3.5-0.8b")

        let original = try #require(ModelGroup.grouping([first, second]).first)
        let reversed = try #require(ModelGroup.grouping([second, first]).first)
        let filtered = try #require(ModelGroup.grouping([second]).first)

        #expect(original.id == reversed.id)
        #expect(original.id == filtered.id)
    }

    private func model(_ digest: String, name: String, quant: String = "Q4_K_M", size: UInt64 = 100) -> Model {
        Model(
            digest: "sha256:\(digest)",
            name: name,
            kind: .model,
            format: "GGUF",
            quant: quant,
            params: "0.8B",
            architecture: "qwen3",
            size: size,
            stored: StoredCopy(path: "/store/\(digest)", present: true),
            references: [
                Reference(
                    path: "/models/\(digest).gguf",
                    provider: digest == "a" ? .ollama : .manual,
                    label: nil,
                    source: nil,
                    tags: [],
                    state: .clone
                ),
            ],
            space: .zero
        )
    }
}
