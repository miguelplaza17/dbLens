import AppKit
import SwiftUI
import DBLensKit

/// How each kind of database shows up: its color and icon, in the connection
/// sheet's tag, the sidebar and the connections list. Settings > Appearance
/// lets people pick the logo or an SF Symbol, and the color, for each kind.
extension Engine {
    /// The color a kind of database starts with.
    var defaultColor: UInt32 {
        switch self {
        case .postgres: 0x5B9BE0
        case .mysql: 0xF29111
        case .sqlite: 0x4FB3E8
        case .redis: 0xE5534B
        case .mongodb: 0x4DB33D
        }
    }

    var color: Color { EngineIconStore.shared.color(self) }

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

/// The icon style and color chosen for each kind of database, saved in UserDefaults.
@Observable
final class EngineIconStore {
    static let shared = EngineIconStore()

    enum Style: String, CaseIterable, Identifiable {
        case logo, symbol
        var id: String { rawValue }
    }

    private var styles: [String: String]
    private var colors: [String: Int]

    private init() {
        styles = UserDefaults.standard.dictionary(forKey: "appearance.engineIconStyle") as? [String: String] ?? [:]
        colors = UserDefaults.standard.dictionary(forKey: "appearance.engineIconColor") as? [String: Int] ?? [:]
    }

    func style(_ engine: Engine) -> Style { styles[engine.rawValue].flatMap(Style.init) ?? .logo }

    func color(_ engine: Engine) -> Color { Color(hex: UInt32(colors[engine.rawValue] ?? Int(engine.defaultColor))) }

    func isCustomized(_ engine: Engine) -> Bool { styles[engine.rawValue] != nil || colors[engine.rawValue] != nil }

    func setStyle(_ style: Style, for engine: Engine) {
        styles[engine.rawValue] = style.rawValue
        UserDefaults.standard.set(styles, forKey: "appearance.engineIconStyle")
    }

    func setColor(_ color: Color, for engine: Engine) {
        guard let rgb = NSColor(color).usingColorSpace(.sRGB) else { return }
        let value = Int(rgb.redComponent * 255 + 0.5) << 16 | Int(rgb.greenComponent * 255 + 0.5) << 8 | Int(rgb.blueComponent * 255 + 0.5)
        colors[engine.rawValue] = value
        UserDefaults.standard.set(colors, forKey: "appearance.engineIconColor")
    }

    func reset(_ engine: Engine) {
        styles[engine.rawValue] = nil
        colors[engine.rawValue] = nil
        UserDefaults.standard.set(styles, forKey: "appearance.engineIconStyle")
        UserDefaults.standard.set(colors, forKey: "appearance.engineIconColor")
    }
}

/// A kind of database's icon, in the style and color chosen in Settings.
struct EngineIcon: View {
    let engine: Engine
    private var store = EngineIconStore.shared

    init(_ engine: Engine) { self.engine = engine }

    var body: some View {
        if store.style(engine) == .logo, let logo = EngineLogos.image(engine) {
            Image(nsImage: logo)
                .renderingMode(.template)
                .resizable()
                .scaledToFit()
                .foregroundStyle(store.color(engine))
                .frame(width: Space.lg, height: Space.lg)
        } else {
            Image(systemName: engine.symbol).foregroundStyle(store.color(engine))
        }
    }
}

/// A small colored tag naming the database a URL is for.
struct EngineTag: View {
    let engine: Engine

    var body: some View {
        Label { Text(verbatim: engine.name) } icon: { EngineIcon(engine) }
            .font(.caption.weight(.semibold))
            .foregroundStyle(engine.color)
            .padding(.horizontal, Space.sm)
            .padding(.vertical, Space.xs / 2)
            .background(engine.color.opacity(0.16), in: Capsule())
            .fixedSize()
            .accessibilityLabel(Text(verbatim: engine.name))
    }
}
