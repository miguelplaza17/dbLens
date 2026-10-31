import SwiftUI
import DBLensKit

/// How each kind of database shows up: its color and symbol, in the
/// connection sheet's tag and the sidebar.
extension Engine {
    var color: Color {
        switch self {
        case .postgres: Color(hex: 0x5B9BE0)
        case .mysql: Color(hex: 0xF29111)
        case .sqlite: Color(hex: 0x4FB3E8)
        case .redis: Color(hex: 0xE5534B)
        case .mongodb: Color(hex: 0x4DB33D)
        }
    }

    var symbol: String {
        switch self {
        case .postgres: "cylinder.split.1x2"
        case .mysql: "cylinder"
        case .sqlite: "doc.text"
        case .redis: "bolt.horizontal"
        case .mongodb: "leaf"
        }
    }
}

/// A small colored tag naming the database a URL is for.
struct EngineTag: View {
    let engine: Engine

    var body: some View {
        Label(engine.name, systemImage: engine.symbol)
            .font(.caption.weight(.semibold))
            .foregroundStyle(engine.color)
            .padding(.horizontal, Space.sm)
            .padding(.vertical, Space.xs / 2)
            .background(engine.color.opacity(0.16), in: Capsule())
            .fixedSize()
            .accessibilityLabel(Text(verbatim: engine.name))
    }
}
