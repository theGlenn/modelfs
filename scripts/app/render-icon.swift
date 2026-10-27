#!/usr/bin/env swift
// Original vector artwork for ModelFS. Regenerate with scripts/generate-app-icon.sh.
import AppKit
import Foundation

let output = URL(fileURLWithPath: CommandLine.arguments.dropFirst().first ?? ".context/ModelFS.iconset")
try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)

func color(_ red: CGFloat, _ green: CGFloat, _ blue: CGFloat) -> NSColor {
    NSColor(srgbRed: red, green: green, blue: blue, alpha: 1)
}

func layer(centerY: CGFloat, fill: NSColor) {
    let path = NSBezierPath()
    path.move(to: NSPoint(x: 512, y: centerY + 150))
    path.line(to: NSPoint(x: 780, y: centerY))
    path.line(to: NSPoint(x: 512, y: centerY - 150))
    path.line(to: NSPoint(x: 244, y: centerY))
    path.close()
    path.lineJoinStyle = .round
    path.lineWidth = 28
    fill.setFill()
    path.fill()
    fill.setStroke()
    path.stroke()
}

func drawIcon(pixels: Int) throws -> Data {
    guard let bitmap = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels,
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true,
        isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
    ), let context = NSGraphicsContext(bitmapImageRep: bitmap) else {
        throw CocoaError(.coderInvalidValue)
    }
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = context
    context.cgContext.scaleBy(x: CGFloat(pixels) / 1024, y: CGFloat(pixels) / 1024)

    let tile = NSBezierPath(roundedRect: NSRect(x: 100, y: 100, width: 824, height: 824), xRadius: 184, yRadius: 184)
    NSGraphicsContext.saveGraphicsState()
    let shadow = NSShadow()
    shadow.shadowColor = NSColor.black.withAlphaComponent(0.24)
    shadow.shadowBlurRadius = 26
    shadow.shadowOffset = NSSize(width: 0, height: -12)
    shadow.set()
    color(0.10, 0.27, 0.67).setFill()
    tile.fill()
    NSGraphicsContext.restoreGraphicsState()
    NSGradient(starting: color(0.18, 0.43, 0.91), ending: color(0.08, 0.22, 0.61))?
        .draw(in: tile, angle: -90)

    // Separate layers converge on one shared stack, matching the menu bar mark.
    layer(centerY: 396, fill: color(0.43, 0.68, 1.0))
    layer(centerY: 506, fill: color(0.69, 0.83, 1.0))
    layer(centerY: 616, fill: .white)

    NSGraphicsContext.restoreGraphicsState()
    guard let png = bitmap.representation(using: .png, properties: [:]) else {
        throw CocoaError(.fileWriteUnknown)
    }
    return png
}

for points in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let suffix = scale == 2 ? "@2x" : ""
        let url = output.appending(path: "icon_\(points)x\(points)\(suffix).png")
        try drawIcon(pixels: points * scale).write(to: url, options: .atomic)
    }
}
