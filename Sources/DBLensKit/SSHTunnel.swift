import Darwin
import Foundation

/// A local port forwarded through SSH to a database the Mac can't reach
/// directly, using the Mac's own /usr/bin/ssh: your keys, ssh-agent, and
/// ~/.ssh/config (a Host alias works as the server). The database host is
/// seen from the SSH server, so "localhost" there means the server itself.
///   ssh -N -L 127.0.0.1:<free port>:<db host>:<db port> user@server
public final class SSHTunnel: @unchecked Sendable {
    public struct Config: Sendable, Hashable {
        /// A host name, an IP, or a Host alias from ~/.ssh/config. May be user@host.
        public var host: String
        /// nil uses the port from ~/.ssh/config, or 22.
        public var port: Int?
        /// Empty uses the user from ~/.ssh/config, or your Mac user name.
        public var user: String
        /// A private key file. Empty uses ~/.ssh/config and ssh-agent.
        public var keyPath: String

        public init(host: String, port: Int? = nil, user: String = "", keyPath: String = "") {
            self.host = host
            self.port = port
            self.user = user
            self.keyPath = keyPath
        }

        /// "ana@bastion.example.com:2222" for the sidebar and the log.
        public var label: String {
            (user.isEmpty || host.contains("@") ? "" : "\(user)@") + host + (port.map { ":\($0)" } ?? "")
        }
    }

    public let localPort: Int
    public let config: Config
    private let process: Process
    private let errors = Pipe()
    private let lock = NSLock()
    private var closedOnPurpose = false
    /// Called once if the tunnel stops without close(): the network dropped, the server restarted.
    public var onUnexpectedExit: (@Sendable (String) -> Void)?

    /// Opens the tunnel and waits until the local port answers, or throws ssh's own message.
    public static func open(_ config: Config, to remoteHost: String, port remotePort: Int) async throws -> SSHTunnel {
        if let problem = problem(config, remoteHost: remoteHost) { throw PostgresError(message: problem) }
        let tunnel = try SSHTunnel(config: config, remoteHost: remoteHost, remotePort: remotePort)
        try await tunnel.waitUntilReady()
        return tunnel
    }

    /// The arguments for /usr/bin/ssh, public for tests.
    public static func arguments(_ config: Config, localPort: Int, remoteHost: String, remotePort: Int) -> [String] {
        let target = remoteHost.contains(":") ? "[\(remoteHost)]" : remoteHost
        var args = [
            "-N", "-T",
            "-L", "127.0.0.1:\(localPort):\(target):\(remotePort)",
            "-o", "ExitOnForwardFailure=yes",
            "-o", "ServerAliveInterval=15",
            "-o", "ServerAliveCountMax=3",
            "-o", "ConnectTimeout=10",
            // No prompts: there is no terminal. Keys come from the agent or the key file.
            "-o", "BatchMode=yes",
            // Trust a server the first time, refuse it if its key ever changes.
            "-o", "StrictHostKeyChecking=accept-new",
        ]
        if let port = config.port { args += ["-p", "\(port)"] }
        if !config.keyPath.isEmpty {
            args += ["-i", (config.keyPath as NSString).expandingTildeInPath, "-o", "IdentitiesOnly=yes"]
        }
        if !config.user.isEmpty, !config.host.contains("@") { args += ["-l", config.user] }
        // "--": nothing after it is read as an option, whatever the host says.
        args += ["--", config.host]
        return args
    }

    /// Why these values can't go to ssh, or nil. A host like "-oProxyCommand=…"
    /// would run a command, and shell characters reach a ProxyCommand's %h.
    public static func problem(_ config: Config, remoteHost: String) -> String? {
        let hostChars = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-_@:[]%")
        let userChars = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-_")
        func valid(_ value: String, _ allowed: CharacterSet) -> Bool {
            !value.hasPrefix("-") && value.unicodeScalars.allSatisfy(allowed.contains)
        }
        if config.host.isEmpty || !valid(config.host, hostChars) {
            return String(localized: "The SSH server can only have letters, digits, and . - _ @ : [ ] (and can't start with -).")
        }
        if !config.user.isEmpty, !valid(config.user, userChars) {
            return String(localized: "The SSH user can only have letters, digits, and . - _ (and can't start with -).")
        }
        if config.keyPath.hasPrefix("-") || config.keyPath.contains(where: \.isNewline) {
            return String(localized: "The SSH key path is not valid.")
        }
        if remoteHost.isEmpty || !valid(remoteHost, hostChars) {
            return String(localized: "The database host is not valid for an SSH tunnel.")
        }
        return nil
    }

    private init(config: Config, remoteHost: String, remotePort: Int) throws {
        self.config = config
        localPort = try Self.freePort()
        process = Process()
        // ssh runs under a small shell that stops it when the app goes away,
        // even after a crash or kill -9, so no tunnel is left holding a port.
        process.executableURL = URL(fileURLWithPath: "/bin/sh")
        process.arguments = ["-c", Self.watchdog, "dblens-ssh"]
            + Self.arguments(config, localPort: localPort, remoteHost: remoteHost, remotePort: remotePort)
        process.standardError = errors
        process.standardOutput = FileHandle.nullDevice
        process.standardInput = FileHandle.nullDevice
        process.terminationHandler = { [weak self] _ in self?.exited() }
        do {
            try process.run()
        } catch {
            throw PostgresError(message: String(localized: "Couldn't start ssh: \(error.localizedDescription)"))
        }
        Self.registry.add(self)
    }

    /// Runs ssh with the given arguments; ends it when the app (the parent) is
    /// gone or this shell is told to stop, and exits with ssh's status.
    private static let watchdog = """
        /usr/bin/ssh "$@" & child=$!
        trap 'kill $child 2>/dev/null; exit 143' TERM INT HUP
        while kill -0 $child 2>/dev/null; do
          kill -0 $PPID 2>/dev/null || { kill $child 2>/dev/null; break; }
          sleep 1 & wait $!   # a wait, not a plain sleep, so a signal stops it at once
        done
        wait $child
        """

    deinit { close() }

    /// Stops the tunnel. Safe to call more than once.
    public func close() {
        lock.lock()
        closedOnPurpose = true
        lock.unlock()
        if process.isRunning { process.terminate() }
        Self.registry.remove(self)
    }

    public var isRunning: Bool { process.isRunning }
    /// The process running ssh (its watchdog shell); SIGTERM to it drops the tunnel like a network failure.
    public var processIdentifier: Int32 { process.processIdentifier }

    /// Every open tunnel, so the app can close them all when it quits.
    public static func closeAll() { registry.closeAll() }

    private func exited() {
        lock.lock()
        let onPurpose = closedOnPurpose
        lock.unlock()
        Self.registry.remove(self)
        guard !onPurpose else { return }
        onUnexpectedExit?(errorText())
    }

    private func errorText() -> String {
        let data = (try? errors.fileHandleForReading.readToEnd()) ?? Data()
        let text = String(decoding: data, as: UTF8.self)
            .split(separator: "\n")
            .filter { !$0.hasPrefix("Warning: Permanently added") }
            .joined(separator: "\n")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return text.isEmpty ? "O ssh terminou sem mensagem." : text
    }

    /// Polls the local port; ssh opens it only after logging in and setting up the forward.
    private func waitUntilReady() async throws {
        let deadline = ContinuousClock.now + .seconds(20)
        while ContinuousClock.now < deadline {
            if !process.isRunning {
                process.waitUntilExit()
                throw PostgresError(message: "SSH: \(errorText())")
            }
            if Self.answers(port: localPort) {
                guard listenerIsOurs() else {
                    close()
                    throw PostgresError(message: String(localized: "SSH: another program took the tunnel's local port. Try again."))
                }
                return
            }
            try await Task.sleep(for: .milliseconds(100))
        }
        close()
        throw PostgresError(message: String(localized: "SSH: the tunnel did not respond within 20 seconds."))
    }

    /// True when the process listening on the local port is the ssh under our watchdog.
    private func listenerIsOurs() -> Bool {
        let listeners = Set(Self.run("/usr/sbin/lsof", ["-nP", "-iTCP:\(localPort)", "-sTCP:LISTEN", "-t"]))
        let children = Set(Self.run("/usr/bin/pgrep", ["-P", "\(process.processIdentifier)"]))
        return !listeners.isDisjoint(with: children)
    }

    /// A command's output lines.
    private static func run(_ path: String, _ arguments: [String]) -> [String] {
        let task = Process()
        task.executableURL = URL(fileURLWithPath: path)
        task.arguments = arguments
        let out = Pipe()
        task.standardOutput = out
        task.standardError = FileHandle.nullDevice
        guard (try? task.run()) != nil else { return [] }
        let data = (try? out.fileHandleForReading.readToEnd()) ?? Data()
        task.waitUntilExit()
        return String(decoding: data, as: UTF8.self).split(separator: "\n").map(String.init)
    }

    private static func answers(port: Int) -> Bool {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { return false }
        defer { Darwin.close(fd) }
        var address = loopback(port: port)
        return withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) == 0
            }
        }
    }

    /// Asks the system for an unused port by binding to port 0.
    private static func freePort() throws -> Int {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { throw PostgresError(message: String(localized: "No free local port for the tunnel.")) }
        defer { Darwin.close(fd) }
        var address = loopback(port: 0)
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
        }
        var length = socklen_t(MemoryLayout<sockaddr_in>.size)
        let named = withUnsafeMutablePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(fd, $0, &length) }
        }
        guard bound == 0, named == 0 else { throw PostgresError(message: String(localized: "No free local port for the tunnel.")) }
        return Int(UInt16(bigEndian: address.sin_port))
    }

    private static func loopback(port: Int) -> sockaddr_in {
        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = in_port_t(UInt16(port).bigEndian)
        address.sin_addr = in_addr(s_addr: inet_addr("127.0.0.1"))
        return address
    }

    private static let registry = Registry()

    private final class Registry: @unchecked Sendable {
        private let lock = NSLock()
        private var tunnels: [ObjectIdentifier: SSHTunnel] = [:]
        func add(_ tunnel: SSHTunnel) { lock.withLock { tunnels[ObjectIdentifier(tunnel)] = tunnel } }
        func remove(_ tunnel: SSHTunnel) { lock.withLock { _ = tunnels.removeValue(forKey: ObjectIdentifier(tunnel)) } }
        func closeAll() { lock.withLock { Array(tunnels.values) }.forEach { $0.close() } }
    }
}
