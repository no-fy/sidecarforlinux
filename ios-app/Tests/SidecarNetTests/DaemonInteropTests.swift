// Cross-implementation test: the real Rust daemon (`sidecard`, rustls +
// rcgen certificate) against the iPad app's PeerServer (Network.framework +
// swift-certificates identity) over loopback TLS. Runs in CI on macOS with
// SIDECARD_BIN pointing at a `cargo build --no-default-features` binary;
// skipped otherwise.
import Foundation
import SidecarProtocol
import XCTest

@testable import SidecarNet

final class DaemonInteropTests: XCTestCase {
    var ipadNS = DeviceIdentity.Namespace("interop-ipad-\(UUID().uuidString.prefix(8))", commonName: "Interop iPad")

    override func tearDown() {
        try? DeviceIdentity.deleteAll(ipadNS)
    }

    struct Run {
        let process: Process
        let output: Pipe
        func text() -> String {
            String(decoding: output.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
        }
    }

    func launch(_ bin: String, _ args: [String], configDir: URL, stdin: String) throws -> Run {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: bin)
        p.arguments = args
        var env = ProcessInfo.processInfo.environment
        env["SIDECAR_CONFIG_DIR"] = configDir.path
        env["XDG_RUNTIME_DIR"] = configDir.path
        env["NO_COLOR"] = "1"
        p.environment = env
        let out = Pipe()
        p.standardOutput = out
        p.standardError = out
        let input = Pipe()
        p.standardInput = input
        try p.run()
        input.fileHandleForWriting.write(Data(stdin.utf8))
        try? input.fileHandleForWriting.close()
        return Run(process: p, output: out)
    }

    func waitUntil(_ timeout: TimeInterval = 10, _ cond: @escaping @MainActor () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if await MainActor.run(body: cond) { return }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        XCTFail("condition not met within \(timeout) s")
    }

    func waitExit(_ p: Process, timeout: TimeInterval = 15) async throws {
        let deadline = Date().addingTimeInterval(timeout)
        while p.isRunning && Date() < deadline { try await Task.sleep(nanoseconds: 20_000_000) }
        if p.isRunning {
            p.terminate()
            XCTFail("daemon did not exit")
        }
    }

    func stroke() -> [PenBatch] {
        func s(_ seq: UInt32, _ phase: PenPhase, _ x: Float, _ p: Float) -> PenSample {
            PenSample(timestampNs: SessionClock.nowNs(), seq: seq, phase: phase, flags: SampleFlags.rollValid,
                      x: x, y: 0.5, pressure: p, altitude: 0.8, azimuth: 0.3, roll: 1.0, zOffset: 0,
                      estimationIndex: 0)
        }
        return [
            PenBatch(sendTimeNs: SessionClock.nowNs(), samples: [s(1, .hover, 0.2, 0)]),
            PenBatch(sendTimeNs: SessionClock.nowNs(), samples: [s(2, .down, 0.2, 0.4), s(3, .move, 0.3, 0.9)]),
            PenBatch(sendTimeNs: SessionClock.nowNs(), samples: [s(4, .up, 0.35, 0), s(5, .out, 0.35, 0)]),
        ]
    }

    func testRustDaemonPairsAndReceivesPenOverAppleTLS() async throws {
        guard let bin = ProcessInfo.processInfo.environment["SIDECARD_BIN"] else {
            throw XCTSkip("set SIDECARD_BIN to a sidecard binary to run the Rust<->Swift interop test")
        }
        let ipad = try DeviceIdentity.loadOrCreate(ipadNS)
        let trust = TrustStore(defaultsKey: "interop.trust.\(UUID().uuidString)")
        let port = UInt16.random(in: 42000...48000)
        let configDir = FileManager.default.temporaryDirectory.appendingPathComponent("sidecard-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: configDir, withIntermediateDirectories: true)

        // ---- 1. first contact: pairing, codes compared, pen samples injected
        var server = PeerServer(identity: ipad, trust: trust, usbPort: port)
        let delegate = await MainActor.run { RecordingDelegate() }
        await MainActor.run { server.delegate = delegate }
        server.start(wifi: false)
        try await waitUntil { !delegate.statuses.isEmpty }

        let run1 = try launch(bin, ["run", "--pair", "--once", "--dry-run", "--connect", "127.0.0.1:\(port)"],
                              configDir: configDir, stdin: "y\n")
        try await waitUntil(15) { delegate.isConnected }
        for b in stroke() { server.send(.penBatch(b)) }
        try await Task.sleep(nanoseconds: 700_000_000)
        server.stop()
        try await waitExit(run1.process)
        let out1 = run1.text()
        print("---- daemon output (pairing run) ----\n\(out1)")

        let ipadCode = try await MainActor.run { try XCTUnwrap(delegate.pairingCodes.first) }
        XCTAssertTrue(out1.contains("pairing code \(ipadCode)"),
                      "Rust and Swift must derive the same 6-digit code (iPad showed \(ipadCode))")
        XCTAssertTrue(out1.contains("paired with"))
        XCTAssertTrue(out1.contains("BTN_TOUCH 1"), "tip down injected")
        XCTAssertTrue(out1.contains("ABS_PRESSURE"), "pressure injected")
        XCTAssertTrue(out1.contains("ABS_TILT_X"), "tilt injected")
        XCTAssertTrue(out1.contains("ABS_Z"), "roll injected")
        XCTAssertTrue(out1.contains("BTN_TOOL_PEN 0"), "left proximity")
        XCTAssertEqual(trust.entries.count, 1, "iPad pinned the daemon")
        let trustedToml = try String(contentsOf: configDir.appendingPathComponent("trusted.toml"), encoding: .utf8)
        XCTAssertTrue(trustedToml.contains(Pairing.hex(ipad.fingerprint)), "daemon pinned the iPad")

        // ---- 2. reconnect: plain `run`, no pairing prompt
        server = PeerServer(identity: ipad, trust: trust, usbPort: port)
        let delegate2 = await MainActor.run { RecordingDelegate() }
        await MainActor.run { server.delegate = delegate2 }
        server.start(wifi: false)
        try await waitUntil { !delegate2.statuses.isEmpty }
        let run2 = try launch(bin, ["run", "--once", "--dry-run", "--connect", "127.0.0.1:\(port)"],
                              configDir: configDir, stdin: "")
        try await waitUntil(15) { delegate2.isConnected }
        for b in stroke() { server.send(.penBatch(b)) }
        try await Task.sleep(nanoseconds: 700_000_000)
        server.stop()
        try await waitExit(run2.process)
        let out2 = run2.text()
        print("---- daemon output (reconnect run) ----\n\(out2)")
        let repaired = await MainActor.run { delegate2.pairingCodes.isEmpty }
        XCTAssertTrue(repaired, "no pairing on reconnect")
        XCTAssertTrue(out2.contains("BTN_TOUCH 1"))
    }
}
