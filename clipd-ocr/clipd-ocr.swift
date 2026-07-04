// clipd-ocr — on-device OCR for clipd image clips using Apple's Vision
// framework. Runs entirely on-device (no network), like the rest of clipd.
//
// Usage:  clipd-ocr <path-to-image>
// Output: recognized text on stdout (one line per recognized block). Prints
//         nothing and exits 0 when no text is found; exits non-zero on error.
//
// Build:  swiftc -O -o clipd-ocr clipd-ocr.swift -framework Vision -framework AppKit

import AppKit
import Foundation
import Vision

let args = CommandLine.arguments
guard args.count >= 2 else {
    FileHandle.standardError.write("usage: clipd-ocr <image-path>\n".data(using: .utf8)!)
    exit(2)
}

let path = args[1]
guard let image = NSImage(contentsOfFile: path),
    let tiff = image.tiffRepresentation,
    let bitmap = NSBitmapImageRep(data: tiff),
    let cgImage = bitmap.cgImage
else {
    FileHandle.standardError.write("clipd-ocr: could not load image\n".data(using: .utf8)!)
    exit(1)
}

let request = VNRecognizeTextRequest()
request.recognitionLevel = .accurate
request.usesLanguageCorrection = true

let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
do {
    try handler.perform([request])
} catch {
    FileHandle.standardError.write("clipd-ocr: recognition failed: \(error)\n".data(using: .utf8)!)
    exit(1)
}

guard let observations = request.results else {
    exit(0)
}

var lines: [String] = []
for obs in observations {
    if let candidate = obs.topCandidates(1).first {
        lines.append(candidate.string)
    }
}

let text = lines.joined(separator: "\n")
if !text.isEmpty {
    print(text)
}
exit(0)
