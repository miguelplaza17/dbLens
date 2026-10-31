import AppKit
import SwiftUI

/// A thin, quiet scroll bar that stays visible whenever there is something to
/// scroll, so a wide grid shows it can scroll sideways even without a trackpad.
/// The knob is a soft capsule that brightens under the pointer; no track.
final class ThinScroller: NSScroller {
    private static let thickness: CGFloat = 10
    private var hovering = false {
        didSet { if hovering != oldValue { needsDisplay = true } }
    }

    override class var isCompatibleWithOverlayScrollers: Bool { false }

    override class func scrollerWidth(for controlSize: ControlSize, scrollerStyle: Style) -> CGFloat { thickness }

    override func drawKnobSlot(in slotRect: NSRect, highlight flag: Bool) {
        NSColor.white.withAlphaComponent(0.025).setFill()
        slotRect.fill()
    }

    override func drawKnob() {
        let knob = rect(for: .knob)
        let vertical = bounds.height > bounds.width
        let rect = vertical ? knob.insetBy(dx: 2.5, dy: 2) : knob.insetBy(dx: 2, dy: 2.5)
        guard rect.width > 0, rect.height > 0 else { return }
        let radius = min(rect.width, rect.height) / 2
        NSColor.white.withAlphaComponent(hovering ? 0.38 : 0.2).setFill()
        NSBezierPath(roundedRect: rect, xRadius: radius, yRadius: radius).fill()
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: bounds, options: [.mouseEnteredAndExited, .activeInActiveApp, .inVisibleRect],
                                       owner: self))
    }

    override func mouseEntered(with event: NSEvent) { hovering = true }
    override func mouseExited(with event: NSEvent) { hovering = false }
}

/// A scroll view that always uses ThinScroller, whatever the Mac's
/// "Show scroll bars" setting, since the system resets the style when it changes.
final class ThinScrollView: NSScrollView {
    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        verticalScroller = ThinScroller()
        horizontalScroller = ThinScroller()
        autohidesScrollers = true
        super.scrollerStyle = .legacy
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    override var scrollerStyle: NSScroller.Style {
        get { .legacy }
        set { super.scrollerStyle = .legacy }
    }
}

/// Gives the scroll view around it (a SwiftUI List or ScrollView) the thin
/// scroll bars. Put it inside the scrolling content, as a row's background.
struct ThinScrollBars: NSViewRepresentable {
    func makeNSView(context: Context) -> Installer { Installer() }
    func updateNSView(_ view: Installer, context: Context) { view.install() }

    final class Installer: NSView {
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            NotificationCenter.default.removeObserver(self)
            guard window != nil else { return }
            // The system puts its own scroll bars back when the Mac's setting changes.
            NotificationCenter.default.addObserver(self, selector: #selector(install),
                                                   name: NSScroller.preferredScrollerStyleDidChangeNotification, object: nil)
            DispatchQueue.main.async { [weak self] in self?.install() }
        }

        @objc func install() {
            guard let scroll = enclosingScrollView else { return }
            if !(scroll.verticalScroller is ThinScroller) { scroll.verticalScroller = ThinScroller() }
            if !(scroll.horizontalScroller is ThinScroller) { scroll.horizontalScroller = ThinScroller() }
            if scroll.scrollerStyle != .legacy { scroll.scrollerStyle = .legacy }
            scroll.autohidesScrollers = true
        }
    }
}
