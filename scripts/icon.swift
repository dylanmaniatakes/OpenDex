import AppKit
import CoreGraphics
import Foundation

// Fixed 8-bit RGBA prevents the display's scale and color format from changing
// the PNG decoded by the native window runtime.
let pixels = Int(CommandLine.arguments[1])!
let context = CGContext(
    data: nil, width: pixels, height: pixels, bitsPerComponent: 8,
    bytesPerRow: pixels * 4, space: CGColorSpace(name: CGColorSpace.sRGB)!,
    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
)!
let scale = CGFloat(pixels) / 1024
context.scaleBy(x: scale, y: scale)

func box(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat, _ radius: CGFloat, _ rgb: (CGFloat, CGFloat, CGFloat)) {
    context.setFillColor(red: rgb.0, green: rgb.1, blue: rgb.2, alpha: 1)
    context.addPath(CGPath(roundedRect: CGRect(x: x, y: y, width: w, height: h), cornerWidth: radius, cornerHeight: radius, transform: nil))
    context.fillPath()
}

box(54, 54, 916, 916, 198, (0.089, 0.106, 0.120))
box(244, 529, 251, 251, 53, (0.757, 0.957, 0.514))
box(529, 529, 251, 251, 53, (0.205, 0.288, 0.188))
box(244, 244, 251, 251, 53, (0.162, 0.204, 0.175))
box(529, 244, 251, 251, 53, (0.521, 0.737, 0.379))

let bitmap = NSBitmapImageRep(cgImage: context.makeImage()!)
let data = bitmap.representation(using: .png, properties: [:])!
try data.write(to: URL(fileURLWithPath: CommandLine.arguments[2]))
