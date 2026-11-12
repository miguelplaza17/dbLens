// Test helper for src/import_swift.rs: writes connections.key and connections.enc
// into a folder exactly as the dbLens for Mac does, by compiling the Swift app's
// own SecretBox.swift (copied here from the retired Swift app) with this file. Throwaway folders only.
//
//   swiftc SecretBox.swift main.swift -o make-fixture
//   make-fixture <folder> <plain-json-file>
//
// Exit 0: done, with a Secure Enclave key. Exit 3: this Mac has no usable
// Secure Enclave (the box fell back to a software key; nothing is written).
import Foundation

let arguments = CommandLine.arguments
guard arguments.count == 3 else {
    FileHandle.standardError.write(Data("usage: make-fixture <folder> <plain-json-file>\n".utf8))
    exit(2)
}
let folder = URL(fileURLWithPath: arguments[1], isDirectory: true)
let plain = try Data(contentsOf: URL(fileURLWithPath: arguments[2]))
let keyFile = folder.appendingPathComponent("connections.key")
let box = try SecretBox(keyFile: keyFile)
guard box.hardwareBound else {
    try? FileManager.default.removeItem(at: keyFile)
    exit(3)
}
try writePrivately(try box.seal(plain), to: folder.appendingPathComponent("connections.enc"))
