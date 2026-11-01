import AppKit
import ModelFSKit
import SwiftUI

/// Removing models: each way to do it, with its plan, then the result.
///
/// Plans come from dry runs, fetched in parallel. Confirming runs exactly
/// the chosen plan (`--expect-plan`); if anything changed meanwhile, the
/// engine changes nothing and the sheet shows the new plan instead. Once a
/// removal starts it cannot be cancelled, so the sheet stays open until it
/// ends.
///
/// Several models are removed everywhere, as one plan. The sheet lists
/// each of them; leaving one out plans again without it.
struct RemoveSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var targets: [Model]

    init(target: Model) {
        _targets = State(initialValue: [target])
    }

    init(targets: [Model]) {
        _targets = State(initialValue: targets)
    }

    private var isBatch: Bool { targets.count > 1 }

    private enum Phase: Equatable {
        case loading
        case choosing
        case removing
        case finished(RemovalResult)
    }

    @State private var phase = Phase.loading
    @State private var options: [RemovalOption] = []
    @State private var selection: RemovalScope?
    @State private var notice: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 4) {
                Text(isBatch ? "Remove \(targets.count) models" : "Remove \(targets.first?.name ?? "")")
                    .font(.title3.weight(.semibold))
                Text("\(totalSize.bytes) · \(providers.map(\.displayName).joined(separator: ", "))")
                    .foregroundStyle(.secondary)
            }
            if let notice {
                Label(notice, systemImage: "info.circle")
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            }
            content
            Spacer(minLength: 0)
            footer
        }
        .padding(20)
        .frame(width: 580, height: 560)
        .interactiveDismissDisabled(phase == .removing)
        .task { await load() }
    }

    @ViewBuilder
    private var content: some View {
        switch phase {
        case .loading:
            ProgressView(isBatch ? "Checking what removing them involves…" : "Checking what removing it involves…")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .choosing, .removing:
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    ForEach(options) { option in
                        OptionCard(
                            option: option,
                            title: title(for: option.scope),
                            selected: selection == option.scope,
                            retry: { Task { await load() } },
                            blocked: isBatch && phase != .removing ? blocked : { _ in nil }
                        )
                        .onTapGesture { selection = option.scope }
                        .disabled(phase == .removing)
                    }
                    // The plan first: what it frees and what to know matter
                    // more than the list, which can be long.
                    if isBatch {
                        Text("Models")
                            .font(.headline)
                            .padding(.top, 4)
                        BatchList(
                            targets: targets,
                            leaveOut: phase == .removing ? nil : { leaveOut($0) }
                        )
                    }
                }
            }
        case .finished(let result):
            ResultSummary(result: result)
        }
    }

    private var footer: some View {
        HStack {
            if phase == .removing {
                ProgressView().controlSize(.small)
                Text("Removing… this cannot be cancelled.")
                    .foregroundStyle(.secondary)
            }
            Spacer()
            switch phase {
            case .finished:
                Button("Done") { dismiss() }
                    .keyboardShortcut(.defaultAction)
            default:
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .disabled(phase == .removing)
                Button("Remove", role: .destructive) {
                    Task { await remove() }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!canRemove)
            }
        }
    }

    private var selectedPlan: RemovalPlan? {
        guard case .success(let result) = options.first(where: { $0.scope == selection })?.plan else {
            return nil
        }
        return result.plan
    }

    private var canRemove: Bool {
        guard phase == .choosing, let plan = selectedPlan else { return false }
        return plan.refusals.isEmpty && !model.activity.isBusy
    }

    private var totalSize: UInt64 {
        targets.reduce(0) { $0 + $1.size }
    }

    private var providers: [Provider] {
        Array(Set(targets.flatMap(\.providers))).sorted()
    }

    private func title(for scope: RemovalScope) -> String {
        switch scope {
        case .app(let provider): "Remove from \(provider.displayName)"
        case .everywhere where isBatch: "Remove everywhere"
        case .everywhere: options.count == 1 ? "Remove from \(providers.first?.displayName ?? "the store")" : "Remove everywhere"
        }
    }

    /// The listed model a refusal is about, with a way to leave it out.
    private func blocked(_ refusal: RemovalRefusal) -> (name: String, leaveOut: () -> Void)? {
        guard let target = targets.first(where: refusal.concerns) else { return nil }
        return (target.name, { leaveOut(target) })
    }

    private func leaveOut(_ target: Model) {
        targets.removeAll { $0.id == target.id }
        if targets.isEmpty {
            dismiss()
        } else {
            Task { await load() }
        }
    }

    private func load() async {
        phase = .loading
        options = await model.removalOptions(for: targets)
        if selection == nil || !options.contains(where: { $0.scope == selection }) {
            selection = options.first?.scope
        }
        phase = .choosing
    }

    private func remove() async {
        guard let scope = selection, let plan = selectedPlan else { return }
        phase = .removing
        notice = nil
        let outcome = await model.remove(targets, scope: scope, plan: plan)
        switch outcome {
        case .none:
            notice = model.problem ?? "Another change is running; try again when it ends."
            phase = .choosing
        case .success(let result) where result.status == .planChanged:
            if let index = options.firstIndex(where: { $0.scope == scope }) {
                options[index].plan = .success(result)
            }
            notice = "Something changed since this plan was made, so nothing was removed. Review the new plan."
            phase = .choosing
        case .success(let result):
            phase = .finished(result)
        case .failure(let error):
            notice = error.localizedDescription
            phase = .choosing
        }
    }
}

/// The models a batch removes, each with a way to leave it out.
private struct BatchList: View {
    let targets: [Model]
    let leaveOut: ((Model) -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(targets) { target in
                HStack(spacing: 8) {
                    VStack(alignment: .leading, spacing: 1) {
                        Text(target.name)
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Text(target.providers.map(\.displayName).joined(separator: ", "))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Spacer(minLength: 8)
                    Text(target.size.bytes)
                        .font(.callout)
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                    if let leaveOut {
                        Button {
                            leaveOut(target)
                        } label: {
                            Image(systemName: "minus.circle")
                        }
                        .buttonStyle(.borderless)
                        .help("Leave \(target.name) out of this removal.")
                        .accessibilityLabel("Leave \(target.name) out")
                    }
                }
                .padding(.vertical, 5)
                .padding(.horizontal, 10)
                .help(target.digest)
                if target.id != targets.last?.id {
                    Divider()
                }
            }
        }
        .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 8))
    }
}

/// One way to remove the model: what it frees, what it does, what blocks it.
private struct OptionCard: View {
    let option: RemovalOption
    let title: String
    let selected: Bool
    let retry: () -> Void
    var blocked: (RemovalRefusal) -> (name: String, leaveOut: () -> Void)? = { _ in nil }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Image(systemName: selected ? "largecircle.fill.circle" : "circle")
                    .foregroundStyle(selected ? Color.accentColor : .secondary)
                Text(title).fontWeight(.semibold)
                Spacer()
            }
            switch option.plan {
            case .success(let result):
                PlanDetails(plan: result.plan, retry: retry, blocked: blocked)
            case .failure(let error):
                Label(error.localizedDescription, systemImage: "exclamationmark.triangle")
                    .foregroundStyle(.orange)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary.opacity(selected ? 0.9 : 0.4), in: RoundedRectangle(cornerRadius: 8))
        .overlay {
            RoundedRectangle(cornerRadius: 8)
                .strokeBorder(selected ? Color.accentColor : .clear, lineWidth: 1.5)
        }
        .contentShape(Rectangle())
    }
}

/// A plan's frees line, its steps by app, refusals, notes, and extra files.
private struct PlanDetails: View {
    let plan: RemovalPlan
    let retry: () -> Void
    let blocked: (RemovalRefusal) -> (name: String, leaveOut: () -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(plan.frees.summary(bytes: \.bytes))
                .font(.callout)
            ForEach(plan.changes) { change in
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Image(systemName: change.app?.symbol ?? "externaldrive")
                        .frame(width: 16)
                        .foregroundStyle(.secondary)
                    Text("\(change.app?.displayName ?? "ModelFS store"): \(change.summary)")
                }
                .font(.callout)
                .help(change.paths.joined(separator: "\n"))
            }
            if !plan.collateral.isEmpty {
                let total = plan.collateral.reduce(0) { $0 + $1.size }
                Text("Also removes \(plan.collateral.count) other file(s), \(total.bytes), that only this model uses.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .help(plan.collateral.map(\.path).joined(separator: "\n"))
            }
            ForEach(plan.noteSummaries) { note in
                Label(note.text, systemImage: "info.circle")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .help(note.paths.joined(separator: "\n"))
            }
            ForEach(plan.refusals) { refusal in
                RefusalRow(refusal: refusal, retry: retry, blocked: blocked(refusal))
            }
        }
    }
}

/// Why the plan cannot run, with a way to fix it when there is one.
private struct RefusalRow: View {
    let refusal: RemovalRefusal
    let retry: () -> Void
    /// In a batch: the model it blocks, and a way to leave it out.
    let blocked: (name: String, leaveOut: () -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Label(refusal.advice, systemImage: "xmark.octagon.fill")
                .foregroundStyle(.red)
                .fixedSize(horizontal: false, vertical: true)
            Text(refusal.path)
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.middle)
            HStack {
                if refusal.code == .ollamaNotRunning {
                    Button("Open Ollama") { openOllama() }
                }
                Button("Check again", action: retry)
                if let blocked {
                    Button("Leave out \(blocked.name)", action: blocked.leaveOut)
                }
            }
            .controlSize(.small)
        }
        .font(.callout)
    }

    private func openOllama() {
        let workspace = NSWorkspace.shared
        guard let app = workspace.urlForApplication(withBundleIdentifier: "com.electron.ollama") else {
            workspace.open(URL(fileURLWithPath: "/Applications/Ollama.app"))
            return
        }
        workspace.openApplication(at: app, configuration: NSWorkspace.OpenConfiguration())
    }
}

/// What a finished removal did.
private struct ResultSummary: View {
    let result: RemovalResult

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label(headline, systemImage: symbol)
                .font(.headline)
                .foregroundStyle(result.status == .removed ? Color.green : Color.orange)
            if result.status == .removed {
                Text(result.plan.frees.summary(bytes: \.bytes).replacingOccurrences(of: "Frees", with: "Freed"))
            }
            if let failure = result.outcome?.failure {
                Text("Stopped at: \(failure.reason)")
                    .textSelection(.enabled)
                Text("The steps before it ran; the stored copy was kept, so nothing is lost.")
                    .foregroundStyle(.secondary)
            }
            ForEach(result.plan.refusals) { refusal in
                Text("\(refusal.path): \(refusal.advice)")
                    .font(.callout)
            }
            ForEach(result.outcome?.warnings ?? [], id: \.self) { warning in
                Text(warning).font(.caption).foregroundStyle(.secondary)
            }
            if result.plan.frees.afterTrash > 0, result.status == .removed {
                Text("Files moved to the Trash keep their names, so you can drag them back.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private var headline: String {
        switch result.status {
        case .removed: "Removed"
        case .failed: "Removal stopped at an error"
        case .stopped: "Removal stopped"
        case .refused: "Nothing was removed"
        default: "Nothing was removed"
        }
    }

    private var symbol: String {
        result.status == .removed ? "checkmark.circle.fill" : "exclamationmark.triangle.fill"
    }
}
