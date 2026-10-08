// Draws the app icon (dark tile, a selection frame with corner handles over a coral "shot")
// and writes crates/shotty-app/assets/Shotty.icns. Run: swift tools/make-icon.swift
import AppKit

let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()

func rgb(_ hex: UInt32, _ alpha: CGFloat = 1) -> CGColor {
    CGColor(red: CGFloat((hex >> 16) & 0xff) / 255, green: CGFloat((hex >> 8) & 0xff) / 255,
            blue: CGFloat(hex & 0xff) / 255, alpha: alpha)
}

func draw(size: Int) -> Data {
    let s = CGFloat(size) / 1024
    let ctx = CGContext(data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: 0,
                        space: CGColorSpace(name: CGColorSpace.sRGB)!,
                        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.scaleBy(x: s, y: s)

    // macOS icon grid: 824 pt tile with a soft drop shadow
    let tile = CGRect(x: 100, y: 100, width: 824, height: 824)
    let shape = CGPath(roundedRect: tile, cornerWidth: 185, cornerHeight: 185, transform: nil)
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -12), blur: 28, color: rgb(0x000000, 0.35))
    ctx.addPath(shape)
    ctx.setFillColor(rgb(0x1c1c20))
    ctx.fillPath()
    ctx.restoreGState()

    ctx.saveGState()
    ctx.addPath(shape)
    ctx.clip()
    let gradient = CGGradient(colorsSpace: nil, colors: [rgb(0x34343c), rgb(0x18181c)] as CFArray, locations: [0, 1])!
    ctx.drawLinearGradient(gradient, start: CGPoint(x: 512, y: 924), end: CGPoint(x: 512, y: 100), options: [])

    // the captured area: a warm gradient card, slightly inset
    let shot = CGRect(x: 262, y: 300, width: 500, height: 400)
    let shotPath = CGPath(roundedRect: shot, cornerWidth: 34, cornerHeight: 34, transform: nil)
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -10), blur: 30, color: rgb(0x000000, 0.5))
    ctx.addPath(shotPath)
    ctx.setFillColor(rgb(0xff6b4a))
    ctx.fillPath()
    ctx.restoreGState()
    ctx.saveGState()
    ctx.addPath(shotPath)
    ctx.clip()
    let warm = CGGradient(colorsSpace: nil, colors: [rgb(0xffb347), rgb(0xff4f5e)] as CFArray, locations: [0, 1])!
    ctx.drawLinearGradient(warm, start: CGPoint(x: 262, y: 700), end: CGPoint(x: 762, y: 300), options: [])
    // a little arrow annotation on the shot
    ctx.setStrokeColor(rgb(0xffffff))
    ctx.setLineWidth(30)
    ctx.setLineCap(.round)
    ctx.move(to: CGPoint(x: 360, y: 400)); ctx.addLine(to: CGPoint(x: 600, y: 590))
    ctx.strokePath()
    ctx.setFillColor(rgb(0xffffff))
    ctx.move(to: CGPoint(x: 660, y: 640)); ctx.addLine(to: CGPoint(x: 540, y: 620)); ctx.addLine(to: CGPoint(x: 640, y: 520))
    ctx.closePath(); ctx.fillPath()
    ctx.restoreGState()

    // selection corners around it
    ctx.setStrokeColor(rgb(0xffffff))
    ctx.setLineWidth(26)
    ctx.setLineCap(.round)
    ctx.setLineJoin(.round)
    let (l, b, r, t, arm): (CGFloat, CGFloat, CGFloat, CGFloat, CGFloat) = (212, 250, 812, 750, 110)
    for (x, y, dx, dy) in [(l, t, 1.0, -1.0), (r, t, -1.0, -1.0), (l, b, 1.0, 1.0), (r, b, -1.0, 1.0)] {
        ctx.move(to: CGPoint(x: x, y: y + dy * arm)); ctx.addLine(to: CGPoint(x: x, y: y)); ctx.addLine(to: CGPoint(x: x + dx * arm, y: y))
        ctx.strokePath()
    }
    ctx.restoreGState()

    let image = ctx.makeImage()!
    return NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:])!
}

let iconset = FileManager.default.temporaryDirectory.appendingPathComponent("Shotty.iconset")
try? FileManager.default.removeItem(at: iconset)
try FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
for base in [16, 32, 128, 256, 512] {
    try draw(size: base).write(to: iconset.appendingPathComponent("icon_\(base)x\(base).png"))
    try draw(size: base * 2).write(to: iconset.appendingPathComponent("icon_\(base)x\(base)@2x.png"))
}
let assets = root.appendingPathComponent("crates/shotty-app/assets")
try FileManager.default.createDirectory(at: assets, withIntermediateDirectories: true)
let out = assets.appendingPathComponent("Shotty.icns")
let p = Process()
p.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
p.arguments = ["-c", "icns", iconset.path, "-o", out.path]
try p.run(); p.waitUntilExit()
try draw(size: 512).write(to: root.appendingPathComponent("docs/icon.png"))
print("wrote \(out.path)")
