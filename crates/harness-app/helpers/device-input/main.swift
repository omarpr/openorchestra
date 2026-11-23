// oo-device-input: touch, keys and the Home button for one booted iOS simulator, through
// CoreDevice's remote HID, for openOrchestra's Device tab. One long-lived process per watched
// device: sends are queued and asynchronous, so a process that exits straight after its last
// send loses it.
//
// Why a helper, the calls and the stub module it builds against: "Input spike findings
// (CoreDevice)" in docs/superpowers/specs/2026-11-23-device-tab-controls-design.md.
//
// usage: oo-device-input <udid>
// stdin, one command per line:
//   down x y | move x y | up x y   a finger; x and y are 0...1 ratios of the portrait screen,
//                                  top-left origin
//   key <usage> down|up            a USB HID keyboard usage (page 7)
//   home                           the Home button, pressed and released
//   quit                           (or end of input)
// stdout: "ready" once the device and its HID are found, then one line per command,
//         "ok <command>" or "err <command>: <why>".
// stderr: only a fatal problem, then a non-zero exit: 1 the device isn't found, 2 bad usage,
//         3 timed out, 4 its HID couldn't be reached.
import CoreDevice
import Foundation

func fail(_ code: Int32, _ why: String) -> Never {
  FileHandle.standardError.write(("oo-device-input: " + why + "\n").data(using: .utf8)!)
  exit(code)
}

struct CommandError: Error, CustomStringConvertible {
  let description: String
}

/// One line, whatever the error says.
func oneLine(_ error: Error) -> String {
  String(describing: error).split(whereSeparator: \.isNewline).joined(separator: " ")
}

struct Device {
  let digitizer: any HIDDigitizer
  let keyboard: any HIDKeyboard
  let button: any HIDButton

  func run(_ words: [String]) throws {
    func ratio(_ i: Int) throws -> Double {
      guard i < words.count, let v = Double(words[i]), v.isFinite else {
        throw CommandError(description: "expected \(words[0]) x y")
      }
      return min(max(v, 0), 1)
    }
    func touch(_ phase: DigitizerEventType) throws {
      let point = DigitizerPoint(x: try ratio(1), y: try ratio(2))
      try digitizer.send(pointOne: point, pointTwo: nil, eventType: phase, edge: .none, target: .mainScreen)
    }
    switch words[0] {
    case "down": try touch(.start)
    case "move": try touch(.position)
    case "up": try touch(.end)
    case "key":
      guard words.count == 3, let usage = UInt16(words[1]), ["down", "up"].contains(words[2]) else {
        throw CommandError(description: "expected key <usage> down|up")
      }
      try keyboard.send(key: HIDKeyboardUsageCode(rawValue: usage), state: words[2] == "down" ? .down : .up)
    case "home":
      // Consumer page, Menu.
      try button.sendCustomButton(usagePage: 0x0C, usageCode: 0x40, state: .down)
      try button.sendCustomButton(usagePage: 0x0C, usageCode: 0x40, state: .up)
    default:
      throw CommandError(description: "unknown command")
    }
  }
}

@main struct Main {
  static func main() async {
    setvbuf(stdout, nil, _IOLBF, 0)
    let args = CommandLine.arguments
    guard args.count == 2, let uuid = UUID(uuidString: args[1]) else {
      fail(2, "usage: oo-device-input <udid>")
    }
    // CoreDeviceService can hang; don't sit there forever.
    let watchdog = DispatchWorkItem { fail(3, "timed out reaching the simulator") }
    DispatchQueue.global().asyncAfter(deadline: .now() + 15, execute: watchdog)

    // Without the visibility set, simulators are left out of the list.
    let manager = DeviceManager(
      serviceConnection: CoreDeviceService.sharedConnection,
      allowedDeviceVisibilityClasses: [.simulators])
    await manager.awaitFullInitialization()
    // The list holds other simulators and physical devices too: only ever this one.
    guard let remote = manager.devices[uuid] else { fail(1, "no simulator \(uuid)") }
    let device: Device
    do {
      device = Device(
        digitizer: try await remote.getImplementation(for: CapabilityStaticMember<HIDDeviceCapability>.digitizer),
        keyboard: try await remote.getImplementation(for: CapabilityStaticMember<HIDDeviceCapability>.keyboard),
        button: try await remote.getImplementation(for: CapabilityStaticMember<HIDDeviceCapability>.button))
    } catch {
      fail(4, "the simulator's HID: \(oneLine(error))")
    }
    watchdog.cancel()
    print("ready")

    while let line = readLine() {
      let words = line.split(separator: " ").map(String.init)
      guard let command = words.first else { continue }
      if command == "quit" { break }
      do {
        try device.run(words)
        print("ok \(command)")
      } catch {
        print("err \(command): \(oneLine(error))")
      }
    }
    // Let the last sends leave before the process (and its connection) goes.
    try? await Task.sleep(nanoseconds: 300_000_000)
  }
}
