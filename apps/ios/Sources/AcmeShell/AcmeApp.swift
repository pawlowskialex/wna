// The shell's entry point. Call `AcmeApp.start()` once from `@main`; after
// that, `Storage`, `Rules` and `Features` are live. Nothing here touches FFI.

@_exported import AcmeBindings
@_exported import Wna

public enum AcmeApp {
    public static func start(
        secrets: some Secrets.Exports = KeychainSecrets(),
        notify: some Notify.Exports = UserNotifications(),
        config: some Config.Exports = AppConfig(),
        logging: some Logging.Exports = OSLogging()
    ) throws {
        try ShellWorld.compose(secrets: secrets, notify: notify, config: config, logging: logging)
    }

    /// Installs a downloaded feature package (bytes fetched by the app).
    /// Verification, compatibility checks and the swap happen in the host.
    @discardableResult
    public static func install(_ package: [UInt8], slot: String) throws -> String {
        try Features.install(slot: slot, package: package)
    }
}
