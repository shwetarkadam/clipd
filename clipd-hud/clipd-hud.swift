import Cocoa

class HUD {
    var panel: NSPanel!

    func show(text: String) {
        let app = NSApplication.shared
        app.setActivationPolicy(.accessory)

        guard let screen = NSScreen.main else { return }
        let w: CGFloat = 240
        let h: CGFloat = 56
        let x = (screen.frame.width - w) / 2
        let y = screen.frame.height * 0.82

        panel = NSPanel(
            contentRect: NSRect(x: x, y: y, width: w, height: h),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        panel.level = .floating
        panel.isOpaque = false
        panel.backgroundColor = NSColor(white: 0.08, alpha: 0.88)
        panel.hasShadow = true
        panel.contentView?.wantsLayer = true
        panel.contentView?.layer?.cornerRadius = 14
        panel.contentView?.layer?.masksToBounds = true

        let label = NSTextField(labelWithString: text)
        label.textColor = .white
        label.alignment = .center
        label.font = .systemFont(ofSize: 20, weight: .semibold)
        label.frame = NSRect(x: 0, y: 12, width: w, height: 32)
        panel.contentView?.addSubview(label)

        panel.alphaValue = 0
        panel.orderFront(nil)

        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = 0.08
            self.panel.animator().alphaValue = 1
        }

        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) {
            NSAnimationContext.runAnimationGroup({ ctx in
                ctx.duration = 0.2
                self.panel.animator().alphaValue = 0
            }, completionHandler: {
                app.terminate(nil)
            })
        }

        app.run()
    }
}

let text = CommandLine.arguments.dropFirst().joined(separator: " ")
guard !text.isEmpty else { exit(0) }
HUD().show(text: text)
