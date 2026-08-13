import Foundation

struct AppConfiguration: Sendable {
    static let defaultURL = URL(string: "http://127.0.0.1:8080/")!

    let homeURL: URL
    let startsFullscreen: Bool
    let hidesCursor: Bool
    let developerMode: Bool

    static func load(
        arguments: [String] = CommandLine.arguments,
        environment: [String: String] = ProcessInfo.processInfo.environment
    ) -> AppConfiguration {
        var url = webURL(environment["GEOFRAME_URL"]) ?? defaultURL
        var fullscreen = boolean(environment["GEOFRAME_FULLSCREEN"], default: true)
        var hideCursor = boolean(environment["GEOFRAME_HIDE_CURSOR"], default: true)
        var developerMode = boolean(environment["GEOFRAME_DEVELOPER"], default: false)

        var index = 1
        while index < arguments.count {
            switch arguments[index] {
            case "--url" where index + 1 < arguments.count:
                if let candidate = webURL(arguments[index + 1]) {
                    url = candidate
                }
                index += 1
            case "--fullscreen":
                fullscreen = true
            case "--windowed":
                fullscreen = false
            case "--hide-cursor":
                hideCursor = true
            case "--show-cursor":
                hideCursor = false
            case "--developer":
                developerMode = true
            default:
                break
            }
            index += 1
        }

        return AppConfiguration(
            homeURL: url,
            startsFullscreen: fullscreen,
            hidesCursor: hideCursor,
            developerMode: developerMode
        )
    }

    private static func webURL(_ value: String?) -> URL? {
        guard
            let value,
            let url = URL(string: value),
            let scheme = url.scheme?.lowercased(),
            scheme == "http" || scheme == "https",
            url.host != nil
        else {
            return nil
        }
        return url
    }

    private static func boolean(_ value: String?, default defaultValue: Bool) -> Bool {
        guard let value else { return defaultValue }
        switch value.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() {
        case "1", "true", "yes", "on":
            return true
        case "0", "false", "no", "off":
            return false
        default:
            return defaultValue
        }
    }
}
