import SwiftUI

/// The update notice at the top of the window: downloading, then ready with
/// "Restart Now" (otherwise it installs when the app quits).
struct UpdateNotice: View {
    @Environment(Updater.self) private var updater

    var body: some View {
        if let notice = updater.notice {
            HStack(spacing: Space.sm) {
                switch notice {
                case .downloading(let version):
                    ProgressView().controlSize(.small)
                    Text("Downloading dbLens \(version)…")
                case .ready(let version):
                    Image(systemName: "arrow.down.circle.fill").foregroundStyle(Palette.accentText)
                    Text("dbLens \(version) is ready. It installs when you quit.")
                    Button("Restart Now") { updater.installNow() }
                        .buttonStyle(.borderedProminent)
                }
                Button("Close", systemImage: "xmark") { updater.dismissNotice() }
                    .labelStyle(.iconOnly)
                    .help("Hide this notice; the update still installs when you quit")
                    .buttonStyle(.borderless)
            }
            .controlSize(.small)
            .padding(.horizontal, Space.md)
            .padding(.vertical, Space.sm)
            .background(.regularMaterial, in: Capsule())
            .overlay(Capsule().strokeBorder(Color(nsColor: .separatorColor)))
            .shadow(color: .black.opacity(0.3), radius: 8, y: 3)
            .padding(.top, Space.sm)
            .transition(.move(edge: .top).combined(with: .opacity))
        }
    }
}
