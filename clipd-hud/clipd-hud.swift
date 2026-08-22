import Cocoa

let kAccent = NSColor(calibratedRed: 0.11, green: 0.62, blue: 0.46, alpha: 1.0)
let kAccentTint = NSColor(calibratedRed: 0.11, green: 0.62, blue: 0.46, alpha: 0.10)

struct Row {
    let badge: String
    let kind: String
    let preview: String
    let active: Bool
}

/// A clickable choice in a `prompt` HUD. `id` is what gets written to stdout.
struct Button {
    let id: String
    let label: String
    let primary: Bool
}

struct Payload {
    var style = ""
    var title = ""
    var hint = ""
    var badge = ""
    var previewKind = ""
    var preview = ""
    var foot = ""
    var rows: [Row] = []
    var buttons: [Button] = []
    var timeout: Double = 0
    /// Points of screen the HUD must keep clear at the top.
    ///
    /// The island lives at the top centre — the same place the HUD used to
    /// open — so when it is the active layout the daemon sends the depth it
    /// occupies and the HUD opens below it instead of on top of it.
    var avoidTop: CGFloat = 0
}

/// Where a panel of this height belongs, in AppKit's bottom-left origin.
///
/// `fraction` is the usual resting height; `avoidTop` pushes it down far
/// enough that it clears whatever owns the top of the screen.
func panelY(_ screen: NSScreen, height h: CGFloat, fraction: CGFloat, avoidTop: CGFloat) -> CGFloat {
    let y = screen.frame.height * fraction
    guard avoidTop > 0 else { return y }
    // Distance from the top of the screen to the panel's top edge.
    let highest = screen.frame.height - avoidTop - h
    return min(y, highest)
}

func parse(_ text: String) -> Payload {
    var p = Payload()
    for line in text.components(separatedBy: "\n") {
        let f = line.components(separatedBy: "\t")
        guard let tag = f.first else { continue }
        switch tag {
        case "STYLE":   if f.count > 1 { p.style = f[1] }
        case "TITLE":   if f.count > 1 { p.title = f[1] }
        case "HINT":    if f.count > 1 { p.hint = f[1] }
        case "BADGE":   if f.count > 1 { p.badge = f[1] }
        case "FOOT":    if f.count > 1 { p.foot = f[1] }
        case "PREVIEW": if f.count > 2 { p.previewKind = f[1]; p.preview = f[2] }
        case "TIMEOUT": if f.count > 1 { p.timeout = Double(f[1]) ?? 0 }
        case "AVOIDTOP": if f.count > 1 { p.avoidTop = CGFloat(Double(f[1]) ?? 0) }
        case "BTN":
            if f.count > 2 { p.buttons.append(Button(id: f[1], label: f[2], primary: f.count > 3 && f[3] == "1")) }
        case "ROW":
            if f.count > 5 { p.rows.append(Row(badge: f[2], kind: f[3], preview: f[4], active: f[5] == "1")) }
        default: break
        }
    }
    return p
}

func sfName(_ kind: String) -> String {
    switch kind {
    case "link":   return "link"
    case "mail":   return "envelope"
    case "code":   return "chevron.left.forwardslash.chevron.right"
    case "number": return "number"
    case "slot":   return "square.stack"
    case "empty":  return "tray"
    default:       return "doc.text"
    }
}

/// A panel that can take key status so buttons highlight and Return works,
/// but — paired with `becomesKeyOnlyIfNeeded` — only actually takes it for
/// controls that need text input. Buttons don't, so the password field the
/// user is typing into keeps focus while the prompt is up.
class PromptPanel: NSPanel {
    override var canBecomeKey: Bool { true }
}

// NSObject so the prompt style's buttons can use target/action.
class HUD: NSObject {
    var panel: NSPanel!
    var root: NSView!
    /// Button ids for the `prompt` style, indexed by the button's tag.
    var choices: [String] = []

    func label(_ s: String, size: CGFloat, weight: NSFont.Weight, color: NSColor,
               align: NSTextAlignment = .left) -> NSTextField {
        let l = NSTextField(labelWithString: s)
        l.textColor = color
        l.font = .systemFont(ofSize: size, weight: weight)
        l.alignment = align
        l.lineBreakMode = .byTruncatingTail
        l.backgroundColor = .clear
        l.isBordered = false
        return l
    }

    func box(_ frame: NSRect, color: NSColor, radius: CGFloat) -> NSView {
        let v = NSView(frame: frame)
        v.wantsLayer = true
        v.layer?.backgroundColor = color.cgColor
        v.layer?.cornerRadius = radius
        return v
    }

    func symbol(_ kind: String, frame: NSRect, point: CGFloat, color: NSColor) {
        let iv = NSImageView(frame: frame)
        let cfg = NSImage.SymbolConfiguration(pointSize: point, weight: .regular)
        if let base = NSImage(systemSymbolName: sfName(kind), accessibilityDescription: nil) {
            iv.image = base.withSymbolConfiguration(cfg) ?? base
        }
        iv.contentTintColor = color
        iv.imageScaling = .scaleProportionallyDown
        root.addSubview(iv)
    }

    func textWidth(_ s: String, size: CGFloat, weight: NSFont.Weight) -> CGFloat {
        let f = NSFont.systemFont(ofSize: size, weight: weight)
        return (s as NSString).size(withAttributes: [.font: f]).width
    }

    func showToast(_ p: Payload) {
        guard let screen = NSScreen.main else { return }
        let w: CGFloat = 400, h: CGFloat = 72
        let x = (screen.frame.width - w) / 2
        let y = panelY(screen, height: h, fraction: 0.74, avoidTop: p.avoidTop)
        makePanel(NSRect(x: x, y: y, width: w, height: h))

        // Slot badge — the clearest answer to "which slot".
        let bs: CGFloat = 40
        let by = (h - bs) / 2
        root.addSubview(box(NSRect(x: 16, y: by, width: bs, height: bs), color: kAccent, radius: 11))
        let bt = label(p.badge, size: 18, weight: .semibold, color: .white, align: .center)
        bt.frame = NSRect(x: 16, y: by + 8, width: bs, height: 24)
        root.addSubview(bt)

        let tx: CGFloat = 16 + bs + 14
        let hintW = textWidth(p.hint, size: 12, weight: .regular) + 2
        let hasPreview = !p.preview.isEmpty
        // With a preview the title sits on the upper line; without one it
        // centers vertically so the toast doesn't look top-heavy.
        let titleY: CGFloat = hasPreview ? 37 : (h - 20) / 2

        // Line 1: status + muted retrieval hint on the right.
        let t = label(p.title, size: 15, weight: .medium, color: .labelColor)
        t.frame = NSRect(x: tx, y: titleY, width: w - tx - hintW - 24, height: 20)
        root.addSubview(t)
        if !p.hint.isEmpty {
            let hl = label(p.hint, size: 12, weight: .regular, color: .tertiaryLabelColor, align: .right)
            hl.frame = NSRect(x: w - hintW - 16, y: titleY + 2, width: hintW, height: 16)
            root.addSubview(hl)
        }

        // Line 2: content-type symbol + preview.
        if !p.preview.isEmpty {
            symbol(p.previewKind, frame: NSRect(x: tx, y: 15, width: 16, height: 16),
                   point: 13, color: .secondaryLabelColor)
            let pv = label(p.preview, size: 13, weight: .regular, color: .secondaryLabelColor)
            pv.frame = NSRect(x: tx + 22, y: 14, width: w - tx - 22 - 16, height: 18)
            root.addSubview(pv)
        }

        present(duration: p.preview.isEmpty ? 1.0 : 1.8)
    }

    func showList(_ p: Payload) {
        guard let screen = NSScreen.main else { return }
        let w: CGFloat = 440
        let rowH: CGFloat = 32, rowGap: CGFloat = 2
        let padTop: CGFloat = 14, padBottom: CGFloat = 12
        let headerH: CGFloat = 20, headerGap: CGFloat = 10
        let footH: CGFloat = p.foot.isEmpty ? 0 : 15
        let footGap: CGFloat = p.foot.isEmpty ? 0 : 10
        let bodyCount = max(p.rows.count, p.preview.isEmpty ? 0 : 1)
        let listH = CGFloat(bodyCount) * rowH + CGFloat(max(bodyCount - 1, 0)) * rowGap
        let h = padTop + headerH + headerGap + listH + footGap + footH + padBottom
        let x = (screen.frame.width - w) / 2
        let y = panelY(screen, height: h, fraction: 0.72, avoidTop: p.avoidTop) - h / 2
        makePanel(NSRect(x: x, y: y, width: w, height: h))

        var cursor = h - padTop
        cursor -= headerH
        let t = label(p.title, size: 14, weight: .medium, color: .labelColor)
        t.frame = NSRect(x: 20, y: cursor, width: w - 140, height: headerH)
        root.addSubview(t)
        if !p.hint.isEmpty {
            let hl = label(p.hint, size: 12, weight: .regular, color: .tertiaryLabelColor, align: .right)
            hl.frame = NSRect(x: w - 140, y: cursor + 1, width: 120, height: 16)
            root.addSubview(hl)
        }
        cursor -= headerGap

        func drawRow(badge: String, kind: String, text: String, active: Bool, y: CGFloat) {
            if active {
                root.addSubview(box(NSRect(x: 16, y: y, width: w - 32, height: rowH),
                                    color: kAccentTint, radius: 9))
            }
            if !badge.isEmpty {
                let bSize: CGFloat = 22
                let bY = y + (rowH - bSize) / 2
                root.addSubview(box(NSRect(x: 24, y: bY, width: bSize, height: bSize),
                                    color: active ? kAccent : NSColor(white: 0, alpha: 0.06), radius: 6))
                let bt = label(badge, size: 11.5, weight: .medium,
                               color: active ? .white : .secondaryLabelColor, align: .center)
                bt.frame = NSRect(x: 24, y: bY + 3, width: bSize, height: 16)
                root.addSubview(bt)
            }
            symbol(kind, frame: NSRect(x: 56, y: y + (rowH - 16) / 2, width: 16, height: 16),
                   point: 13, color: .secondaryLabelColor)
            let tColor: NSColor = active ? .labelColor : .secondaryLabelColor
            let tw = label(text, size: 13, weight: active ? .medium : .regular, color: tColor)
            tw.frame = NSRect(x: 82, y: y + (rowH - 18) / 2, width: w - 82 - 20, height: 18)
            root.addSubview(tw)
        }

        if p.rows.isEmpty && !p.preview.isEmpty {
            cursor -= rowH
            drawRow(badge: "", kind: p.previewKind, text: p.preview, active: false, y: cursor)
        } else {
            for row in p.rows {
                cursor -= rowH
                drawRow(badge: row.badge, kind: row.kind, text: row.preview, active: row.active, y: cursor)
                cursor -= rowGap
            }
        }

        if !p.foot.isEmpty {
            let f = label(p.foot, size: 11.5, weight: .regular, color: .tertiaryLabelColor)
            f.frame = NSRect(x: 20, y: padBottom - 2, width: w - 40, height: footH)
            root.addSubview(f)
        }

        present(duration: 2.2)
    }

    /// An interactive HUD that waits for a click and writes the chosen button
    /// id to stdout. Unlike the passive styles this one never auto-dismisses
    /// silently: on timeout it reports `timeout` so the caller can distinguish
    /// "the user declined" from "the user never saw it".
    func showPrompt(_ p: Payload) {
        guard let screen = NSScreen.main else { exit(0) }
        let w: CGFloat = 400
        let btnH: CGFloat = 28, btnGap: CGFloat = 8
        let hasPreview = !p.preview.isEmpty
        let h: CGFloat = (hasPreview ? 108 : 88) + btnH
        // Top-right, out of the way of whatever the user is typing into.
        let x = screen.visibleFrame.maxX - w - 20
        let y = screen.visibleFrame.maxY - h - 20
        makePanel(NSRect(x: x, y: y, width: w, height: h), interactive: true)

        let bs: CGFloat = 40
        let topY = h - 16 - bs
        root.addSubview(box(NSRect(x: 16, y: topY, width: bs, height: bs), color: kAccent, radius: 11))
        let bt = label(p.badge, size: 18, weight: .semibold, color: .white, align: .center)
        bt.frame = NSRect(x: 16, y: topY + 8, width: bs, height: 24)
        root.addSubview(bt)

        let tx: CGFloat = 16 + bs + 14
        let t = label(p.title, size: 15, weight: .medium, color: .labelColor)
        t.frame = NSRect(x: tx, y: topY + bs - 20, width: w - tx - 16, height: 20)
        root.addSubview(t)

        if !p.hint.isEmpty {
            let hl = label(p.hint, size: 12, weight: .regular, color: .tertiaryLabelColor)
            hl.frame = NSRect(x: tx, y: topY + 2, width: w - tx - 16, height: 16)
            root.addSubview(hl)
        }
        if hasPreview {
            let pv = label(p.preview, size: 13, weight: .regular, color: .secondaryLabelColor)
            pv.frame = NSRect(x: 16, y: topY - 24, width: w - 32, height: 18)
            root.addSubview(pv)
        }

        // Drawn right-to-left over the reversed list, so buttons appear in the
        // order given and the last one lands in the bottom-right corner where
        // macOS users expect the default action.
        var right: CGFloat = w - 16
        for (i, b) in p.buttons.enumerated().reversed() {
            let bw = max(textWidth(b.label, size: 13, weight: .medium) + 28, 76)
            right -= bw
            let button = NSButton(frame: NSRect(x: right, y: 14, width: bw, height: btnH))
            button.title = b.label
            button.bezelStyle = .rounded
            button.font = .systemFont(ofSize: 13, weight: .medium)
            button.keyEquivalent = b.primary ? "\r" : ""
            button.tag = i
            button.target = self
            button.action = #selector(HUD.choose(_:))
            root.addSubview(button)
            right -= btnGap
        }
        choices = p.buttons.map { $0.id }

        panel.alphaValue = 0
        panel.orderFront(nil)
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = 0.10
            self.panel.animator().alphaValue = 1
        }
        if p.timeout > 0 {
            DispatchQueue.main.asyncAfter(deadline: .now() + p.timeout) { self.finish("timeout") }
        }
    }

    @objc func choose(_ sender: NSButton) {
        finish(sender.tag < choices.count ? choices[sender.tag] : "dismiss")
    }

    func finish(_ id: String) {
        print(id)
        fflush(stdout)
        NSApplication.shared.terminate(nil)
    }

    func showSimple(_ text: String) {
        guard let screen = NSScreen.main else { return }
        let title = text.components(separatedBy: "\n").first ?? text
        let w: CGFloat = 300, h: CGFloat = 60
        let x = (screen.frame.width - w) / 2
        let y = screen.frame.height * 0.76
        makePanel(NSRect(x: x, y: y, width: w, height: h))
        let l = label(title, size: 18, weight: .medium, color: .labelColor, align: .center)
        l.frame = NSRect(x: 16, y: 18, width: w - 32, height: 24)
        root.addSubview(l)
        present(duration: 0.75)
    }

    func makePanel(_ rect: NSRect, interactive: Bool = false) {
        let mask: NSWindow.StyleMask = [.borderless, .nonactivatingPanel]
        panel = interactive
            ? PromptPanel(contentRect: rect, styleMask: mask, backing: .buffered, defer: false)
            : NSPanel(contentRect: rect, styleMask: mask, backing: .buffered, defer: false)
        panel.becomesKeyOnlyIfNeeded = true
        // Passive HUDs must never swallow a click meant for the app underneath;
        // only the prompt is interactive.
        panel.ignoresMouseEvents = !interactive
        panel.level = .floating
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.appearance = NSAppearance(named: .aqua)

        let fx = NSVisualEffectView(frame: NSRect(origin: .zero, size: rect.size))
        fx.material = .popover
        fx.blendingMode = .behindWindow
        fx.state = .active
        fx.wantsLayer = true
        fx.layer?.cornerRadius = 17
        fx.layer?.masksToBounds = true
        fx.autoresizingMask = [.width, .height]
        panel.contentView = fx
        root = fx
    }

    func present(duration: Double) {
        let app = NSApplication.shared
        panel.alphaValue = 0
        panel.orderFront(nil)
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = 0.10
            self.panel.animator().alphaValue = 1
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + duration) {
            NSAnimationContext.runAnimationGroup({ ctx in
                ctx.duration = 0.22
                self.panel.animator().alphaValue = 0
            }, completionHandler: { app.terminate(nil) })
        }
    }

    func show(text: String) {
        NSApplication.shared.setActivationPolicy(.accessory)
        let p = parse(text)
        switch p.style {
        case "toast": showToast(p)
        case "list": showList(p)
        case "prompt": showPrompt(p)
        default: showSimple(text)
        }
        NSApplication.shared.run()
    }
}

let text = CommandLine.arguments.dropFirst().joined(separator: " ")
guard !text.isEmpty else { exit(0) }
HUD().show(text: text)
