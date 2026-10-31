import Testing
@testable import DBLensKit

/// Real messages from libpq, MariaDB Connector/C, the Redis client, and the
/// MongoDB C driver, and the cause dbLens names for each.
@Test(arguments: [
    ("postgresql://u@localhost:59999/db", "connection to server at \"localhost\" (::1), port 59999 failed: Connection refused\n\tIs the server running?", "refused"),
    ("mysql://u:p@127.0.0.1:59998/db", "Can't connect to server on '127.0.0.1' (36)", "refused"),
    ("redis://127.0.0.1:59997", "Couldn't connect to 127.0.0.1:59997: Connection refused", "refused"),
    ("mongodb://u:p@127.0.0.1:59996/db", "No suitable servers found: [connection refused calling hello on '127.0.0.1:59996']", "refused"),
    ("postgresql://u@nohost.example:5432/db", "could not translate host name \"nohost.example\" to address: nodename nor servname provided", "wasn't found"),
    ("redis://nohost.example:6379", "Couldn't connect to nohost.example:6379: host not found", "wasn't found"),
    ("mongodb://u:p@nohost.example:27017/db", "No suitable servers found: [Failed to resolve 'nohost.example']", "\"nohost.example\" wasn't found"),
    ("mysql://u:p@10.255.255.1/db", "Can't connect to server on '10.255.255.1' (60)", "didn't answer in time"),
    ("rediss://127.0.0.1:6379", "Couldn't connect to 127.0.0.1:6379 within 10 seconds.", "redis:// instead of rediss://"),
    ("mysql://root:p@h/db", "Access denied for user 'root'@'1.2.3.4' (using password: YES)", "refused the login for \"root\""),
    ("redis://h", "NOAUTH Authentication required.", "has none"),
    ("postgresql://nobody@h/db", "FATAL:  role \"nobody\" does not exist", "\"nobody\" doesn't exist"),
    ("mysql://u:p@h/nope", "Unknown database 'nope'", "\"nope\" doesn't exist"),
    ("redis://h/99", "ERR DB index is out of range", "no database 99"),
    ("postgresql://u@h/db?sslmode=require", "server does not support SSL, but SSL was required", "TLS didn't work"),
])
func namesTheCause(url: String, raw: String, expected: String) {
    let message = ConnectionProblem.explain(DatabaseError(message: raw), url: url, viaSSH: false).message
    #expect(message.contains(expected), "\(message)")
    #expect(message.hasSuffix(ConnectionProblem.clean(raw)))
}

@Test func keepsOneCopyOfRepeatedMessages() {
    let raw = "connection to server at \"localhost\" (::1), port 1 failed: Connection refused\nconnection to server at \"localhost\" (127.0.0.1), port 1 failed: Connection refused"
    #expect(ConnectionProblem.clean(raw).split(separator: "\n").count == 1)
}
