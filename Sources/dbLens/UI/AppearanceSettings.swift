import DBLensKit
import SwiftUI

/// Settings > Appearance: the accent color. dbLens is always dark.
struct AppearanceSettings: View {
    @Bindable private var store = ThemeStore.shared
    private var icons = EngineIconStore.shared

    var body: some View {
        Section {
            LabeledContent("Accent Color") {
                VStack(alignment: .trailing, spacing: Space.xs) {
                    HStack(spacing: Space.sm) {
                        ForEach(Theme.all) { theme in
                            AccentSwatch(theme: theme, selected: store.themeID == theme.id) {
                                store.themeID = theme.id
                            }
                        }
                    }
                    Text(store.theme.name)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
        Section("Database Icons") {
            ForEach(Engine.allCases, id: \.self) { engine in
                LabeledContent {
                    HStack(spacing: Space.sm) {
                        Picker("Icon", selection: Binding(get: { icons.style(engine) }, set: { icons.setStyle($0, for: engine) })) {
                            Text("Logo").tag(EngineIconStore.Style.logo)
                            Text("Symbol").tag(EngineIconStore.Style.symbol)
                        }
                        .pickerStyle(.segmented)
                        .labelsHidden()
                        .fixedSize()
                        ColorPicker("Color", selection: Binding(get: { icons.color(engine) }, set: { icons.setColor($0, for: engine) }),
                                    supportsOpacity: false)
                            .labelsHidden()
                        Button("Reset") { icons.reset(engine) }
                            .disabled(!icons.isCustomized(engine))
                    }
                } label: {
                    Label { Text(verbatim: engine.name) } icon: { EngineIcon(engine) }
                }
            }
        }
    }
}

/// One color dot, like the accent row in System Settings.
private struct AccentSwatch: View {
    let theme: Theme
    let selected: Bool
    let choose: () -> Void

    var body: some View {
        Button(action: choose) {
            Circle()
                .fill(fill)
                .frame(width: 16, height: 16)
                .overlay {
                    if selected {
                        Circle().fill(theme.onAccent).frame(width: 6, height: 6) // grid: ok
                    }
                }
                .overlay(Circle().strokeBorder(.primary.opacity(0.15), lineWidth: 1))
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .help(theme.name)
        .accessibilityLabel(theme.name)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private var fill: AnyShapeStyle {
        if let tint = theme.tint {
            AnyShapeStyle(tint)
        } else {
            AnyShapeStyle(AngularGradient(colors: [.red, .orange, .yellow, .green, .blue, .purple, .pink, .red],
                                          center: .center))
        }
    }
}
