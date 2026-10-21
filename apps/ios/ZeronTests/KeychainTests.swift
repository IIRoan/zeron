import XCTest
@testable import Zeron

final class KeychainTests: XCTestCase {
    func testCredentialStorageUsesTheRealKeychain() {
        let service = "zeron.test.\(UUID().uuidString)"
        let account = "pairing"
        let data = Data("test credential".utf8)
        defer { Keychain.delete(service: service, account: account) }
        XCTAssertTrue(Keychain.save(data, service: service, account: account))
        XCTAssertEqual(Keychain.load(service: service, account: account), data)
    }
}
