// Clipd app icon: the helmeted cat on the brand tile, with a clipboard badge.
// Usage (from the repo root):
//   swiftc -O -o /tmp/make-app-icon packaging/icons/make-app-icon.swift
//   /tmp/make-app-icon clipd-gui/assets/cat.png packaging/icons/AppIcon-1024.png
import AppKit

let args = CommandLine.arguments
let source = NSBitmapImageRep(data: NSImage(contentsOfFile: args[1])!.tiffRepresentation!)!
// Crop to the drawing itself: the frame has empty margin around the cat.
var (minX, minY, maxX, maxY) = (source.pixelsWide, source.pixelsHigh, 0, 0)
for y in 0..<source.pixelsHigh {
    for x in 0..<source.pixelsWide where (source.colorAt(x: x, y: y)?.alphaComponent ?? 0) > 0.05 {
        minX = min(minX, x); maxX = max(maxX, x); minY = min(minY, y); maxY = max(maxY, y)
    }
}
let cropped = source.cgImage!.cropping(to: CGRect(x: minX, y: minY, width: maxX - minX + 1, height: maxY - minY + 1))!
let cat = NSImage(cgImage: cropped, size: NSSize(width: cropped.width, height: cropped.height))
let size: CGFloat = 1024
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: 1024, pixelsHigh: 1024,
                           bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                           colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
NSGraphicsContext.current?.imageInterpolation = .high

// macOS icon grid: an 824pt tile centred on the 1024 canvas.
let tile = NSRect(x: 100, y: 100, width: 824, height: 824)
let tilePath = NSBezierPath(roundedRect: tile, xRadius: 185, yRadius: 185)
NSGraphicsContext.saveGraphicsState()
let shadow = NSShadow()
shadow.shadowColor = NSColor.black.withAlphaComponent(0.28)
shadow.shadowBlurRadius = 28
shadow.shadowOffset = NSSize(width: 0, height: -10)
shadow.set()
NSColor.black.setFill(); tilePath.fill()
NSGraphicsContext.restoreGraphicsState()
let gradient = NSGradient(starting: NSColor(calibratedRed: 0.39, green: 0.40, blue: 0.95, alpha: 1),
                          ending: NSColor(calibratedRed: 0.55, green: 0.36, blue: 0.96, alpha: 1))!
gradient.draw(in: tilePath, angle: -45)

// The cat: as large as the tile allows, nudged up-left to leave the badge room.
NSGraphicsContext.saveGraphicsState()
tilePath.addClip()
let catW: CGFloat = 690
let catH = catW * cat.size.height / cat.size.width
let catRect = NSRect(x: (size - catW) / 2 - 10, y: (size - catH) / 2 + 70, width: catW, height: catH)
let catShadow = NSShadow()
catShadow.shadowColor = NSColor.black.withAlphaComponent(0.22)
catShadow.shadowBlurRadius = 18
catShadow.shadowOffset = NSSize(width: 0, height: -8)
catShadow.set()
cat.draw(in: catRect, from: .zero, operation: .sourceOver, fraction: 1)
NSGraphicsContext.restoreGraphicsState()

// The clipboard badge: a white disc with the clipboard symbol in brand indigo.
let badge = NSRect(x: 650, y: 140, width: 240, height: 240)
NSGraphicsContext.saveGraphicsState()
let bs = NSShadow()
bs.shadowColor = NSColor.black.withAlphaComponent(0.25)
bs.shadowBlurRadius = 20
bs.shadowOffset = NSSize(width: 0, height: -6)
bs.set()
NSColor.white.setFill()
NSBezierPath(ovalIn: badge).fill()
NSGraphicsContext.restoreGraphicsState()
let cfg = NSImage.SymbolConfiguration(pointSize: 150, weight: .semibold)
    .applying(.init(hierarchicalColor: NSColor(calibratedRed: 0.36, green: 0.33, blue: 0.90, alpha: 1)))
if let sym = NSImage(systemSymbolName: "list.clipboard", accessibilityDescription: nil)?
    .withSymbolConfiguration(cfg) {
    let s = sym.size
    let scale = min(140 / s.width, 140 / s.height)
    let w = s.width * scale, h = s.height * scale
    sym.draw(in: NSRect(x: badge.midX - w / 2, y: badge.midY - h / 2, width: w, height: h))
}

NSGraphicsContext.restoreGraphicsState()
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: args[2]))
